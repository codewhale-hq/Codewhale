

#[test]
fn model_catalog_exposes_work_update_as_sole_progress_surface() {
    // #4132: ordinary progress is one model-visible and executable tool (todo_write).
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let registry = engine
        .build_turn_tool_registry_builder(
            AppMode::Agent,
            engine.config.todos.clone(),
            engine.config.plan_state.clone(),
        )
        .build(engine.build_tool_context(AppMode::Agent, false));
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog(
        registry.to_api_tools_with_cache(true),
        vec![],
        AppMode::Agent,
        &always_load,
    );
    let active = initial_active_tools(&catalog);
    let catalog_names: HashSet<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();

    assert!(
        catalog_names.contains("todo_write"),
        "todo_write must be model-visible"
    );
    assert!(
        active.contains("todo_write"),
        "todo_write must be available without a discovery turn"
    );
    assert!(
        !catalog_names.contains("update_plan"),
        "retired Strategy/Plan must stay replay-only"
    );
    // Actually registered hidden aliases (work_update family + checklist_write/update)
    // remain callable via registry but hidden from catalog. Others were never
    // registered and must stay not callable.
    for retired in [
        "work_update",
        "TodoWrite",
        "todo",
        "checklist_write",
        "checklist_update",
    ] {
        assert!(
            registry.contains(retired),
            "{retired} hidden alias must remain callable"
        );
        assert!(
            !catalog_names.contains(retired),
            "{retired} must not appear in the model catalog"
        );
    }
    for retired in [
        "checklist_add",
        "checklist_list",
        "todo_add",
        "todo_update",
        "todo_list",
    ] {
        assert!(
            !registry.contains(retired),
            "{retired} must not be callable"
        );
        assert!(
            !catalog_names.contains(retired),
            "{retired} must not appear in the model catalog"
        );
    }
    for retired in [
        "checklist_write",
        "checklist_add",
        "checklist_update",
        "checklist_list",
        "work_update",
        "TodoWrite",
        "todo",
        "todo_add",
        "todo_update",
        "todo_list",
    ] {
        assert!(
            preflight_requested_deferred_tool(
                retired,
                &json!({
                    "todos": [
                        { "content": "should not hydrate hidden alias", "status": "completed" }
                    ]
                }),
                &catalog,
                &mut active.clone(),
            )
            .is_none(),
            "{retired} must not have a deferred catalog preflight path"
        );
    }
}

#[test]
fn user_shell_turn_outcome_distinguishes_cancel_failure_and_success() {
    let cancelled = Ok(
        ToolResult::error("Command canceled; process killed.").with_metadata(json!({
            "status": "Killed",
            "canceled": true,
        })),
    );
    assert_eq!(
        user_shell_turn_outcome(&cancelled, false),
        TurnOutcomeStatus::Interrupted
    );

    let cancelled_while_awaiting_approval = Err(ToolError::execution_failed(
        "Request cancelled while awaiting approval",
    ));
    assert_eq!(
        user_shell_turn_outcome(&cancelled_while_awaiting_approval, true),
        TurnOutcomeStatus::Interrupted
    );

    let failed = Ok(ToolResult::error("Command failed (exit code: 1)"));
    assert_eq!(
        user_shell_turn_outcome(&failed, false),
        TurnOutcomeStatus::Failed
    );

    let execution_error = Err(ToolError::execution_failed("shell manager unavailable"));
    assert_eq!(
        user_shell_turn_outcome(&execution_error, false),
        TurnOutcomeStatus::Failed
    );

    let completed = Ok(ToolResult::success("done"));
    assert_eq!(
        user_shell_turn_outcome(&completed, true),
        TurnOutcomeStatus::Interrupted
    );
    assert_eq!(
        user_shell_turn_outcome(&completed, false),
        TurnOutcomeStatus::Completed
    );
}

/// #5191: a user-typed `!` command is pre-approved by provenance — typing it
/// IS the approval. It must run without the tool-approval modal even in an
/// Ask/Suggest session, and the audit trail must record the user provenance.
#[tokio::test]
async fn run_shell_command_op_executes_without_approval_modal() {
    let _guard = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let audit_path = tmp.path().join("tool-audit.jsonl");
    let _audit = EnvVarGuard::set("CODEWHALE_TOOL_AUDIT_LOG", &audit_path);
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &Config::default());
    engine.session.allow_shell = false;
    engine.config.allow_shell = false;

    engine
        .handle_run_shell_command(
            "echo bang-ok".to_string(),
            AppMode::Agent,
            true,
            false,
            false,
            ApprovalMode::Suggest,
        )
        .await;

    let mut saw_started = false;
    let mut saw_approval = false;
    let mut saw_complete = false;
    let mut saw_turn_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TurnStarted { turn_id, route, .. } => {
                assert!(turn_id.starts_with(USER_SHELL_TOOL_ID_PREFIX));
                assert!(route.is_none());
            }
            Event::ToolCallStarted {
                id,
                name,
                input,
                model_call,
            } => {
                saw_started = true;
                assert!(model_call.is_none());
                assert!(id.starts_with(USER_SHELL_TOOL_ID_PREFIX));
                assert_eq!(name, "Bash");
                assert_eq!(input["action"], json!("run"));
                assert_eq!(input["command"], json!("echo bang-ok"));
                assert_eq!(input["source"], json!("user"));
            }
            Event::ApprovalRequired { .. } => {
                saw_approval = true;
            }
            Event::ToolCallComplete {
                id,
                name,
                result,
                model_call,
            } => {
                saw_complete = true;
                assert!(model_call.is_none());
                assert!(id.starts_with(USER_SHELL_TOOL_ID_PREFIX));
                assert_eq!(name, "Bash");
                let result = result.expect("shell result");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("bang-ok"), "{result:?}");
            }
            Event::TurnComplete { status, .. } => {
                saw_turn_complete = true;
                assert_eq!(status, TurnOutcomeStatus::Completed);
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert!(saw_started);
    assert!(
        !saw_approval,
        "user-typed bang commands must not raise the approval modal (#5191)"
    );
    assert!(saw_complete);
    assert!(saw_turn_complete);

    let audit = std::fs::read_to_string(&audit_path).expect("audit log written");
    assert!(
        audit.contains("tool.user_provenance_preapproved"),
        "audit trail must record the user-provenance pre-approval: {audit}"
    );
    assert!(
        audit.contains("composer_bang"),
        "audit row must name the composer-bang source: {audit}"
    );
}

#[tokio::test]
async fn run_shell_command_op_skips_approval_when_auto_approved() {
    let workspace = tempdir().expect("tempdir");
    let todos = crate::tools::todo::new_shared_todo_list();
    let plan = crate::tools::plan::new_shared_plan_state();
    let work = crate::work_graph::new_shared_work_runtime(todos, plan);
    let runtime_services = crate::tools::spec::RuntimeToolServices {
        work: Some(work.clone()),
        ..Default::default()
    };
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            runtime_services,
            ..EngineConfig::default()
        },
        &Config::default(),
    );
    let session_id = engine.session.id.clone();

    engine
        .handle_run_shell_command(
            "echo bang-yolo".to_string(),
            AppMode::Agent,
            true,
            true,
            true,
            ApprovalMode::Auto,
        )
        .await;

    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = rx.recv().await {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("auto-approved shell shortcut should not request approval");
            }
            Event::ToolCallComplete { result, .. } => {
                saw_complete = true;
                let result = result.expect("shell result");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("bang-yolo"), "{result:?}");
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                break;
            }
            _ => {}
        }
    }

    assert!(saw_complete);
    let graph = work
        .capture(Some(&session_id))
        .expect("capture bang-shell work")
        .expect("bang-shell graph")
        .graph;
    let operation = graph
        .nodes
        .iter()
        .find(|node| node.kind == crate::work_graph::NodeKind::Operation)
        .expect("bang-shell operation registered before execution");
    assert_eq!(operation.state, crate::work_graph::NodeState::Completed);
    let observation = operation
        .binding
        .as_ref()
        .and_then(|binding| binding.last_observation.as_ref())
        .expect("terminal shell owner observation");
    assert!(
        observation
            .output
            .as_ref()
            .and_then(crate::work_graph::EvidenceRef::raw_bytes)
            .is_some_and(|raw_bytes| raw_bytes > 0),
        "bang-shell completion must retain a logical byte-count receipt"
    );
}

#[tokio::test]
async fn run_shell_command_op_allows_readonly_shell_in_auto_mode() {
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &Config::default());
    let handle_for_approval = handle.clone();

    let task = tokio::spawn(async move {
        engine
            .handle_run_shell_command(
                "pwd".to_string(),
                AppMode::Agent,
                true,
                false,
                false,
                ApprovalMode::Auto,
            )
            .await;
    });

    let mut saw_approval = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = rx.recv().await {
        match event {
            Event::ApprovalRequired { id, .. } => {
                saw_approval = true;
                handle_for_approval
                    .approve_tool_call(id)
                    .await
                    .expect("approve unexpected shell prompt");
            }
            Event::ToolCallComplete { result, .. } => {
                saw_complete = true;
                let result = result.expect("shell result");
                assert!(result.success, "{result:?}");
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                break;
            }
            _ => {}
        }
    }
    drop(rx);
    task.await.expect("shell op task");

    assert!(
        !saw_approval,
        "read-only shell shortcut should not request approval in Auto mode"
    );
    assert!(saw_complete);
}

#[tokio::test]
async fn yolo_mode_does_not_prompt_for_typed_ask_rule() {
    // #3386: a command matching a typed ask-rule (permissions.toml) must not
    // surface an approval modal in the Full Access posture, even though the
    // stale ApprovalMode::Auto maps to OnFailure in the execpolicy (honors
    // ask-rules). The auto_review safety floor and typed deny rules still
    // apply; only the ask-rule Prompt is suppressed under Full Access.
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            exec_policy_engine: ask_rule_engine("echo"),
            ..EngineConfig::default()
        },
        &Config::default(),
    );

    engine
        .handle_run_shell_command(
            "echo yolo-ask-rule".to_string(),
            AppMode::Agent,
            true,
            true,
            true,
            ApprovalMode::Auto,
        )
        .await;

    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = rx.recv().await {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("YOLO mode must not prompt for a typed ask-rule");
            }
            Event::ToolCallComplete { result, .. } => {
                saw_complete = true;
                let result = result.expect("shell result");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("yolo-ask-rule"), "{result:?}");
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                break;
            }
            _ => {}
        }
    }

    assert!(saw_complete);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn operate_model_shell_uses_normal_approval_and_workspace_sandbox() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;

    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-operate-tools\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_operate_shell\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",",
        "\"arguments\":\"{\\\"action\\\":\\\"run\\\",\\\"command\\\":\\\"echo operate-approved > operate-mode-approved.txt\\\"}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-operate-tools\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-operate-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-operate-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    // The goal this fixture seals is seeded directly (prose no longer
    // creates goals since the #6290 rework); after the approved shell runs,
    // the model seals it through the same `update_goal` tool a live Operate
    // turn uses, and only then does the final "done" arrive.
    let goal_seal_marker = "goal-seal-receipt-0902";
    let goal_sse = concat!(
        "data: {\"id\":\"chatcmpl-operate-goal\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_operate_goal\",\"type\":\"function\",\"function\":{\"name\":\"update_goal\",",
        "\"arguments\":\"{\\\"status\\\":\\\"complete\\\",\\\"evidence\\\":\\\"goal-seal-receipt-0902: operate-mode-approved.txt written\\\",",
        "\\\"verification\\\":{\\\"status\\\":\\\"passed\\\",\\\"check\\\":\\\"cat operate-mode-approved.txt\\\",\\\"summary\\\":\\\"fixture contains operate-approved\\\"}}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-operate-goal\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains(goal_seal_marker))
        .and(body_string_contains("tokens_used"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    // Goal control must execute immediately: continuation cannot depend on
    // discovering or retrying the very tool that lets the model stop it.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("was deferred and has now been loaded"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(goal_sse),
        )
        .expect(0)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("operate-mode-approved.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(goal_sse),
        )
        .expect(1)
        .with_priority(3)
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
        .with_priority(3)
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
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal lock")
        .create("write the requested local fixture".to_string(), None)
        .expect("seed fixture goal");
    let handle_for_approval = handle.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "Write the requested local fixture to the workspace".to_string(),
            images: Vec::new(),
            mode: AppMode::Operate,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("write the requested local fixture".to_string()),
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
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
        .expect("send Operate model turn");

    let mut saw_approval = false;
    let mut saw_shell_result = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for Operate tool event")
    {
        match event {
            Event::ApprovalRequired { id, tool_name, .. } => {
                saw_approval = true;
                assert_eq!(tool_name, "Bash");
                // The seeded fixture goal is orthogonal to this gate:
                // Operate uses the normal approval flow either way.
                handle_for_approval
                    .approve_tool_call(id)
                    .await
                    .expect("approve Operate shell");
            }
            Event::ToolCallComplete { name, result, .. } if name == "Bash" => {
                saw_shell_result = true;
                let result = result.expect("approved Operate shell result");
                assert!(result.success, "{result:?}");
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
        saw_approval,
        "Operate should use the normal approval gate instead of a mode-only denial"
    );
    assert!(saw_shell_result);
    assert!(saw_complete);
    let requests = server.received_requests().await.expect("recorded requests");
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
    for name in ["create_goal", "get_goal", "update_goal"] {
        assert!(
            first["tools"]
                .as_array()
                .expect("wire tools")
                .iter()
                .any(|tool| tool["function"]["name"] == name),
            "first provider request must expose {name} without discovery"
        );
    }
    let written = std::fs::read_to_string(workspace.path().join("operate-mode-approved.txt"))
        .expect("workspace-scoped shell output");
    assert_eq!(written.trim_end(), "operate-approved");
}

/// Drives one model turn whose single `Bash` call needs approval, publishes
/// `change_to` (as a runtime PATCH does) while the approval is pending, then
/// approves. Returns the call's result and whether the file was written.
async fn posture_change_during_approval_wait(
    change_to: (AppMode, ApprovalMode, bool),
) -> (Result<crate::tools::spec::ToolResult, ToolError>, bool) {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-e2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_e2_shell\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",",
        "\"arguments\":\"{\\\"action\\\":\\\"run\\\",\\\"command\\\":\\\"echo approved > e2-approved.txt\\\"}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-e2\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-e2-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-e2-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("call_e2_shell"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
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
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());
    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "Record the approval fixture in the workspace".to_string(),
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
        .expect("send model turn");

    let (mode, approval_mode, auto_approve) = change_to;
    let mut shell_result = None;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for turn event")
    {
        match event {
            Event::ApprovalRequired { id, .. } => {
                // The PATCH lands while the approval card is open.
                handle
                    .try_send(Op::ChangeMode {
                        mode,
                        allow_shell: true,
                        trust_mode: false,
                        auto_approve,
                        approval_mode,
                        configured_sandbox_mode: None,
                    })
                    .expect("publish posture change");
                handle.approve_tool_call(id).await.expect("approve shell");
            }
            Event::ToolCallComplete { name, result, .. } if name == "Bash" => {
                shell_result = Some(result);
            }
            Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    drop(rx);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    let written = workspace.path().join("e2-approved.txt").exists();
    (shell_result.expect("the approved call completes"), written)
}

#[test]
fn live_runtime_authority_narrows_only_when_a_grant_is_withdrawn() {
    let at = |mode, approval_mode, sandbox: Option<&str>| {
        LiveRuntimeAuthority::from_fields(
            mode,
            true,
            false,
            approval_mode == ApprovalMode::Bypass,
            approval_mode,
            sandbox.map(str::to_string),
        )
    };
    let ask = at(AppMode::Agent, ApprovalMode::Suggest, None);
    assert!(!ask.narrows(&ask));
    assert!(!at(AppMode::Agent, ApprovalMode::Auto, None).narrows(&ask));
    assert!(!at(AppMode::Agent, ApprovalMode::Bypass, None).narrows(&ask));
    assert!(!ask.narrows(&at(AppMode::Plan, ApprovalMode::Suggest, None)));
    assert!(at(AppMode::Plan, ApprovalMode::Suggest, None).narrows(&ask));
    assert!(at(AppMode::Operate, ApprovalMode::Suggest, None).narrows(&ask));
    assert!(ask.narrows(&at(AppMode::Agent, ApprovalMode::Bypass, None)));
    assert!(at(AppMode::Agent, ApprovalMode::Never, None).narrows(&ask));
    assert!(at(AppMode::Agent, ApprovalMode::Suggest, Some("read-only")).narrows(&ask));
    assert!(
        !at(
            AppMode::Agent,
            ApprovalMode::Suggest,
            Some("workspace-write")
        )
        .narrows(&at(
            AppMode::Agent,
            ApprovalMode::Suggest,
            Some("read-only")
        ))
    );
    assert!(at(AppMode::Agent, ApprovalMode::Suggest, Some("custom")).narrows(&ask));
    let mut no_shell = ask.clone();
    no_shell.allow_shell = false;
    assert!(no_shell.narrows(&ask));
}

/// E2: approving a call must never invalidate the call it approves. A posture
/// PATCH that is equal or broader (Ask -> Auto-Review, Ask -> Full Access)
/// while the approval card is open leaves the approved call running.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn broader_posture_patch_during_approval_wait_keeps_the_approved_call() {
    let _lock = lock_test_env();
    for change_to in [
        (AppMode::Agent, ApprovalMode::Auto, false),
        (AppMode::Agent, ApprovalMode::Bypass, true),
        (AppMode::Agent, ApprovalMode::Suggest, false),
    ] {
        let (result, written) = posture_change_during_approval_wait(change_to).await;
        let result = result.unwrap_or_else(|err| panic!("{change_to:?}: {err}"));
        assert!(result.success, "{change_to:?}: {result:?}");
        assert!(written, "{change_to:?}: the approved shell ran");
    }
}

/// E2 counterpart: a narrowing PATCH (Work -> Plan, Ask -> Never) still sends
/// the approved call back to the model instead of running it under a grant
/// the user has since withdrawn.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn narrower_posture_patch_during_approval_wait_fails_the_call() {
    let _lock = lock_test_env();
    for change_to in [
        (AppMode::Plan, ApprovalMode::Suggest, false),
        (AppMode::Agent, ApprovalMode::Never, false),
    ] {
        let (result, written) = posture_change_during_approval_wait(change_to).await;
        let err = result.expect_err("narrowed posture fails the call");
        assert!(
            err.to_string()
                .contains("Permissions changed before this tool call executed"),
            "{change_to:?}: {err}"
        );
        assert!(!written, "{change_to:?}: the shell must not run");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn full_access_subagent_handoff_keeps_model_shell_free_of_approval_prompts() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;

    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-yolo\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_yolo\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",",
        "\"arguments\":\"{\\\"action\\\":\\\"run\\\",\\\"command\\\":\\\"echo yolo-model-ask-rule\\\"}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-yolo\",\"choices\":[{\"index\":0,\"delta\":{},",
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
        .and(body_string_contains("yolo-model-ask-rule"))
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
            exec_policy_engine: ask_rule_engine("echo"),
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "continue from the completed child".to_string(),
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
            // Exercise the valid legacy/host shape where the named posture is
            // authoritative but the redundant bit is stale.
            auto_approve: false,
            approval_mode: ApprovalMode::Bypass,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::SubAgentHandoff,
            submission_id: None,
        }))
        .await
        .expect("send model turn");

    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("Full Access child handoff must not prompt for an ordinary shell call");
            }
            Event::ToolCallComplete { name, result, .. } if name == "Bash" => {
                saw_complete = true;
                let result = result.expect("shell result");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("yolo-model-ask-rule"), "{result:?}");
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(saw_complete);
}

async fn assert_full_access_model_tool_batch_is_blocked(
    engine_config: EngineConfig,
    tool_calls: Vec<(&'static str, serde_json::Value)>,
    expected_errors: &[(&str, &str)],
    followup_fragment: &str,
) {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let model_tool_calls = tool_calls
        .iter()
        .enumerate()
        .map(|(index, (name, arguments))| {
            json!({
                "index": index,
                "id": format!("call_full_access_{index}"),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": arguments.to_string(),
                },
            })
        })
        .collect::<Vec<_>>();
    let tool_delta = json!({
        "id": "chatcmpl-full-access-blocked",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": model_tool_calls},
            "finish_reason": serde_json::Value::Null,
        }],
    });
    let tool_finish = json!({
        "id": "chatcmpl-full-access-blocked",
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
        .and(body_string_contains(followup_fragment))
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
    let (engine, handle) = Engine::new(engine_config, &api_config);
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "exercise the Full Access execution boundary".to_string(),
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
        .expect("send Full Access model turn");

    let expected = expected_errors.iter().copied().collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut saw_turn_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for Full Access boundary event")
    {
        match event {
            Event::ApprovalRequired { tool_name, .. } => {
                panic!("Full Access must not open an approval modal for blocked tool {tool_name}")
            }
            Event::ToolCallComplete { name, result, .. }
                if expected.contains_key(name.as_str()) =>
            {
                let error = result.expect_err("blocked tool must return an error");
                let fragment = expected[name.as_str()];
                assert!(
                    error.to_string().contains(fragment),
                    "unexpected {name} denial: {error:?}"
                );
                seen.insert(name);
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
    assert_eq!(seen.len(), expected.len(), "missing blocked tool results");
    assert!(saw_turn_complete);
}

async fn assert_full_access_model_tool_batch_runs(
    engine_config: EngineConfig,
    tool_calls: Vec<(&'static str, serde_json::Value)>,
    expected_names: &[&str],
) {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let model_tool_calls = tool_calls
        .iter()
        .enumerate()
        .map(|(index, (name, arguments))| {
            json!({
                "index": index,
                "id": format!("call_full_access_{index}"),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": arguments.to_string(),
                },
            })
        })
        .collect::<Vec<_>>();
    let tool_delta = json!({
        "id": "chatcmpl-full-access-blocked",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": model_tool_calls},
            "finish_reason": serde_json::Value::Null,
        }],
    });
    let tool_finish = json!({
        "id": "chatcmpl-full-access-blocked",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
    });
    let tool_call_sse = format!("data: {tool_delta}\n\ndata: {tool_finish}\n\ndata: [DONE]\n\n");
    // Request 1's "model" response: discover the deferred specialized tools
    // through tool_search, exactly as the lowercase contract expects before
    // the first direct call.
    let search_tool_calls = vec![
        json!({
            "index": 0,
            "id": "call_search_mcp",
            "type": "function",
            "function": {
                "name": "tool_search",
                "arguments": r#"{"query":"mcp server"}"#,
            },
        }),
        json!({
            "index": 1,
            "id": "call_search_rlm",
            "type": "function",
            "function": {
                "name": "tool_search",
                "arguments": r#"{"query":"rlm"}"#,
            },
        }),
    ];
    let search_delta = json!({
        "id": "chatcmpl-full-access-search",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": search_tool_calls},
            "finish_reason": serde_json::Value::Null,
        }],
    });
    let search_finish = json!({
        "id": "chatcmpl-full-access-search",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
    });
    let search_sse = format!("data: {search_delta}\n\ndata: {search_finish}\n\ndata: [DONE]\n\n");
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    // wiremock keeps every mounted mock matching even after its expected
    // count is reached, and request history accumulates across the three
    // steps, so substring matchers on the *calls* would re-fire forever.
    // Anchor each mock on the tool-result ids that exist in exactly one
    // request body: request 3 carries the executed batch's results, request 2
    // carries the search results, and request 1 carries neither.

    // Request 3 (exec batch results) terminates with the done turn.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains(
            "\"tool_call_id\":\"call_full_access_0\"",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    // Request 2 (search results) executes the deferred specialized tools that
    // request 1 discovered through tool_search. The search call activates
    // them in the session cache, so this request runs them under Full Access
    // without an approval modal.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("\"tool_call_id\":\"call_search_mcp\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;
    // Request 1: the model discovers the deferred specialized tools it needs.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(search_sse),
        )
        .expect(1)
        .with_priority(3)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(engine_config, &api_config);
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "exercise the Full Access auto-approval boundary".to_string(),
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
        .expect("send Full Access model turn");

    let expected = expected_names
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut seen = HashSet::new();
    let mut saw_turn_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for Full Access auto-approval event")
    {
        match event {
            Event::ApprovalRequired { tool_name, .. } => {
                panic!(
                    "Full Access must not open an approval modal for auto-approved tool {tool_name}"
                )
            }
            Event::ToolCallComplete { name, result, .. } if expected.contains(name.as_str()) => {
                if let Err(error) = &result {
                    let message = error.to_string();
                    assert!(
                        !message.contains("blocked in Full Access"),
                        "Full Access auto-approves non-bypassable tools: {message}"
                    );
                }
                seen.insert(name);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(
                    status,
                    TurnOutcomeStatus::Completed,
                    "Full Access turn must complete: {error:?}"
                );
                saw_turn_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert_eq!(
        seen.len(),
        expected.len(),
        "every tool must reach execution, seen: {seen:?}"
    );
    assert!(saw_turn_complete);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn full_access_auto_approves_non_bypassable_registered_tools() {
    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let marker = workspace.path().join("runtime-tool-must-run");
    let marker_literal = marker
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "\\'");
    // GitHub's Windows image exposes the interpreter as `python`; Unix
    // images expose `python3`. Keep the runtime-tool execution receipt
    // platform-neutral so this test measures the Full Access boundary rather
    // than an executable-name convention.
    let python = if cfg!(windows) { "python" } else { "python3" };
    let start_probe =
        format!("{python} -c \"__import__('pathlib').Path('{marker_literal}').write_text('ran')\"");
    let rlm_probe = format!("__import__('pathlib').Path('{marker_literal}').write_text('ran')");
    let engine_config = EngineConfig {
        model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
        workspace: workspace.path().to_path_buf(),
        mcp_config_path: workspace.path().join("mcp.json"),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..EngineConfig::default()
    };
    assert_full_access_model_tool_batch_runs(
        engine_config,
        vec![
            (
                "start_mcp_server",
                json!({"server": start_probe, "name": "auto-approved"}),
            ),
            (
                "rlm",
                json!({"action": "eval", "name": "missing-context", "code": rlm_probe}),
            ),
        ],
        &["start_mcp_server", "rlm"],
    )
    .await;

    assert!(
        marker.exists(),
        "Full Access auto-approves start_mcp_server, so its server command must actually run"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn full_access_permission_allow_cannot_bypass_repo_law() {
    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let law_dir = workspace.path().join(".codewhale");
    fs::create_dir_all(&law_dir).expect("create law directory");
    fs::write(
        law_dir.join("constitution.json"),
        r#"{
            "protected_invariants": [{
                "text": "Release notes need human review",
                "paths": ["CHANGELOG.md"]
            }]
        }"#,
    )
    .expect("write repo law fixture");
    let target = workspace.path().join("CHANGELOG.md");
    let allow_rule = codewhale_execpolicy::ToolAskRule::file_path("write_file", "CHANGELOG.md")
        .into_exact_workspace_allow(workspace.path().to_string_lossy().into_owned());
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
    let tool_input =
        json!({"action": "write", "filePath": "CHANGELOG.md", "content": "must not be written\n"});
    assert_eq!(
        file_tool_ask_rule_decision(
            &engine_config,
            "write_file",
            &tool_input,
            workspace.path(),
            ApprovalMode::Bypass,
        ),
        Some(ToolAskRuleDecision::Allow),
        "precondition: the remembered grant must match before repo law tightens the plan"
    );

    assert_full_access_model_tool_batch_is_blocked(
        engine_config,
        vec![("File", tool_input)],
        &[(
            "File",
            "Repository law blocked tool 'File' in Full Access: Repo law holds this write: \"Release notes need human review\"",
        )],
        "Repository law blocked tool 'File' in Full Access: Repo law holds this write:",
    )
    .await;

    assert!(!target.exists(), "repo-law block must prevent the write");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn full_access_file_rules_block_alias_and_parent_paths_without_mutation() {
    use codewhale_execpolicy::{ExecPolicyEngine, PermissionAction, Ruleset, ToolAskRule};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    fs::create_dir(workspace.path().join("sub")).expect("fixture directory");
    let target = workspace.path().join("protected.txt");
    fs::write(&target, "original\n").expect("fixture contents");
    let rules = ["write_file", "edit_file", "apply_patch"]
        .into_iter()
        .map(|tool| {
            let mut rule = ToolAskRule::file_path(tool, "protected.txt");
            rule.action = PermissionAction::Deny;
            rule
        })
        .collect();
    let engine_config = EngineConfig {
        model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
        workspace: workspace.path().to_path_buf(),
        snapshots_enabled: false,
        subagents_enabled: false,
        exec_policy_engine: ExecPolicyEngine::with_rulesets(vec![
            Ruleset::user(vec![], vec![]).with_ask_rules(rules),
        ]),
        ..EngineConfig::default()
    };
    assert_full_access_model_tool_batch_is_blocked(
        engine_config,
        vec![
            ("write_file", json!({"filePath": "protected.txt", "content": "changed\n"})),
            ("File", json!({"action": "edit", "file_path": "protected.txt", "search": "original", "replace": "changed"})),
            ("apply_patch", json!({"filePath": "protected.txt", "patch": "@@ -1 +1 @@\n-original\n+changed\n"})),
            ("write", json!({"path": "sub/../protected.txt", "content": "changed\n"})),
        ],
        &[
            ("write_file", "Permission rule 'tool=write_file path=protected.txt' explicitly denies"),
            ("File", "Permission rule 'tool=edit_file path=protected.txt' explicitly denies"),
            ("apply_patch", "Permission rule 'tool=apply_patch path=protected.txt' explicitly denies"),
            ("write", "Permission rule 'tool=write_file path=protected.txt' explicitly denies"),
        ],
        "explicitly denies this invocation",
    ).await;
    assert_eq!(
        fs::read_to_string(&target).expect("retained contents"),
        "original\n"
    );
}
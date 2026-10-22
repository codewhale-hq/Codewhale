#[test]
fn agent_mode_elevates_writes_without_granting_network() {
    // #273 elevated Agent mode's sandbox so `curl`, package managers, and
    // similar shell commands worked, and justified it by saying the
    // application-level NetworkPolicy would remain "the only outbound
    // boundary". That premise did not hold: NetworkPolicy governs
    // fetch_url/web_search/MCP HTTP and never constrained shell subprocesses,
    // so workspace-write turns had unrestricted egress with no boundary at
    // all. Writing to the workspace is now decoupled from reaching the
    // network: the write elevation stays, the network grant does not.
    // Network comes from `sandbox_network_access`, from a danger-full-access
    // posture, or from the post-denial elevation prompt.
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());

    let agent_ctx = engine.build_tool_context(AppMode::Agent, false);
    let agent_policy = agent_ctx
        .elevated_sandbox_policy
        .as_ref()
        .expect("Agent mode should elevate the sandbox policy");
    assert!(
        !agent_policy.has_network_access(),
        "Agent mode must not grant shell network access by default; got {agent_policy:?}",
    );
    assert!(
        !agent_policy
            .get_writable_roots(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .is_empty(),
        "Agent mode must still elevate workspace writes; got {agent_policy:?}",
    );

    let full_access_ctx = engine.build_tool_context(AppMode::Agent, true);
    let full_access_policy = full_access_ctx
        .elevated_sandbox_policy
        .as_ref()
        .expect("Full Access should elevate the sandbox policy");
    assert!(full_access_policy.has_network_access());
    // v0.8.11: Full Access drops to DangerFullAccess (no sandbox) so the
    // user is not bounced through approval round-trips for legitimate
    // outside-workspace writes (package installs, sub-agent
    // workspaces, ~/.cache mutations, etc.). Full Access is opt-in and
    // already enables trust mode + auto-approve; the sandbox was the
    // last guardrail and contradicts the contract.
    assert!(
        matches!(
            full_access_policy,
            crate::sandbox::SandboxPolicy::DangerFullAccess
        ),
        "Full Access must use DangerFullAccess (no sandbox); got {full_access_policy:?}",
    );

    // Plan mode (#1077): the sandbox must actually deny workspace writes.
    // The previous WorkspaceWrite-with-empty-network policy whitelisted the
    // workspace as writable, so `python -c "open('f','w').write('x')"`
    // mutated files inside the workspace despite Plan-mode's intent. Lock
    // it to ReadOnly: no writes anywhere, no network. The shell tool stays
    // exposed for read-only inspection (`ls`, `git log`, `grep`, …) and
    // the per-platform sandbox enforces the rest.
    let plan_ctx = engine.build_tool_context(AppMode::Plan, false);
    let plan_policy = plan_ctx
        .elevated_sandbox_policy
        .as_ref()
        .expect("Plan mode should make the shell sandbox policy explicit");
    assert!(
        matches!(plan_policy, crate::sandbox::SandboxPolicy::ReadOnly { .. }),
        "Plan mode must use ReadOnly sandbox to deny workspace writes (#1077); got {plan_policy:?}",
    );
    assert!(!plan_policy.has_network_access());
    assert!(!plan_policy.has_full_disk_write_access());
    assert!(
        plan_policy
            .get_writable_roots(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .is_empty(),
        "ReadOnly policy must enumerate zero writable roots; got {plan_policy:?}",
    );
    assert!(
        plan_ctx
            .shell_network_denied_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("Plan mode") && hint.contains("read-only")),
    );
}

#[test]
fn sandbox_policy_for_turn_returns_correct_default_policy_per_mode() {
    use crate::core::authority::{SandboxNetworkAccess, sandbox_policy_for_turn};
    use crate::sandbox::SandboxPolicy;
    use ApprovalMode;

    let workspace = PathBuf::from("/tmp/example-workspace");

    // Plan: ReadOnly. The whole point of #1077.
    assert!(matches!(
        sandbox_policy_for_turn(
            AppMode::Plan,
            ApprovalMode::Suggest,
            None,
            &workspace,
            SandboxNetworkAccess::Restricted,
        ),
        SandboxPolicy::ReadOnly { .. }
    ));

    // Agent: WorkspaceWrite with workspace as writable root, network OFF.
    match sandbox_policy_for_turn(
        AppMode::Agent,
        ApprovalMode::Suggest,
        None,
        &workspace,
        SandboxNetworkAccess::Restricted,
    ) {
        SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network_access,
            ..
        } => {
            assert_eq!(writable_roots, vec![workspace.clone()]);
            assert!(
                !network_access,
                "workspace-write must not imply shell network access"
            );
        }
        other => panic!("Agent mode should be WorkspaceWrite; got {other:?}"),
    }

    // Agent with the explicit opt-in: same posture, network on.
    match sandbox_policy_for_turn(
        AppMode::Agent,
        ApprovalMode::Suggest,
        None,
        &workspace,
        SandboxNetworkAccess::Allowed,
    ) {
        SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            assert!(
                network_access,
                "sandbox_network_access = true must grant shell network access"
            );
        }
        other => panic!("Agent mode should be WorkspaceWrite; got {other:?}"),
    }

    // Bypass posture: DangerFullAccess.
    assert!(matches!(
        sandbox_policy_for_turn(
            AppMode::Agent,
            ApprovalMode::Bypass,
            None,
            &workspace,
            SandboxNetworkAccess::Restricted,
        ),
        SandboxPolicy::DangerFullAccess
    ));
}

#[tokio::test]
async fn session_update_preserves_reasoning_tool_only_turn() {
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &Config::default());
    let assistant = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                signature: None,
                state: None,
                redacted_data: None,
                thinking: "Need a tool before answering.".to_string(),
            },
            ContentBlock::ToolUse {
                execution_id: None,
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({"path": "Cargo.toml"}),
                caller: None,
                thought_signature: None,
            },
        ],
    };

    engine.add_session_message(assistant.clone()).await;

    let event = {
        let mut rx = handle.rx_event.write().await;
        rx.recv().await.expect("session update event")
    };
    let Event::SessionUpdated { messages, .. } = event else {
        panic!("expected session update event");
    };

    assert_eq!(*messages, vec![assistant]);
}

#[tokio::test]
async fn set_model_reloads_instruction_sources_and_updates_session_prompt() {
    let tmp = tempdir().expect("tempdir");
    let instructions = tmp.path().join("instructions.md");
    fs::write(&instructions, "FLASH_INSTRUCTIONS_MARKER").expect("write instructions");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-flash".to_string(),
        instructions: vec![instructions.clone().into()],
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());
    fs::write(&instructions, "PRO_INSTRUCTIONS_MARKER").expect("rewrite instructions");

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::SetModel {
            model: "deepseek-v4-pro".to_string(),
            mode: AppMode::Agent,
            route_limits: None,
        })
        .await
        .expect("send set model");

    let (model, prompt) = {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("session update after model switch")
                .expect("event");
            if let Event::SessionUpdated {
                model,
                system_prompt,
                ..
            } = event
            {
                let prompt = match system_prompt.expect("system prompt") {
                    SystemPrompt::Text(text) => text,
                    SystemPrompt::Blocks(blocks) => blocks
                        .into_iter()
                        .map(|block| block.text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                break (model, prompt);
            }
        }
    };
    run.abort();

    assert_eq!(model, "deepseek-v4-pro");
    assert!(prompt.contains("PRO_INSTRUCTIONS_MARKER"));
    assert!(!prompt.contains("FLASH_INSTRUCTIONS_MARKER"));
}

#[tokio::test]
async fn change_mode_refreshes_session_prompt_and_updates_session() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::ChangeMode {
            mode: AppMode::Agent,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            configured_sandbox_mode: None,
        })
        .await
        .expect("send change mode");

    let (_prompt, messages) = {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("session update after mode switch")
                .expect("event");
            if let Event::SessionUpdated {
                system_prompt,
                messages,
                ..
            } = event
            {
                let prompt = match system_prompt.expect("system prompt") {
                    SystemPrompt::Text(text) => text,
                    SystemPrompt::Blocks(blocks) => blocks
                        .into_iter()
                        .map(|block| block.text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                break (prompt, messages);
            }
        }
    };
    run.abort();

    assert!(
        messages.iter().all(|message| message.role != "system"),
        "mode switch must not persist appended system messages: {messages:?}"
    );
}

/// A posture change announces itself in product words (§19): Permissions,
/// then Plan / Work / Operate. A republished identical posture says nothing.
#[tokio::test]
async fn posture_change_status_uses_permissions_and_work() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, handle) = Engine::new(config, &Config::default());
    let publish = |handle: &EngineHandle| {
        handle
            .try_send(Op::ChangeMode {
                mode: AppMode::Agent,
                allow_shell: true,
                trust_mode: false,
                auto_approve: true,
                approval_mode: ApprovalMode::Bypass,
                configured_sandbox_mode: None,
            })
            .expect("publish live runtime authority");
    };
    publish(&handle);
    assert!(engine.apply_pending_runtime_authority().await);
    publish(&handle);
    assert!(!engine.apply_pending_runtime_authority().await);

    let mut statuses = Vec::new();
    let mut rx = handle.rx_event.write().await;
    while let Ok(event) = rx.try_recv() {
        if let Event::Status { message } = event {
            statuses.push(message);
        }
    }
    assert_eq!(
        statuses,
        vec!["Permissions: Full Access · Work".to_string()]
    );
}

#[tokio::test]
async fn live_runtime_authority_applies_latest_posture_and_sandbox_before_tools() {
    use crate::sandbox::SandboxPolicy;
    use ApprovalMode;

    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, handle) = Engine::new(config, &Config::default());
    let registry = ToolRegistryBuilder::new()
        .build(engine.build_tool_context(engine.current_mode, engine.session.auto_approve));

    for (mode, posture, auto_approve, sandbox_mode, expected_sandbox) in [
        (
            AppMode::Operate,
            ApprovalMode::Auto,
            false,
            Some("read-only".to_string()),
            SandboxPolicy::read_only(),
        ),
        (
            AppMode::Agent,
            ApprovalMode::Bypass,
            true,
            None,
            SandboxPolicy::DangerFullAccess,
        ),
        (
            AppMode::Agent,
            ApprovalMode::Suggest,
            false,
            None,
            SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![tmp.path().to_path_buf()],
                network_access: false,
                exclude_tmpdir: false,
                exclude_slash_tmp: false,
            },
        ),
    ] {
        handle
            .try_send(Op::ChangeMode {
                mode,
                allow_shell: true,
                trust_mode: false,
                auto_approve,
                approval_mode: posture,
                configured_sandbox_mode: sandbox_mode,
            })
            .expect("publish live runtime authority");

        let published = handle.runtime_permission_authority();
        assert_eq!(published.approval_mode, posture);
        assert_eq!(published.auto_approve, auto_approve);
        assert!(engine.apply_pending_runtime_authority().await);
        assert_eq!(engine.current_mode, mode);
        assert_eq!(engine.session.approval_mode, posture);
        assert_eq!(engine.session.auto_approve, auto_approve);
        assert_eq!(
            engine
                .live_tool_context(Some(&registry))
                .expect("live registry context")
                .elevated_sandbox_policy,
            Some(expected_sandbox),
        );
        // Tools carry the posture the turn resolved, so a task they create can
        // pin the authority it was actually granted.
        assert_eq!(
            engine
                .live_tool_context(Some(&registry))
                .expect("live registry context")
                .approval_mode,
            posture,
        );
    }
}

#[test]
fn turn_approval_mode_prefers_auto_approve_flag() {
    use ApprovalMode;

    assert_eq!(
        agent_approval_mode_for_turn(true, ApprovalMode::Suggest),
        ApprovalMode::Bypass
    );
    assert_eq!(
        agent_approval_mode_for_turn(true, ApprovalMode::Never),
        ApprovalMode::Bypass
    );
}

#[test]
fn messages_with_turn_metadata_returns_stored_session_messages() {
    use ApprovalMode;

    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine.current_mode = AppMode::Plan;
    engine.session.approval_mode = ApprovalMode::Suggest;
    engine.session.messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "summary after compaction".to_string(),
            cache_control: None,
        }],
    }]
    .into();
    let stored = engine.session.messages.clone();

    let request_messages = engine.messages_with_turn_metadata();

    assert_eq!(&*engine.session.messages, &*stored);
    assert_eq!(request_messages.len(), stored.len());
    assert!(
        request_messages
            .iter()
            .all(|message| message.role != "system"),
        "model request projection must not create appended system messages"
    );
}

// === To-do state reaches the model through its own tool results ===
//
// Codewhale has one To-do list. The model learns what is on it the way it
// learns anything else: from its own `work_update` call and the short receipt
// it returned, which are ordinary persisted history. No request re-states the list, on any
// step. The complete list stays visible in the UI, which is a different
// surface from the request.

fn todo_engine() -> (
    Engine,
    EngineHandle,
    crate::tools::todo::SharedTodoList,
    crate::work_graph::SharedWorkRuntime,
    tempfile::TempDir,
) {
    let tmp = tempdir().expect("tempdir");
    let todos = crate::tools::todo::new_shared_todo_list();
    let plan = crate::tools::plan::new_shared_plan_state();
    let work = crate::work_graph::new_shared_work_runtime(todos.clone(), plan.clone());
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        todos: todos.clone(),
        plan_state: plan,
        runtime_services: crate::tools::spec::RuntimeToolServices {
            work: Some(work.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let (mut engine, handle) = Engine::new(config, &Config::default());
    engine.session.messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "land the To-do seam".to_string(),
            cache_control: None,
        }],
    }]
    .into();
    (engine, handle, todos, work, tmp)
}

fn message_text_of(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the real `work_update` tool against the attached graph — not a direct
/// mutation of the legacy list, which is exactly the state the fork seam must
/// stop trusting.
async fn run_graph_backed_work_update(
    todos: &crate::tools::todo::SharedTodoList,
    work: &crate::work_graph::SharedWorkRuntime,
    items: serde_json::Value,
) {
    use crate::tools::spec::ToolSpec as _;
    let mut context = crate::tools::spec::ToolContext::new(std::env::temp_dir());
    context.runtime.work = Some(work.clone());
    crate::tools::todo::TodoWriteTool::new(todos.clone())
        .execute(json!({ "todos": items }), &context)
        .await
        .expect("graph-backed todo_write");
}

/// A non-empty To-do adds nothing to the messages a request is built from.
#[tokio::test]
async fn a_non_empty_todo_adds_nothing_to_the_request_messages() {
    let (engine, _handle, todos, work, _tmp) = todo_engine();
    let stored = engine.session.messages.clone();

    run_graph_backed_work_update(
        &todos,
        &work,
        json!([
            { "content": "read the runtime seam", "status": "completed" },
            { "content": "write the renderer", "status": "in_progress" }
        ]),
    )
    .await;

    let request = engine.messages_with_turn_metadata();

    assert_eq!(request.len(), stored.len(), "nothing may be appended");
    assert_eq!(&*engine.session.messages, &*stored, "history is untouched");
    for message in &request {
        let text = message_text_of(message);
        assert!(
            !text.contains("To-do ("),
            "request re-stated the list: {text}"
        );
        assert!(
            !text.contains("write the renderer"),
            "request re-stated an item: {text}"
        );
        assert!(!text.contains("codewhale:work"), "{text}");
    }
}

/// The outbound payload itself, across a whole turn and the turn after it:
/// with work on the list, no provider request body mentions it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn provider_request_bodies_never_carry_the_todo_list() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-todo\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"noted.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-todo\",\"choices\":[{\"index\":0,",
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
        .mount(&server)
        .await;

    let api_config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let todos = crate::tools::todo::new_shared_todo_list();
    let plan = crate::tools::plan::new_shared_plan_state();
    let work = crate::work_graph::new_shared_work_runtime(todos.clone(), plan.clone());
    let engine_config = EngineConfig {
        workspace: workspace.path().to_path_buf(),
        snapshots_enabled: false,
        subagents_enabled: false,
        todos: todos.clone(),
        plan_state: plan,
        runtime_services: crate::tools::spec::RuntimeToolServices {
            work: Some(work.clone()),
            ..Default::default()
        },
        ..EngineConfig::default()
    };
    run_graph_backed_work_update(
        &todos,
        &work,
        json!([{ "content": "ship the outbound payload test", "status": "in_progress" }]),
    )
    .await;

    let (engine, handle) = Engine::new(engine_config, &api_config);
    let task = tokio::spawn(engine.run());

    for prompt in ["first turn", "second turn"] {
        handle
            .send(external_user_message_op(
                prompt,
                AppMode::Agent,
                &api_config,
            ))
            .await
            .expect("send turn");
        let mut rx = handle.rx_event.write().await;
        while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for turn completion")
        {
            match event {
                Event::Error { envelope, .. } => panic!("turn errored: {envelope:?}"),
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
    }

    let requests = server
        .received_requests()
        .await
        .expect("recorded provider requests");
    assert!(requests.len() >= 2, "expected one request per turn");
    for request in &requests {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains("ship the outbound payload test"),
            "a provider request restated the To-do list: {body}"
        );
        assert!(!body.contains("To-do ("), "{body}");
        assert!(!body.contains("codewhale:work"), "{body}");
    }

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

/// The turn-start structured state is deliberately To-do-free: the list moves
/// during a turn, so it is resolved at the fork seam instead.
#[test]
fn turn_start_structured_state_carries_no_todo_section() {
    let state = StructuredState {
        mode_label: "Agent".to_string(),
        workspace: PathBuf::from("/workspace/codewhale"),
        cwd: None,
        working_set_summary: None,
        subagent_snapshots: Vec::new(),
    };

    let block = state.to_system_block().expect("fork state block");

    assert!(
        !block.contains(crate::todo_snapshot::FORK_TODO_SECTION_HEADING),
        "stable capture must not pin a To-do section: {block}"
    );
    assert!(!block.contains("To-do ("));
}

/// The fork handoff and `/relay` show the same To-do snapshot body. Relay
/// parity is asserted in `commands::tests`.
#[tokio::test]
async fn fork_state_block_reuses_the_snapshot_body() {
    let (engine, _handle, todos, work, _tmp) = todo_engine();
    run_graph_backed_work_update(
        &todos,
        &work,
        json!([
            { "content": "Wire Fleet progress projection", "status": "in_progress" },
            { "content": "Run focused gates", "status": "pending" }
        ]),
    )
    .await;
    let snapshot = engine.todo_source().snapshot().await;
    let body = crate::todo_snapshot::todo_snapshot_body(&snapshot).expect("body");

    let state = StructuredState {
        mode_label: "Agent".to_string(),
        workspace: PathBuf::from("/workspace/codewhale"),
        cwd: None,
        working_set_summary: None,
        subagent_snapshots: Vec::new(),
    };
    let fork_context = crate::tools::subagent::SubAgentForkContext {
        messages: engine.messages_with_turn_metadata(),
        live_header: crate::prompt_zones::new_live_header_cell(),
        structured_state_block: state.to_system_block(),
        work_source: Some(engine.todo_source()),
    };

    let resolved = fork_context
        .with_resolved_state_block()
        .await
        .structured_state_block
        .expect("resolved fork state block");

    assert!(resolved.contains(&body), "fork body drifted: {resolved}");
}

/// `update_plan` is conversational reasoning, not a second list: plan-only
/// state must not produce a To-do snapshot.
#[tokio::test]
async fn plan_only_state_produces_no_todo_snapshot() {
    let (engine, _handle, _todos, _work, _tmp) = todo_engine();
    {
        let mut plan = engine.config.plan_state.lock().await;
        plan.update(crate::tools::plan::UpdatePlanArgs {
            objective: Some("Ship the To-do seam".to_string()),
            plan: vec![crate::tools::plan::PlanItemArg {
                step: "draft the renderer".to_string(),
                status: crate::tools::plan::StepStatus::InProgress,
            }],
            ..crate::tools::plan::UpdatePlanArgs::default()
        });
        assert!(!plan.snapshot().is_empty());
    }

    assert!(
        engine.todo_source().body().await.is_none(),
        "legacy plan-only state must not become a To-do snapshot"
    );
}

/// A real graph-backed `work_update` stages the new projection in the
/// `WorkRuntime` and publishes into `config.todos` only later, asynchronously,
/// from the UI. The fork seam must read the staged projection, not the
/// pre-write legacy view.
#[tokio::test]
async fn fork_seam_reflects_a_graph_backed_work_update() {
    let (engine, _handle, todos, work, _tmp) = todo_engine();
    assert!(
        engine.todo_source().is_graph_backed(),
        "this engine must read the graph, not the legacy view"
    );

    run_graph_backed_work_update(
        &todos,
        &work,
        json!([
            { "content": "read the runtime seam", "status": "completed" },
            { "content": "hand the child the live list", "status": "in_progress" }
        ]),
    )
    .await;

    // The staleness this test exists for: the legacy view is still empty here.
    assert!(
        todos.lock().await.snapshot().is_empty(),
        "precondition: work_update stages in the graph and publishes later"
    );

    let body = engine.todo_source().body().await.expect("body");
    assert!(body.contains("[x] #1 read the runtime seam"), "{body}");
    assert!(
        body.contains("[~] #2 hand the child the live list"),
        "{body}"
    );
}

/// A compaction checkpoint is history, never a second To-do surface or a
/// stable-system-prefix mutation.
#[tokio::test]
async fn compaction_keeps_todos_out_of_the_prefix() {
    let (mut engine, _handle, todos, work, _tmp) = todo_engine();
    run_graph_backed_work_update(
        &todos,
        &work,
        json!([{ "content": "staged graph-only todo", "status": "in_progress" }]),
    )
    .await;
    assert!(
        todos.lock().await.snapshot().is_empty(),
        "precondition: the compatibility list is still stale"
    );

    let stable_before = engine.session.system_prompt.clone();
    engine.commit_compaction_checkpoint(Some(SystemPrompt::Text(format!(
        "{COMPACTION_SUMMARY_MARKER}\nsummary"
    ))));
    assert_eq!(engine.session.system_prompt, stable_before);
    let checkpoint = engine.rendered_compaction_summary().expect("checkpoint");
    assert!(
        !checkpoint.contains("staged graph-only todo"),
        "{checkpoint}"
    );
    assert!(!checkpoint.contains("### Todos"), "{checkpoint}");
}

#[tokio::test]
async fn compaction_completed_reports_complete_post_input_tokens() {
    let _env = crate::test_support::lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, handle) = Engine::new(config, &Config::default());
    engine.session.system_prompt = Some(SystemPrompt::Text("stable system context ".repeat(400)));
    engine.session.replace_messages(vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "post-compaction message".to_string(),
            cache_control: None,
        }],
    }]);
    engine.commit_compaction_checkpoint(Some(SystemPrompt::Text(format!(
        "{COMPACTION_SUMMARY_MARKER}\npost-compaction summary"
    ))));

    let messages_only =
        crate::compaction::estimate_input_tokens_for_pressure(&engine.session.messages, None);
    let expected = engine.estimated_input_tokens();
    assert!(expected > messages_only);

    engine
        .emit_compaction_completed(
            "compact_test".to_string(),
            false,
            "Made room".to_string(),
            Some(4),
            Some(1),
            super::compaction::CompactionPass {
                trigger: "manual",
                path: crate::compaction::CompactionPath::Summary,
                tokens_before: 9000,
                threshold_tokens: 8000,
                usage: Usage {
                    input_tokens: 120,
                    output_tokens: 15,
                    ..Default::default()
                },
            },
        )
        .await;

    let event = handle
        .rx_event
        .write()
        .await
        .recv()
        .await
        .expect("compaction completed event");
    let Event::CompactionCompleted {
        post_input_tokens, ..
    } = event
    else {
        panic!("expected CompactionCompleted, got {event:?}");
    };
    assert_eq!(post_input_tokens, Some(expected as u64));
    let log = std::fs::read_to_string(tmp.path().join("audit.log")).unwrap();
    let records = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["event"] == "compaction.completed")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let details = &records[0]["details"];
    assert_eq!(details["messages_before"], 4);
    assert_eq!(details["messages_after"], 1);
    assert_eq!(details["reduction_ratio"], 0.75);
    assert_eq!(details["estimated_tokens_before"], 9000);
    assert_eq!(details["estimated_tokens_after"], expected);
    assert_eq!(details["threshold_tokens"], 8000);
    assert_eq!(details["summarizer_usage"]["input_tokens"], 120);
    assert_eq!(details["trigger"], "manual");
    assert_eq!(details["path"], "summary");
    assert!(!log.contains("post-compaction message"));
    engine
        .record_compaction_event(
            "compaction.refused",
            serde_json::json!({
                "trigger": "auto", "reason": "retained_floor", "threshold_tokens": 8000,
            }),
        )
        .await;
    assert!(
        std::fs::read_to_string(tmp.path().join("audit.log"))
            .unwrap()
            .contains("compaction.refused")
    );
}

/// `fork_context` is captured once at turn start, so a `work_update` followed
/// by an `agent` spawn *in the same turn* must still hand the child the
/// current snapshot. Only the To-do portion is refreshed; the inherited
/// transcript and stable state text are unchanged.
#[tokio::test]
async fn same_turn_fork_carries_the_updated_todo() {
    let (engine, _handle, todos, work, _tmp) = todo_engine();

    // Turn start: capture the fork context, before any work exists.
    let stable_block = StructuredState {
        mode_label: "Agent".to_string(),
        workspace: engine.config.workspace.clone(),
        cwd: None,
        working_set_summary: None,
        subagent_snapshots: Vec::new(),
    }
    .to_system_block();
    let fork_context = crate::tools::subagent::SubAgentForkContext {
        messages: engine.messages_with_turn_metadata(),
        live_header: crate::prompt_zones::new_live_header_cell(),
        structured_state_block: stable_block.clone(),
        work_source: Some(engine.todo_source()),
    };
    let captured_messages = fork_context.messages.clone();
    assert!(
        !fork_context
            .with_resolved_state_block()
            .await
            .structured_state_block
            .expect("stable block")
            .contains("To-do ("),
        "no work yet, so no To-do section"
    );

    // Mid-turn: the model calls work_update, then spawns an agent.
    run_graph_backed_work_update(
        &todos,
        &work,
        json!([{ "content": "hand the child the live list", "status": "in_progress" }]),
    )
    .await;

    let resolved = fork_context.with_resolved_state_block().await;
    let block = resolved
        .structured_state_block
        .as_deref()
        .expect("resolved block");
    let snapshot = engine.todo_source().snapshot().await;
    let body = crate::todo_snapshot::todo_snapshot_body(&snapshot).expect("body");

    assert!(
        block.contains(&body),
        "same-turn fork must carry the current body: {block}"
    );
    assert!(
        block.contains("[~] #1 hand the child the live list"),
        "{block}"
    );
    // Stable history semantics are untouched.
    assert_eq!(resolved.messages, captured_messages);
    assert!(
        block.starts_with(stable_block.as_deref().expect("stable").trim()),
        "the stable capture must stay a byte-identical prefix: {block}"
    );
}

/// U1: hosts resend the compaction config on every model, route or session
/// sync. Neither a session sync nor a config whose switch did not move should
/// produce a status line: it would overwrite a real error (the missing-key
/// notice) or the host's confirmed "Resumed:" receipt in the footer.
#[tokio::test]
async fn unchanged_compaction_config_is_acknowledged_silently() {
    let tmp = tempdir().expect("tempdir");
    let (engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        },
        &Config::default(),
    );
    let current = engine.config.compaction.clone();
    let restored_messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Restored conversation proof".to_string(),
            cache_control: None,
        }],
    }];
    let run = tokio::spawn(engine.run());
    handle
        .send(Op::SyncSession {
            session_id: Some("resumed-session".to_string()),
            messages: restored_messages.clone(),
            system_prompt: None,
            system_prompt_override: false,
            model: current.model.clone(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync restored session");
    handle
        .send(Op::SetCompaction {
            config: current.clone(),
        })
        .await
        .expect("send unchanged config");
    // A session restore resyncs the model and window with the switch as it
    // was: applied, but not news.
    let mut resynced = current.clone();
    resynced.model = format!("{}-resynced", current.model);
    resynced.effective_context_window = Some(64_000);
    handle
        .send(Op::SetCompaction {
            config: resynced.clone(),
        })
        .await
        .expect("send resynced config");
    let mut changed = resynced;
    changed.enabled = !changed.enabled;
    let expected = if changed.enabled {
        "Make room automatically: on"
    } else {
        "Make room automatically: off"
    };
    handle
        .send(Op::SetCompaction { config: changed })
        .await
        .expect("send changed config");

    let mut rx = handle.rx_event.write().await;
    let mut session_updated = false;
    let first_status = loop {
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("status after a real change")
            .expect("event");
        match event {
            Event::SessionUpdated {
                session_id,
                messages,
                model,
                workspace,
                ..
            } => {
                assert_eq!(session_id, "resumed-session");
                assert_eq!(*messages, restored_messages);
                assert_eq!(model, current.model);
                assert_eq!(workspace, tmp.path());
                session_updated = true;
            }
            Event::Status { message } => break message,
            _ => {}
        }
    };
    assert!(
        session_updated,
        "session sync still publishes its authoritative update"
    );
    assert_eq!(
        first_status, expected,
        "session sync and unchanged/resynced configs produced no status; only the switch did"
    );
    drop(rx);
    run.abort();
}

#[tokio::test]
async fn change_mode_op_updates_current_mode_and_emits_status() {
    let tmp = tempdir().expect("tempdir");
    let mut config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    config.features.disable(Feature::Mcp);
    let (engine, handle) = Engine::new(config, &Config::default());

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::ChangeMode {
            mode: AppMode::Agent,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            configured_sandbox_mode: None,
        })
        .await
        .expect("send change mode");

    // Expect a SessionUpdated event confirming the mode change.
    let mut rx = handle.rx_event.write().await;
    let session_updated = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("session update after mode switch")
        .expect("event");
    let Event::SessionUpdated { messages, .. } = session_updated else {
        panic!("should emit SessionUpdated after mode change, got: {session_updated:?}");
    };
    assert!(
        messages.iter().all(|message| message.role != "system"),
        "mode switch must not persist synthetic system messages: {messages:?}"
    );

    // Also expect a status event
    let status = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("status after mode switch")
        .expect("event");
    assert!(
        matches!(status, Event::Status { .. }),
        "should emit Status after mode change, got: {status:?}"
    );

    run.abort();
}

#[test]
fn runtime_mode_policy_updates_engine_session_mirrors() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        allow_shell: false,
        trust_mode: false,
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine.current_mode = AppMode::Plan;
    engine.session.allow_shell = false;
    engine.session.trust_mode = false;
    engine.session.auto_approve = false;
    engine.session.approval_mode = ApprovalMode::Suggest;

    let agent_authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Never,
    );
    engine.apply_runtime_mode_policy(&agent_authority);

    assert_eq!(engine.current_mode, AppMode::Agent);
    assert!(engine.session.allow_shell);
    assert!(engine.config.allow_shell);
    assert!(!engine.session.trust_mode);
    assert!(!engine.config.trust_mode);
    assert!(!engine.session.auto_approve);
    assert_eq!(engine.session.approval_mode, ApprovalMode::Never);

    let full_access_authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    engine.apply_runtime_mode_policy(&full_access_authority);

    assert_eq!(engine.current_mode, AppMode::Agent);
    assert!(engine.session.allow_shell);
    assert!(engine.session.trust_mode);
    assert!(engine.config.trust_mode);
    assert!(engine.session.auto_approve);
    assert_eq!(engine.session.approval_mode, ApprovalMode::Bypass);
}

#[tokio::test]
async fn sync_session_restores_current_mode() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::SyncSession {
            session_id: Some("plan-session".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Plan,
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

    assert_eq!(snapshot.mode, "plan");

    run.abort();
}

#[tokio::test]
async fn sync_session_without_prompt_repins_full_system_prompt_on_next_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const WORKSPACE_RULE: &str = "SYNC_SESSION_FULL_PROMPT_PROOF";

    async fn wait_for_completed_turn(handle: &EngineHandle) {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for turn completion")
                .expect("engine event channel closed before turn completion");
            if let Event::TurnComplete { status, error, .. } = event {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
        }
    }

    let workspace = tempdir().expect("tempdir");
    fs::write(
        workspace.path().join("AGENTS.md"),
        format!("# Rules\n\nAlways preserve {WORKSPACE_RULE}.\n"),
    )
    .expect("write AGENTS.md fixture");
    let config = Config::default();
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "Turn complete.",
    )]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        client,
    );
    let established_context = engine.installed_next_turn_prompt_context();
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&established_context),
        None
    );
    assert_eq!(
        engine.session.pinned_prompt_context.as_ref(),
        Some(&established_context),
        "precondition: the outgoing conversation has an established prompt pin"
    );
    let task = tokio::spawn(engine.run());

    handle
        .send(Op::SyncSession {
            session_id: Some("fresh-session".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: crate::config::DEFAULT_TEXT_MODEL.to_string(),
            workspace: workspace.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync fresh session without a persisted prompt");
    let synced = handle
        .get_session_snapshot()
        .await
        .expect("drain session sync");
    assert!(
        synced.system_prompt.is_none(),
        "SyncSession must install the persisted prompt exactly before the next turn"
    );

    handle
        .send(external_user_message_op(
            "Start the newly synchronized conversation.",
            AppMode::Agent,
            &config,
        ))
        .await
        .expect("send first turn after sync");
    wait_for_completed_turn(&handle).await;

    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 1);
    let repinned = requests[0]
        .system
        .clone()
        .map(system_prompt_text)
        .expect("the first turn after SyncSession must send a full system prompt");
    assert!(repinned.contains(WORKSPACE_RULE), "{repinned}");
    assert!(
        requests[0].messages.iter().all(|message| {
            message.content.iter().all(|block| {
                !matches!(
                    block,
                    ContentBlock::Text { text, .. } if text.starts_with("<context_update>")
                )
            })
        }),
        "the fresh session must not inherit a context-update delta: {:?}",
        requests[0].messages
    );

    let refreshed = handle
        .get_session_snapshot()
        .await
        .expect("snapshot refreshed session");
    let refreshed_prompt = refreshed
        .system_prompt
        .map(system_prompt_text)
        .expect("refreshed session prompt");
    assert!(refreshed_prompt.contains(WORKSPACE_RULE));
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn sync_session_same_id_does_not_finalize_live_worker() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let config = EngineConfig {
        workspace: workspace.clone(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());
    let manager = engine.subagent_manager.clone();

    let run = tokio::spawn(engine.run());
    // Install the conversation identity first so the manager is not
    // finalized by the very first identity transition away from the
    // construction-time UUID.
    handle
        .send(Op::SyncSession {
            session_id: Some("session-keep".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: workspace.clone(),
            mode: AppMode::Agent,
        })
        .await
        .expect("install session");
    handle.get_session_snapshot().await.expect("drain install");

    let agent_id = {
        let mut manager = manager.write().await;
        manager.insert_test_running_agent("keep", &workspace)
    };

    // A same-id re-sync is a reload, not a conversation boundary.
    handle
        .send(Op::SyncSession {
            session_id: Some("session-keep".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: workspace.clone(),
            mode: AppMode::Agent,
        })
        .await
        .expect("re-sync same session");
    handle.get_session_snapshot().await.expect("drain re-sync");

    let record = manager
        .read()
        .await
        .get_worker_record(&agent_id)
        .expect("live worker record");
    assert!(
        !record.status.is_terminal(),
        "same-id re-sync must not finalize the worker: {:?}",
        record.status
    );

    run.abort();
}

#[tokio::test]
async fn sync_session_different_id_finalizes_live_worker() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let config = EngineConfig {
        workspace: workspace.clone(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());
    let manager = engine.subagent_manager.clone();

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::SyncSession {
            session_id: Some("session-a".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: workspace.clone(),
            mode: AppMode::Agent,
        })
        .await
        .expect("install session-a");
    handle.get_session_snapshot().await.expect("drain install");

    let agent_id = {
        let mut manager = manager.write().await;
        let agent_id = manager.insert_test_running_agent("close", &workspace);
        manager.assign_test_session_owner(&agent_id, "session-a");
        agent_id
    };

    // A different id is a conversation boundary: the live worker must be
    // finalized with the session-closed reason.
    handle
        .send(Op::SyncSession {
            session_id: Some("session-b".to_string()),
            messages: Vec::new(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: workspace.clone(),
            mode: AppMode::Agent,
        })
        .await
        .expect("switch to session-b");
    handle.get_session_snapshot().await.expect("drain switch");

    let record = manager
        .read()
        .await
        .get_worker_record(&agent_id)
        .expect("worker record after close");
    assert!(
        record.status.is_terminal(),
        "different-id switch must finalize the worker: {:?}",
        record.status
    );
    assert_eq!(
        record.status,
        crate::tools::subagent::AgentWorkerStatus::Interrupted
    );
    let reason = record.latest_message.as_deref().unwrap_or("");
    assert!(
        reason.contains("parent session closed"),
        "session-closed reason missing: {reason}"
    );

    run.abort();
}

#[tokio::test]
async fn sync_session_migrates_one_checkpoint_and_strips_its_system_carrier() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());
    let carrier = SystemPrompt::Text(format!(
        "stable host prompt\n\n<!-- compaction-summary:begin -->\n{COMPACTION_SUMMARY_MARKER}\nnew checkpoint\n<!-- compaction-summary:end -->"
    ));
    let old_checkpoint = crate::compaction::compaction_checkpoint_message(&SystemPrompt::Text(
        format!("{COMPACTION_SUMMARY_MARKER}\nold checkpoint"),
    ));

    let run = tokio::spawn(engine.run());
    let mut messages = vec![old_checkpoint];
    for round in 0..2 {
        handle
            .send(Op::SyncSession {
                session_id: Some("compacted-session".to_string()),
                messages,
                system_prompt: Some(carrier.clone()),
                system_prompt_override: true,
                model: "deepseek-v4-pro".to_string(),
                workspace: tmp.path().to_path_buf(),
                mode: AppMode::Agent,
            })
            .await
            .expect("sync compacted session");

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

        let checkpoints = snapshot
            .messages
            .iter()
            .filter(|message| crate::compaction::is_compaction_checkpoint_message(message))
            .collect::<Vec<_>>();
        assert_eq!(checkpoints.len(), 1, "round {round}: {checkpoints:?}");
        let checkpoint_text = message_text_of(checkpoints[0]);
        assert!(
            checkpoint_text.contains("new checkpoint"),
            "{checkpoint_text}"
        );
        assert!(
            !checkpoint_text.contains("old checkpoint"),
            "{checkpoint_text}"
        );
        assert_eq!(
            snapshot.system_prompt,
            Some(SystemPrompt::Text("stable host prompt".to_string()))
        );
        messages = snapshot.messages;
    }

    run.abort();
}

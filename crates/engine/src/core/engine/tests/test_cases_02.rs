const REPRESENTATIVE_HANDOFF_RELAY: &str = "REPRESENTATIVE_HANDOFF_RELAY";

#[test]
fn ordinary_engine_default_does_not_install_a_step_budget() {
    assert_eq!(
        DEFAULT_MODEL_STEPS,
        codewhale_runtime::turn_budget::DEFAULT_MAX_MODEL_STEPS
    );
    assert_eq!(EngineConfig::default().max_steps, DEFAULT_MODEL_STEPS);
    let mut turn = TurnContext::new(EngineConfig::default().max_steps);
    assert_eq!(turn.step_limit(), None);
    assert_eq!(turn.stop_diagnostics.effective_max_steps, None);
    // No substitute ceiling or overflow may stop an uncapped turn.
    turn.step = u32::MAX - 1;
    assert!(turn.next_step());
    assert!(turn.next_step());
    assert!(!turn.at_max_steps());
    assert_eq!(turn.steps_used(), u32::MAX);
}

#[test]
fn registry_instruction_is_in_the_initial_prompt_only_when_mcp_is_enabled() {
    let enabled = EngineConfig::default();
    let (engine, _handle) = Engine::new(enabled, &Config::default());
    let prompt = crate::prompts::system_prompt_flat_text(
        engine
            .session
            .system_prompt
            .as_ref()
            .expect("system prompt"),
    );
    assert!(prompt.contains(MCP_REGISTRY_FIRST_INSTRUCTION_SOURCE));
    assert!(prompt.contains("registry_sync"));
    assert!(prompt.contains("start_registry_mcp_server"));

    let mut disabled = EngineConfig::default();
    disabled.features.disable(Feature::Mcp);
    let (engine, _handle) = Engine::new(disabled, &Config::default());
    let prompt = crate::prompts::system_prompt_flat_text(
        engine
            .session
            .system_prompt
            .as_ref()
            .expect("system prompt"),
    );
    assert!(!prompt.contains(MCP_REGISTRY_FIRST_INSTRUCTION_SOURCE));
}

/// The engine hands every agent the live posture cell, not a copy: a
/// posture the person publishes after the agent's context was built is what
/// the agent's next call runs under: approval, shell, sandbox.
#[test]
fn agent_tool_contexts_follow_the_published_posture() {
    let (engine, handle) = Engine::new(EngineConfig::default(), &Config::default());
    let context = engine.build_tool_context(AppMode::Agent, false);
    let mut before = context.clone();
    before
        .refresh_live_posture()
        .expect("engine contexts carry the live cell");
    assert!(!before.auto_approve);
    assert_ne!(
        before.elevated_sandbox_policy,
        Some(codewhale_sandbox::SandboxPolicy::DangerFullAccess)
    );
    handle.publish_turn_authority(
        AppMode::Agent,
        true,
        false,
        true,
        ApprovalMode::Bypass,
        None,
    );
    let mut after = context;
    after.refresh_live_posture();
    assert!(after.auto_approve);
    assert_eq!(after.approval_mode, ApprovalMode::Bypass);
    assert_eq!(
        after.elevated_sandbox_policy,
        Some(codewhale_sandbox::SandboxPolicy::DangerFullAccess)
    );
    // Narrowing reaches it too: Plan drops shell and makes the sandbox read-only.
    handle.publish_turn_authority(
        AppMode::Plan,
        true,
        false,
        false,
        ApprovalMode::Suggest,
        None,
    );
    after.refresh_live_posture();
    assert!(!after.auto_approve);
    assert_eq!(
        after.shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
    assert_eq!(
        after.elevated_sandbox_policy,
        Some(codewhale_sandbox::SandboxPolicy::read_only())
    );
}

/// "Gets stuck": an agent's approval answer only reached the agent when the
/// engine was idle or itself awaiting approval. While the parent turn
/// streamed or ran tools nobody read it, and the agent waited on. The handle
/// now hands it to the agent directly — here with no engine running at all.
#[tokio::test]
async fn an_agent_approval_answer_reaches_the_agent_while_the_engine_is_busy() {
    let mock = mock_engine_handle();
    let id = format!("agent:agent_busy:approval:{}", uuid::Uuid::new_v4());
    let (_, waiting) = mock
        .handle
        .subagent_manager
        .write()
        .await
        .register_child_approval("agent_busy", &id, "bash", "held")
        .expect("register");
    mock.handle.approve_tool_call(id).await.expect("send");
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .expect("the waiting agent is answered without the engine")
        .expect("answer");
    assert_eq!(
        outcome,
        crate::tools::subagent::ChildApprovalOutcome::Approved {
            by: crate::approval_log::ApprovalDecider::User
        }
    );
}

/// The regression this test exists for. A real DeepSeek turn — "build a
/// self-contained HTML focus timer, read a local fixture, verify it" — spent
/// its steps on five `tool_search` calls for `registry_sync`, a deferred-schema
/// retry, and reasoning about starting a browser MCP server, because the
/// always-visible instruction ordered Registry discovery *before* code
/// execution or a manual implementation and named two tools that are not in the
/// catalog head.
///
/// Two properties keep that from coming back, and both are about this prompt,
/// not about a second policy surface: discovery is never ordered ahead of
/// ordinary local work, and the deferred-tool cost of reaching the Registry
/// tools is stated where the model reads about them.
#[test]
fn registry_instruction_does_not_gate_ordinary_local_work() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let prompt = crate::prompts::system_prompt_flat_text(
        engine
            .session
            .system_prompt
            .as_ref()
            .expect("system prompt"),
    );
    assert!(prompt.contains("## MCP Registry"));

    for ordered in [
        "must call `registry_sync`",
        "before `exec_shell`",
        "you must call `start_registry_mcp_server`",
        "not a reason to skip Registry discovery",
    ] {
        assert!(
            !prompt.contains(ordered),
            "Registry guidance must not order discovery ahead of ordinary work: {ordered:?}"
        );
    }

    // Available capability comes first, and the deferred cost is disclosed
    // where the model reads about the two tools it would have to hunt for.
    assert!(prompt.contains("Prefer what is already available"));
    assert!(prompt.contains("not a step before ordinary work"));
    assert!(prompt.contains("Both Registry tools are deferred"));
    assert!(prompt.contains("load one with `tool_search`"));

    // The trust boundary the instruction is actually load-bearing for: a
    // Registry package is started through the approved host path, never
    // installed or run through the shell.
    assert!(prompt.contains("rather than installing or running its package command"));
}

/// A provider's input bill describes one route's tokenization of one prompt.
/// The compaction gate and the preflight guard lift the honest estimate to
/// it, so a bill carried across a route switch would measure the next
/// request with the previous route's tokenizer and prefix. Re-installing the
/// same route keeps the carry-over #5577 relies on; a different route drops
/// it.
/// A named custom provider keeps its name, model string, and (absent) limits
/// across a config reload that points it at a different server. A different
/// server is a different tokenizer, so the bill from the old one must not
/// measure the first request to the new one (post-merge finding on #6380).
#[test]
fn custom_route_endpoint_change_forgets_the_previous_bill() {
    let mut custom = HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_0_0 = Some("lm-studio".to_string());
        let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    };
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &config);
    let install = |engine: &mut Engine, config: &Config| {
        let route = resolve_runtime_route(config, ProviderKind::Custom, Some("local-model"))
            .expect("resolve lm-studio")
            .validate()
            .expect("preflight lm-studio");
        let context_window = route.context_window;
        engine.install_validated_runtime_route(route);
        assert_eq!(engine.active_context_window, Some(context_window));
    };
    install(&mut engine, &config);
    engine.session.latest_parent_input_tokens = Some(150_000);

    install(&mut engine, &config);
    assert_eq!(
        engine.session.latest_parent_input_tokens,
        Some(150_000),
        "the same endpoint keeps the last bill"
    );

    let mut reloaded = config;
    reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider")
        .base_url = Some("http://127.0.0.1:18182/v1".to_string());
    install(&mut engine, &reloaded);
    assert_eq!(
        engine.api_provider_identity.as_ref().unwrap().key.as_str(),
        "lm-studio"
    );
    assert_eq!(
        engine.session.latest_parent_input_tokens, None,
        "a new endpoint under the same name drops the old server's bill"
    );
}

/// A catalog refresh can keep a route's name, base URL, model, and limits and
/// still move it to another endpoint key or wire protocol. Chat Completions
/// and Responses serialize a prompt differently, so the bill from one must
/// not measure the first request on the other (post-merge finding on #6381).
#[test]
fn route_protocol_change_forgets_the_previous_bill() {
    use codewhale_config::route::{RequestProtocol, ResolvedEndpoint};
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let chat = ResolvedEndpoint {
        base_url: "https://gateway.example/v1".to_string(),
        endpoint_key: "chat".to_string(),
        protocol: RequestProtocol::ChatCompletions,
    };
    engine.active_route_endpoint = Some(chat.clone());
    let identity = engine.api_provider_identity.clone().unwrap();
    let model = engine.session.model.clone();
    let limits = engine.active_route_limits;

    engine.session.latest_parent_input_tokens = Some(150_000);
    engine.forget_input_bill_if_route_changes(
        identity.key.as_str(),
        identity.persisted_id(),
        Some(&chat),
        &model,
        limits,
    );
    assert_eq!(
        engine.session.latest_parent_input_tokens,
        Some(150_000),
        "the same endpoint keeps the last bill"
    );

    let responses = ResolvedEndpoint {
        endpoint_key: "responses".to_string(),
        protocol: RequestProtocol::Responses,
        ..chat
    };
    engine.forget_input_bill_if_route_changes(
        identity.key.as_str(),
        identity.persisted_id(),
        Some(&responses),
        &model,
        limits,
    );
    assert_eq!(
        engine.session.latest_parent_input_tokens, None,
        "a new endpoint key or protocol at the same URL drops the bill"
    );
}

#[test]
fn route_switch_forgets_the_previous_routes_input_bill() {
    let mut custom = HashMap::new();
    for (name, base_url, model) in [
        ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                api_key: Some("local-test-key".to_string()),
                ..codewhale_runtime::config::ProviderConfig::default()
            },
        );
    }
    let config = {
        let __cw_config_init_1_0 = Some("custom-a".to_string());
        let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_1_value: Config = Config::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    };
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &config);
    let route_a = || {
        resolve_runtime_route(&config, ProviderKind::Custom, Some("model-a"))
            .expect("resolve custom A")
            .validate()
            .expect("preflight custom A")
    };
    engine.install_validated_runtime_route(route_a());
    engine.session.latest_parent_input_tokens = Some(150_000);

    engine.install_validated_runtime_route(route_a());
    assert_eq!(
        engine.session.latest_parent_input_tokens,
        Some(150_000),
        "re-installing the same route keeps the last bill"
    );

    let mut target = config.clone();
    target.provider = Some("custom-b".to_string());
    let route_b = resolve_runtime_route(&target, ProviderKind::Custom, Some("model-b"))
        .expect("resolve custom B")
        .validate()
        .expect("preflight custom B");
    engine.install_validated_runtime_route(route_b);
    assert_eq!(
        engine.session.latest_parent_input_tokens, None,
        "a different route drops the previous route's bill"
    );
}

#[test]
fn custom_route_identity_change_rebuilds_client_for_new_named_endpoint() {
    let mut custom = HashMap::new();
    for (name, base_url, model) in [
        ("custom-a", "http://127.0.0.1:18181/v1", "model-a"),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                api_key: Some("local-test-key".to_string()),
                ..codewhale_runtime::config::ProviderConfig::default()
            },
        );
    }
    let config = {
        let __cw_config_init_2_0 = Some("custom-a".to_string());
        let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value.providers = __cw_config_init_2_1;
        __cw_config_init_2_value
    };
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &config);
    assert_eq!(
        engine.api_provider_identity.as_ref().unwrap().key.as_str(),
        "custom-a"
    );
    assert_eq!(
        engine
            .codewhale_client
            .as_ref()
            .expect("custom A client")
            .base_url(),
        "http://127.0.0.1:18181/v1"
    );

    let mut target = config.clone();
    target.provider = Some("custom-b".to_string());
    let route = resolve_runtime_route(&target, ProviderKind::Custom, Some("model-b"))
        .expect("resolve custom B")
        .validate()
        .expect("preflight custom B");
    engine.install_validated_runtime_route(route);

    assert_eq!(
        engine.api_provider_identity.as_ref().unwrap().key.as_str(),
        "custom-b"
    );
    assert_eq!(
        engine
            .codewhale_client
            .as_ref()
            .expect("custom B client")
            .base_url(),
        "http://127.0.0.1:18182/v1"
    );
}

#[test]
fn custom_route_config_reload_rebuilds_client_when_identity_is_unchanged() {
    let mut custom = HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("old-local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_3_0 = Some("lm-studio".to_string());
        let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_3_value: Config = Config::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value.providers = __cw_config_init_3_1;
        __cw_config_init_3_value
    };
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &config);

    let mut reloaded = config;
    let provider = reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider");
    provider.base_url = Some("http://127.0.0.1:18182/v1".to_string());
    provider.api_key = Some("new-local-test-key".to_string());

    let route = resolve_runtime_route(&reloaded, ProviderKind::Custom, Some("local-model"))
        .expect("resolve reloaded route")
        .validate()
        .expect("preflight reloaded route");
    engine.install_validated_runtime_route(route);

    assert_eq!(
        engine.api_provider_identity.as_ref().unwrap().key.as_str(),
        "lm-studio"
    );
    assert_eq!(
        engine
            .codewhale_client
            .as_ref()
            .expect("reloaded custom client")
            .base_url(),
        "http://127.0.0.1:18182/v1"
    );
    assert_eq!(
        engine.api_config.active_route_base_url(),
        "http://127.0.0.1:18182/v1"
    );
}

#[test]
fn failed_same_identity_route_preflight_leaves_old_client_untouched() {
    let mut custom = HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("old-local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_4_0 = Some("lm-studio".to_string());
        let __cw_config_init_4_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_4_value: Config = Config::default();
        __cw_config_init_4_value.provider = __cw_config_init_4_0;
        __cw_config_init_4_value.providers = __cw_config_init_4_1;
        __cw_config_init_4_value
    };
    let (engine, _handle) = Engine::new(EngineConfig::default(), &config);
    assert!(engine.codewhale_client.is_some());

    let mut invalid = config;
    invalid
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("lm-studio"))
        .expect("named custom provider")
        .base_url = Some("ftp://invalid.example/v1".to_string());
    let err = crate::route_runtime::resolve_runtime_route_for_identity(
        &invalid,
        engine
            .api_provider_identity
            .as_ref()
            .expect("captured named identity"),
        Some("local-model"),
    )
    .expect_err("invalid route must fail before installation");

    assert!(err.contains("must be an http(s) URL with a host"), "{err}");
    assert_eq!(
        engine.api_provider_identity.as_ref().unwrap().key.as_str(),
        "lm-studio"
    );
    assert!(engine.codewhale_client.is_some());
    assert!(engine.model_client.is_some());
    assert!(engine.codewhale_client_error.is_none());
}

#[tokio::test]
async fn exact_turn_snapshot_restores_custom_endpoint_and_turn_receipt_after_builtin_route() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let custom_server = MockServer::start().await;
    let custom_base_url = format!("{}/v1", custom_server.uri());
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-exact-route\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"exact route\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-exact-route\",\"choices\":[{\"index\":0,",
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
        .mount(&custom_server)
        .await;

    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some(custom_base_url.clone()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_5_0 = Some("custom-a".to_string());
        let __cw_config_init_5_1 = Some(codewhale_runtime::config::ProvidersConfig {
            openai: codewhale_runtime::config::ProviderConfig {
                base_url: Some("http://127.0.0.1:18182/v1".to_string()),
                model: Some("gpt-5.5".to_string()),
                api_key: Some("builtin-test-key".to_string()),
                ..codewhale_runtime::config::ProviderConfig::default()
            },
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_5_value: Config = Config::default();
        __cw_config_init_5_value.provider = __cw_config_init_5_0;
        __cw_config_init_5_value.providers = __cw_config_init_5_1;
        __cw_config_init_5_value
    };
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        ..EngineConfig::default()
    };
    let (mut engine, handle) = Engine::new(engine_config, &config);

    let mut builtin_config = config.clone();
    builtin_config.provider = Some("openai".to_string());
    let builtin_route =
        resolve_runtime_route(&builtin_config, ProviderKind::Openai, Some("gpt-5.5"))
            .expect("resolve intervening builtin route")
            .validate()
            .expect("preflight intervening builtin route");
    engine.install_validated_runtime_route(builtin_route);
    assert_eq!(engine.api_provider, ProviderKind::Openai);
    assert_eq!(
        engine
            .codewhale_client
            .as_ref()
            .expect("builtin client")
            .base_url(),
        "http://127.0.0.1:18182/v1"
    );

    let run_task = tokio::spawn(engine.run());
    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "verify exact route".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: Box::new(
                resolve_runtime_route(&config, ProviderKind::Custom, Some("local-model"))
                    .expect("resolve exact custom route"),
            ),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: true,
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
        .expect("send exact custom turn");

    // This test runs alongside more than ten thousand TUI tests in the release
    // parity job. Keep the assertion bounded, but leave enough headroom for a
    // saturated shared runner to schedule the loopback SSE response.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut lifecycle_stage = 0u8;
    let mut diagnostics = Vec::new();
    let mut rx = handle.rx_event.write().await;
    loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for semantic route sequence: {diagnostics:?}")
            })
            .expect("engine event channel closed before terminal route receipt");
        diagnostics.push(match &event {
            Event::TurnStarted { .. } => "turn_started",
            Event::RouteDispatched { .. } => "route_dispatched",
            Event::TurnComplete { .. } => "turn_complete",
            Event::SessionUpdated { .. } => "session_updated",
            Event::PrefixCacheChange { .. } => "prefix_cache",
            Event::Status { .. } => "status",
            _ => "other",
        });
        match event {
            Event::TurnStarted { route, .. } => {
                assert_eq!(
                    lifecycle_stage, 0,
                    "duplicate/reordered start: {diagnostics:?}"
                );
                // Lifecycle start still carries the installed-route receipt
                // hosts authorize follow-up work against, but it must carry no
                // billing envelope: nothing has been dispatched yet, and an
                // undispatched route has no metering surface or billing time.
                assert!(
                    route.as_ref().is_none_or(|route| route.billing.is_none()),
                    "billing route must not be stamped at lifecycle start"
                );
                lifecycle_stage = 1;
            }
            Event::RouteDispatched { route, .. } => {
                assert_eq!(
                    lifecycle_stage, 1,
                    "dispatch missing, duplicated, or reordered: {diagnostics:?}"
                );
                assert_eq!(route.provider, ProviderKind::Custom);
                assert_eq!(route.provider_identity, "custom-a");
                assert_eq!(route.model, "local-model");
                assert_eq!(
                    route
                        .billing
                        .as_ref()
                        .and_then(|billing| billing.endpoint_fingerprint.clone()),
                    crate::cost_status::endpoint_fingerprint(&custom_base_url),
                    "dispatch receipt borrowed the later ambient route"
                );
                lifecycle_stage = 2;
            }
            Event::TurnComplete { base_url, .. } => {
                assert_eq!(
                    lifecycle_stage, 2,
                    "terminal arrived without an ordered dispatch receipt: {diagnostics:?}"
                );
                assert_eq!(base_url.as_deref(), Some(custom_base_url.as_str()));
                lifecycle_stage = 3;
                break;
            }
            _ => {}
        }
    }
    drop(rx);
    assert_eq!(lifecycle_stage, 3);
    assert_eq!(
        custom_server
            .received_requests()
            .await
            .expect("recorded custom-route request")
            .len(),
        1,
        "semantic dispatch sequence must bracket one real provider request"
    );
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// #6690: the main interactive turn froze its dispatch quote only from the
/// provider lake, so an operator's `[[custom_models]]` rate never priced it
/// even though background/review envelopes honored the same row.
#[tokio::test]
async fn main_turn_dispatch_freezes_declared_custom_model_rate() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let custom_server = MockServer::start().await;
    let custom_base_url = format!("{}/v1", custom_server.uri());
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-declared-rate\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"priced\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-declared-rate\",\"choices\":[{\"index\":0,",
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
        .mount(&custom_server)
        .await;

    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some(custom_base_url.clone()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let declared: codewhale_config::catalog::configured::ConfiguredModel =
        toml::from_str(&format!(
            "provider = \"custom-a\"\nbase_url = \"{custom_base_url}\"\n\
             id = \"local-model\"\ncost = {{ input = 1.0, output = 2.0 }}\n"
        ))
        .expect("declared model row");
    let config = {
        let __cw_config_init_6_0 = Some("custom-a".to_string());
        let __cw_config_init_6_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let __cw_config_init_6_2 = Some(vec![declared]);
        let mut __cw_config_init_6_value: Config = Config::default();
        __cw_config_init_6_value.provider = __cw_config_init_6_0;
        __cw_config_init_6_value.providers = __cw_config_init_6_1;
        __cw_config_init_6_value.custom_models = __cw_config_init_6_2;
        __cw_config_init_6_value
    };
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new(engine_config, &config);
    let run_task = tokio::spawn(engine.run());
    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "price this turn".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: Box::new(
                resolve_runtime_route(&config, ProviderKind::Custom, Some("local-model"))
                    .expect("resolve declared custom route"),
            ),
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
        .expect("send declared custom turn");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut rx = handle.rx_event.write().await;
    let quote = loop {
        let event = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for route dispatch")
            .expect("engine event channel closed before dispatch");
        if let Event::RouteDispatched { route, .. } = event {
            break route
                .billing
                .and_then(|billing| billing.provider_live_pricing)
                .expect("declared custom_models rate must be frozen at main-turn dispatch");
        }
    };
    drop(rx);
    assert_eq!(
        quote.provenance,
        codewhale_config::pricing::PricingProvenance::UserOverride
    );
    assert_eq!(quote.input_per_million.as_deref(), Some("1"));
    assert_eq!(quote.output_per_million.as_deref(), Some("2"));
    assert_eq!(quote.cache_read_per_million, None);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

struct GatedGoalModelClient {
    calls: std::sync::atomic::AtomicUsize,
    requests: std::sync::Mutex<Vec<codewhale_models::MessageRequest>>,
    second_request_entered: std::sync::Arc<tokio::sync::Notify>,
    release_second_request: std::sync::Arc<tokio::sync::Notify>,
    first_usage: Option<Usage>,
}

struct FirstRequestGatedGoalModelClient {
    calls: std::sync::atomic::AtomicUsize,
    request_entered: std::sync::Arc<tokio::sync::Notify>,
    release_request: std::sync::Arc<tokio::sync::Notify>,
}

struct IndexedGatedGoalModelClient {
    calls: std::sync::atomic::AtomicUsize,
    gates: HashMap<
        usize,
        (
            std::sync::Arc<tokio::sync::Notify>,
            std::sync::Arc<tokio::sync::Notify>,
        ),
    >,
    max_calls: usize,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for IndexedGatedGoalModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-goal"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("indexed gate regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if call > self.max_calls {
            anyhow::bail!("unexpected indexed goal model request #{call}");
        }
        if let Some((entered, release)) = self.gates.get(&call).cloned() {
            entered.notify_one();
            release.notified().await;
        }

        let events = crate::llm_client::mock::canned::simple_text_turn("still working")
            .into_iter()
            .map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for FirstRequestGatedGoalModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-goal"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("mailbox regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if call > 1 {
            anyhow::bail!("unexpected mailbox regression model request #{call}");
        }
        self.request_entered.notify_one();
        self.release_request.notified().await;

        let events = crate::llm_client::mock::canned::simple_text_turn("still working")
            .into_iter()
            .map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct FailingGoalModelClient {
    calls: std::sync::atomic::AtomicUsize,
    message: String,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for FailingGoalModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-goal"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("failure regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!(self.message.clone())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

impl GatedGoalModelClient {
    fn captured_requests(&self) -> Vec<codewhale_models::MessageRequest> {
        self.requests
            .lock()
            .expect("goal model request lock")
            .clone()
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for GatedGoalModelClient {
    fn provider_name(&self) -> &str {
        "deterministic-goal"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("goal regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        self.requests
            .lock()
            .expect("goal model request lock")
            .push(request);
        if call == 2 {
            self.second_request_entered.notify_one();
            self.release_second_request.notified().await;
        } else if call > 2 {
            anyhow::bail!("unexpected goal model request #{call}");
        }

        let mut events = crate::llm_client::mock::canned::simple_text_turn("still working");
        if call == 1
            && let Some(usage) = self.first_usage.clone()
            && let Some(codewhale_models::StreamEvent::MessageDelta { usage: slot, .. }) = events
                .iter_mut()
                .find(|event| matches!(event, codewhale_models::StreamEvent::MessageDelta { .. }))
        {
            *slot = Some(usage);
        }
        let events = events.into_iter().map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn goal_continuation_preserves_goal_and_resolves_updated_authoritative_route() {
    let first_base_url = "http://127.0.0.1:18181/v1".to_string();
    let second_base_url = "http://127.0.0.1:18182/v1".to_string();
    let second_request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_second_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(GatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        requests: std::sync::Mutex::new(Vec::new()),
        second_request_entered: std::sync::Arc::clone(&second_request_entered),
        release_second_request: std::sync::Arc::clone(&release_second_request),
        first_usage: Some(Usage {
            input_tokens: 3,
            output_tokens: 2,
            ..Usage::default()
        }),
    });
    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some(first_base_url.clone()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_7_0 = Some("custom-a".to_string());
        let __cw_config_init_7_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.provider = __cw_config_init_7_0;
        __cw_config_init_7_value.providers = __cw_config_init_7_1;
        __cw_config_init_7_value
    };
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("keep going".to_string()),
        goal_token_budget: Some(50_000),
        ..EngineConfig::default()
    };
    let authoritative = Arc::new(parking_lot::RwLock::new(config.clone()));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    engine.authoritative_route_config = Some(Arc::clone(&authoritative));
    let goal_state = engine.config.goal_state.clone();

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "first turn".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("keep going".to_string()),
            goal_token_budget: Some(50_000),
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
        .expect("send first goal turn");

    let mut reloaded = config;
    reloaded
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("custom-a"))
        .expect("custom route")
        .base_url = Some(second_base_url.clone());
    *authoritative.write() = reloaded;
    let refreshed_route = engine
        .current_runtime_route()
        .expect("resolve the updated authoritative route");
    assert_eq!(
        refreshed_route.candidate.endpoint().base_url,
        second_base_url,
        "the synthetic continuation must resolve the latest authoritative endpoint"
    );
    let run_task = tokio::spawn(engine.run());

    let mut lifecycle_starts = 0;
    let mut dispatches = 0;
    let mut completes = 0;
    let mut awaiting_second_sync = false;
    let mut verified_synthetic_goal = false;
    while completes < 2 {
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("goal engine event timeout")
        .expect("goal engine event");
        match event {
            Event::TurnStarted { route, .. } => {
                assert!(
                    route.as_ref().is_none_or(|route| route.billing.is_none()),
                    "lifecycle start must not carry billing time"
                );
                lifecycle_starts += 1;
                if lifecycle_starts == 2 {
                    awaiting_second_sync = true;
                }
            }
            Event::RouteDispatched { route, .. } => {
                dispatches += 1;
                assert_eq!(route.provider_identity, "custom-a");
                let expected_base_url = if dispatches == 1 {
                    first_base_url.as_str()
                } else {
                    second_base_url.as_str()
                };
                assert_eq!(
                    route
                        .billing
                        .as_ref()
                        .and_then(|billing| billing.endpoint_fingerprint.clone()),
                    crate::cost_status::endpoint_fingerprint(expected_base_url),
                    "goal continuation dispatch borrowed the wrong authoritative route"
                );
            }
            Event::SessionUpdated {
                messages,
                system_prompt,
                ..
            } if awaiting_second_sync => {
                awaiting_second_sync = false;
                let snapshot = goal_state.lock().expect("goal lock").snapshot();
                assert_eq!(snapshot.objective.as_deref(), Some("keep going"));
                assert_eq!(snapshot.token_budget, Some(50_000));
                // The first turn records one bounded intra-turn pass, then the
                // synthetic boundary records the second pass before dispatch.
                assert_eq!(snapshot.continuation_count, 2);
                assert!(snapshot.is_active(), "synthetic turn must retain the goal");

                let continuation = messages
                    .last()
                    .expect("synthetic continuation message")
                    .content
                    .iter()
                    .find_map(|block| match block {
                        ContentBlock::Text { text, .. }
                            if text.contains("## Active Goal State") =>
                        {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .expect("durable goal state in synthetic message");
                assert!(continuation.contains("\"objective\": \"keep going\""));
                assert!(continuation.contains("\"token_budget\": 50000"));
                assert!(continuation.contains("\"continuation_count\": 2"));
                assert!(continuation.contains("Continuation pass #2."));

                let system_prompt = match system_prompt.expect("synthetic system prompt") {
                    SystemPrompt::Text(text) => text,
                    SystemPrompt::Blocks(blocks) => blocks
                        .into_iter()
                        .map(|block| block.text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                assert!(system_prompt.contains("<session_goal>"));
                assert!(system_prompt.contains("keep going"));
                verified_synthetic_goal = true;

                tokio::time::timeout(
                    model_turn_event_timeout(),
                    second_request_entered.notified(),
                )
                .await
                .expect("second goal model request was never entered");
                handle
                    .send(Op::SetGoalStatus {
                        goal_id: None,
                        status: crate::tools::goal::GoalStatus::Paused,
                        clear: false,
                    })
                    .await
                    .expect("queue goal pause");
                // The model future cannot finish until the pause operation is
                // already in the engine mailbox, making the queue-order proof
                // deterministic under arbitrarily loaded CI runners.
                release_second_request.notify_one();
            }
            Event::TurnComplete { base_url, .. } => {
                completes += 1;
                assert!(
                    base_url.is_none(),
                    "an injected provider-neutral transport must not claim the auxiliary route's endpoint"
                );
            }
            _ => {}
        }
    }
    assert_eq!(lifecycle_starts, 2);
    assert_eq!(dispatches, 2);
    assert!(verified_synthetic_goal);
    let requests = model.captured_requests();
    assert_eq!(requests.len(), 2);
    let first_intra_turn_prompt = requests[1]
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } if text.contains("Continuation pass #1.") => {
                Some(text.as_str())
            }
            _ => None,
        })
        .expect("first intra-turn goal snapshot must survive into the next request");
    assert!(
        first_intra_turn_prompt.contains("\"tokens_used\": 5"),
        "current-turn usage must be rendered without waiting for durable recording: {first_intra_turn_prompt}"
    );

    // The pause was queued while the second turn was still running. Wait for
    // that control operation, then put a snapshot receipt behind the already
    // queued continuation. Receiving the receipt proves the continuation was
    // consumed; no third TurnStarted may have been emitted.
    let mut saw_paused_prompt = false;
    let mut saw_paused_goal = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("goal pause event timeout")
        .expect("goal pause event");
        match event {
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
                if !prompt.contains("<session_goal>") {
                    saw_paused_prompt = true;
                }
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                assert_eq!(snapshot.objective.as_deref(), Some("keep going"));
                saw_paused_goal = true;
            }
            Event::Status { ref message } if message == "Goal paused." => {
                assert!(
                    saw_paused_prompt,
                    "pause status must follow the persisted prompt refresh"
                );
                assert!(
                    saw_paused_goal,
                    "pause status must follow the visible goal snapshot"
                );
                break;
            }
            Event::TurnStarted { .. } => {
                panic!("queued pause must prevent an additional goal turn")
            }
            _ => {}
        }
    }

    let (snapshot_tx, snapshot_rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(snapshot_tx))),
        })
        .await
        .expect("queue post-continuation receipt");
    tokio::time::timeout(Duration::from_secs(3), snapshot_rx)
        .await
        .expect("post-continuation receipt timeout")
        .expect("post-continuation receipt");
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, Event::TurnStarted { .. }),
                "paused goal continuation started a stale turn"
            );
        }
    }

    handle.send(Op::Shutdown).await.expect("queue shutdown");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn saturated_mailbox_does_not_deadlock_goal_continuation_self_dispatch() {
    let request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(FirstRequestGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        request_entered: std::sync::Arc::clone(&request_entered),
        release_request: std::sync::Arc::clone(&release_request),
    });
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        max_steps: 1,
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("survive a saturated mailbox".to_string()),
        ..EngineConfig::default()
    };
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "start the saturated goal turn".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("survive a saturated mailbox".to_string()),
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
        .expect("send saturated goal turn");
    tokio::time::timeout(model_turn_event_timeout(), request_entered.notified())
        .await
        .expect("first goal request was never entered");

    // The engine has consumed the SendMessage and is gated inside the model
    // request, so every slot below belongs to a queued control operation. The
    // final pause must remain ahead of the synthetic continuation.
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
            .unwrap_or_else(|error| panic!("fill op mailbox slot {index}: {error}"));
    }
    assert_eq!(handle.tx_op.capacity(), 0, "fixture must saturate mailbox");

    release_request.notify_one();
    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("saturated mailbox deadlocked the engine")
        .expect("post-saturation session snapshot");

    let prompt = match session.system_prompt.expect("paused system prompt") {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(!prompt.contains("<session_goal>"), "{prompt}");
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.status, "paused");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the queued pause must suppress the stale continuation"
    );

    let mut starts = 0;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            if matches!(event, Event::TurnStarted { .. }) {
                starts += 1;
            }
        }
    }
    assert_eq!(starts, 1, "only the original goal turn may start");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after mailbox saturation")
        .expect("engine task");
}

#[tokio::test]
async fn queued_ordinary_turn_does_not_multiply_engine_goal_continuations() {
    let first_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_first = std::sync::Arc::new(tokio::sync::Notify::new());
    let third_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_third = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(IndexedGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        gates: HashMap::from([
            (
                1,
                (
                    std::sync::Arc::clone(&first_entered),
                    std::sync::Arc::clone(&release_first),
                ),
            ),
            (
                3,
                (
                    std::sync::Arc::clone(&third_entered),
                    std::sync::Arc::clone(&release_third),
                ),
            ),
        ]),
        max_calls: 3,
    });
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        max_steps: 1,
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("coalesce queued goal turns".to_string()),
        ..EngineConfig::default()
    };
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());
    let send_message = |content: &str| {
        Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: content.to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("coalesce queued goal turns".to_string()),
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
        })
    };

    handle
        .send(send_message("start the goal turn"))
        .await
        .expect("send first goal turn");
    tokio::time::timeout(model_turn_event_timeout(), first_entered.notified())
        .await
        .expect("first goal request was never entered");

    // This ordinary user turn is already ahead of the first synthetic token
    // when the gated turn completes. It may refresh that token's tools, but it
    // must not create a second autonomous continuation.
    handle
        .send(send_message("queued ordinary follow-up"))
        .await
        .expect("queue ordinary follow-up");
    release_first.notify_one();

    tokio::time::timeout(model_turn_event_timeout(), third_entered.notified())
        .await
        .expect("coalesced synthetic continuation was never entered");
    handle
        .send(Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Paused,
            clear: false,
        })
        .await
        .expect("queue goal pause behind synthetic turn");
    release_third.notify_one();

    let _session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("queued-turn coalescing did not settle")
        .expect("post-coalescing session snapshot");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "one initial turn, one queued user turn, and one synthetic continuation are expected"
    );
    assert_eq!(
        goal_state.lock().expect("goal lock").snapshot().status,
        "paused"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after queued-turn coalescing")
        .expect("engine task");
}

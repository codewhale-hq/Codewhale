#[tokio::test]
async fn host_managed_engine_defers_idle_subagent_completion_to_explicit_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::subagent::SubAgentCompletion;

    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..codewhale_runtime::config::ProviderConfig::default()
        },
    );
    let config = {
        let __cw_config_init_0_0 = Some("custom-a".to_string());
        let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..codewhale_runtime::config::ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    };
    let runtime_services = crate::tools::spec::RuntimeToolServices {
        active_thread_id: Some("thr_host_managed".to_string()),
        ..crate::tools::spec::RuntimeToolServices::default()
    };
    let mut engine_config = EngineConfig {
        max_steps: 0,
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        runtime_services,
        ..EngineConfig::default()
    };
    engine_config.features.disable(Feature::Mcp);
    // This branch grants a zero-step host turn exactly one A2 report request
    // (see host_managed_engine_does_not_self_dispatch_goal_continuation,
    // which asserts that single dispatch). Script the response the same way
    // so the turn completes deterministically instead of dialing the loopback
    // route fixture that nothing serves in this test.
    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "drained host turn",
    )]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let owner_session_id = engine.session.id.clone();
    let tx_subagent_completion = engine.tx_subagent_completion.clone();
    let run_task = tokio::spawn(engine.run());

    tx_subagent_completion
        .try_send(SubAgentCompletion {
            owner_session_id,
            agent_id: "agent_deferred".to_string(),
            payload: "deferred child result".to_string(),
        })
        .expect("queue sub-agent completion");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .is_err(),
        "an idle child completion must not create an unclaimed hosted turn"
    );

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "claim the next turn".to_string(),
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
        .expect("send explicit host turn");

    let mut starts = 0;
    let mut drained_completion = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("host engine event timeout")
        .expect("host engine event");
        match event {
            Event::TurnStarted { .. } => starts += 1,
            Event::Status { message } => {
                drained_completion |= message.contains("1 queued sub-agent completion");
            }
            Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    assert_eq!(starts, 1);
    assert!(
        drained_completion,
        "the next explicit turn must drain the queued child completion"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// A host-submitted turn echoes the correlation token it was submitted with,
/// while a runtime self-started turn (idle sub-agent completion resume) never
/// carries one. With this contract a host can tell "my pending submission
/// started" apart from "an autonomous follow-up overtook it in the event
/// stream" and only ever consume a deferred submit-window action on the
/// former.
#[tokio::test]
async fn turn_started_echoes_submission_id_and_self_starts_stay_none() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::subagent::SubAgentCompletion;

    let workspace = tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("submitted turn finished."),
        canned::simple_text_turn("self-started continuation finished."),
    ]));
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        mock.clone(),
    );
    let owner_session_id = engine.session.id.clone();
    let completion_tx = engine.tx_subagent_completion.clone();
    let mut op = external_user_message_op("Watch the child agent.", AppMode::Agent, &config);
    if let Op::SendMessage(TurnSpec { submission_id, .. }) = &mut op {
        *submission_id = Some("sub-host-1".to_string());
    }
    let run_task = tokio::spawn(engine.run());
    handle.send(op).await.expect("send correlated turn");

    // The submitted turn's start echoes the token verbatim.
    let submitted_turn_id = {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the submitted turn")
                .expect("engine event");
            if let Event::TurnStarted {
                turn_id,
                submission_id,
                ..
            } = event
            {
                assert_eq!(
                    submission_id.as_deref(),
                    Some("sub-host-1"),
                    "the submitted turn's TurnStarted must echo its correlation token"
                );
                break turn_id;
            }
        }
    };
    {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the submitted turn to complete")
                .expect("engine event");
            if let Event::TurnComplete {
                turn_id,
                submission_id,
                compaction_id,
                status,
                error,
                ..
            } = event
            {
                assert_eq!(turn_id.as_deref(), Some(submitted_turn_id.as_str()));
                assert_eq!(
                    submission_id.as_deref(),
                    Some("sub-host-1"),
                    "the submitted turn's terminal must echo its correlation token"
                );
                assert!(compaction_id.is_none());
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
        }
    }

    // An idle child completion self-starts the follow-up without any host
    // submission; its start must not present a token.
    completion_tx
        .try_send(SubAgentCompletion {
            owner_session_id,
            agent_id: "idle-child".to_string(),
            payload: "child finished its work".to_string(),
        })
        .expect("inject idle sub-agent completion");
    let self_started_turn_id = {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the self-started turn")
                .expect("engine event");
            if let Event::TurnStarted {
                turn_id,
                submission_id,
                ..
            } = event
            {
                assert_ne!(
                    turn_id, submitted_turn_id,
                    "the idle child completion must self-start a new turn"
                );
                assert!(
                    submission_id.is_none(),
                    "a runtime self-started turn must not present a submission id"
                );
                break turn_id;
            }
        }
    };
    {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the self-started turn to complete")
                .expect("engine event");
            if let Event::TurnComplete {
                turn_id,
                submission_id,
                compaction_id,
                status,
                error,
                ..
            } = event
            {
                assert_eq!(turn_id.as_deref(), Some(self_started_turn_id.as_str()));
                assert!(
                    submission_id.is_none(),
                    "autonomous terminal must not inherit the preceding host token"
                );
                assert!(compaction_id.is_none());
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
        }
    }

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert_eq!(mock.call_count(), 2);
}

/// A composer shell turn never reaches the model; its terminal must still
/// carry its own turn id and nothing else — no host submission and no
/// compaction id.
#[tokio::test]
async fn composer_shell_terminal_carries_its_own_turn_and_nothing_else() {
    use crate::llm_client::mock::MockLlmClient;

    let workspace = tempdir().unwrap();
    // The composer shell turn never reaches the model; the mock has no canned
    // responses so any accidental dispatch fails loudly instead of passing.
    let mock = Arc::new(MockLlmClient::new(vec![]));
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(Op::RunShellCommand {
            command: "echo composer-shell-terminal-attribution".to_string(),
            mode: AppMode::Agent,
            allow_shell: true,
            trust_mode: false,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
        })
        .await
        .expect("queue composer shell command");
    {
        let mut rx = handle.rx_event.write().await;
        let started_turn_id = loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the composer shell turn")
                .expect("engine event");
            if let Event::TurnStarted {
                turn_id,
                submission_id,
                ..
            } = event
            {
                assert!(
                    submission_id.is_none(),
                    "a composer shell turn has no host submission to echo"
                );
                break turn_id;
            }
        };
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the composer shell completion")
                .expect("engine event");
            if let Event::TurnComplete {
                turn_id,
                submission_id,
                compaction_id,
                status,
                error,
                ..
            } = event
            {
                assert_eq!(turn_id.as_deref(), Some(started_turn_id.as_str()));
                assert!(
                    submission_id.is_none(),
                    "a composer shell terminal must not present a submission id"
                );
                assert!(compaction_id.is_none());
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
    assert_eq!(mock.call_count(), 0);
}

#[test]
fn idle_and_in_turn_subagent_delivery_claim_each_completion_once() {
    use crate::tools::subagent::SubAgentCompletion;

    let mut delivered = HashSet::new();
    let first = SubAgentCompletion {
        owner_session_id: "session-a".to_string(),
        agent_id: "agent_same".to_string(),
        payload: "first delivery".to_string(),
    };
    let duplicate = SubAgentCompletion {
        owner_session_id: "session-a".to_string(),
        agent_id: "agent_same".to_string(),
        payload: "duplicate delivery".to_string(),
    };
    let second = SubAgentCompletion {
        owner_session_id: "session-a".to_string(),
        agent_id: "agent_other".to_string(),
        payload: "other delivery".to_string(),
    };

    assert!(claim_subagent_completion(&mut delivered, first).is_some());
    assert!(claim_subagent_completion(&mut delivered, duplicate).is_none());
    assert!(claim_subagent_completion(&mut delivered, second).is_some());
    assert_eq!(
        delivered,
        HashSet::from(["agent_same".to_string(), "agent_other".to_string()])
    );
}

#[tokio::test]
async fn session_switch_drops_old_completion_before_deduplication() {
    use crate::tools::subagent::SubAgentCompletion;

    let workspace = tempdir().expect("tempdir");
    let (mut engine, _handle) = Engine::new(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
    );
    engine.session.id = "session-new".to_string();
    let messages_before = engine.session.messages.len();

    engine
        .handle_idle_subagent_completion(SubAgentCompletion {
            owner_session_id: "session-old".to_string(),
            agent_id: "agent_same".to_string(),
            payload: "foreign task state".to_string(),
        })
        .await;

    assert_eq!(engine.session.messages.len(), messages_before);
    assert!(engine.delivered_subagent_completion_ids.is_empty());
    assert!(
        claim_subagent_completion_for_session(
            &mut engine.delivered_subagent_completion_ids,
            "session-new",
            SubAgentCompletion {
                owner_session_id: "session-new".to_string(),
                agent_id: "agent_same".to_string(),
                payload: "current task state".to_string(),
            },
        )
        .is_some(),
        "the rejected foreign completion must not suppress the same id in the active session"
    );
}

#[tokio::test]
async fn idle_subagent_delivery_releases_claim_when_route_fails_before_recording() {
    use crate::tools::subagent::SubAgentCompletion;

    let workspace = tempdir().expect("tempdir");
    let api_config = Config::default()
    .with_legacy_root(
        Some("test-key".to_string()),
        Some("http://127.0.0.1:1/v1".to_string()),
    );
    let (mut engine, _handle) =
        Engine::new(deterministic_engine_config(workspace.path()), &api_config);
    // Make the persisted exact identity structurally unresolvable. The
    // completion is claimed before route resolution, so this exercises the
    // early error branch before a transcript record can be written.
    engine.api_provider = ProviderKind::Custom;
    engine.api_provider_identity = None;

    engine
        .handle_idle_subagent_completion(SubAgentCompletion {
            owner_session_id: engine.session.id.clone(),
            agent_id: "agent_retryable".to_string(),
            payload: "completed work".to_string(),
        })
        .await;

    assert!(
        !engine
            .delivered_subagent_completion_ids
            .contains("agent_retryable"),
        "a completion that never reached the transcript must remain retryable"
    );
    let owner_session_id = engine.session.id.clone();
    assert!(
        claim_subagent_completion(
            &mut engine.delivered_subagent_completion_ids,
            SubAgentCompletion {
                owner_session_id,
                agent_id: "agent_retryable".to_string(),
                payload: "retry".to_string(),
            },
        )
        .is_some()
    );
}

#[test]
fn subagent_mailbox_keeps_lifecycle_events_reliable() {
    use crate::tools::subagent::MailboxMessage;
    use codewhale_models::Usage;

    assert!(subagent_mailbox_message_is_best_effort(
        &MailboxMessage::progress("agent_a", "step 1")
    ));
    assert!(subagent_mailbox_message_is_best_effort(
        &MailboxMessage::ToolCallStarted {
            agent_id: "agent_a".to_string(),
            tool_name: "read_file".to_string(),
            step: 1,
        }
    ));
    assert!(subagent_mailbox_message_is_best_effort(
        &MailboxMessage::ToolCallCompleted {
            agent_id: "agent_a".to_string(),
            tool_name: "read_file".to_string(),
            step: 1,
            ok: true,
        }
    ));

    assert!(!subagent_mailbox_message_is_best_effort(
        &MailboxMessage::started("agent_a", codewhale_agent_contract::role::FleetRole::Scout)
    ));
    assert!(!subagent_mailbox_message_is_best_effort(
        &MailboxMessage::Completed {
            agent_id: "agent_a".to_string(),
            summary: "done".to_string(),
        }
    ));
    assert!(!subagent_mailbox_message_is_best_effort(
        &MailboxMessage::Failed {
            agent_id: "agent_a".to_string(),
            error: "failed".to_string(),
        }
    ));
    assert!(!subagent_mailbox_message_is_best_effort(
        &MailboxMessage::TokenUsage {
            agent_id: "agent_a".to_string(),
            source_id: "response-a".to_string(),
            route: Box::new(crate::cost_status::EffectiveRouteEnvelope::capture(
                None,
                ProviderKind::Deepseek,
                "deepseek",
                "model",
                Some(ProviderKind::Deepseek.provider().default_base_url()),
                chrono::Utc::now(),
            )),
            usage: Usage::default(),
        }
    ));
}

#[test]
fn subagent_mailbox_samples_best_effort_events_per_agent() {
    use crate::tools::subagent::MailboxMessage;

    let mut last_sent_at = HashMap::new();
    let start = Instant::now();
    let first = MailboxMessage::ToolCallStarted {
        agent_id: "agent_a".to_string(),
        tool_name: "exec_shell".to_string(),
        step: 1,
    };
    let second = MailboxMessage::ToolCallCompleted {
        agent_id: "agent_a".to_string(),
        tool_name: "exec_shell".to_string(),
        step: 1,
        ok: true,
    };
    let other_agent = MailboxMessage::ToolCallCompleted {
        agent_id: "agent_b".to_string(),
        tool_name: "exec_shell".to_string(),
        step: 1,
        ok: true,
    };

    assert!(subagent_mailbox_best_effort_send_permitted(
        &mut last_sent_at,
        &first,
        start,
    ));
    assert!(
        !subagent_mailbox_best_effort_send_permitted(
            &mut last_sent_at,
            &second,
            start + Duration::from_millis(10),
        ),
        "same-agent telemetry inside the sampling window is dropped"
    );
    assert!(
        subagent_mailbox_best_effort_send_permitted(
            &mut last_sent_at,
            &other_agent,
            start + Duration::from_millis(10),
        ),
        "sampling is per agent, so one busy child cannot hide another"
    );
    assert!(
        subagent_mailbox_best_effort_send_permitted(
            &mut last_sent_at,
            &second,
            start + SUBAGENT_MAILBOX_BEST_EFFORT_MIN_INTERVAL,
        ),
        "the next same-agent update is allowed after the interval"
    );
}

#[test]
fn subagent_mailbox_never_samples_lifecycle_or_usage_events() {
    use crate::tools::subagent::MailboxMessage;
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_models::Usage;

    let mut last_sent_at = HashMap::new();
    let start = Instant::now();

    assert!(subagent_mailbox_best_effort_send_permitted(
        &mut last_sent_at,
        &MailboxMessage::started("agent_a", FleetRole::Scout),
        start,
    ));
    assert!(subagent_mailbox_best_effort_send_permitted(
        &mut last_sent_at,
        &MailboxMessage::Completed {
            agent_id: "agent_a".to_string(),
            summary: "done".to_string(),
        },
        start,
    ));
    assert!(subagent_mailbox_best_effort_send_permitted(
        &mut last_sent_at,
        &MailboxMessage::TokenUsage {
            agent_id: "agent_a".to_string(),
            source_id: "response-a".to_string(),
            route: Box::new(crate::cost_status::EffectiveRouteEnvelope::capture(
                None,
                ProviderKind::Deepseek,
                "deepseek",
                "model",
                Some(ProviderKind::Deepseek.provider().default_base_url()),
                chrono::Utc::now(),
            )),
            usage: Usage::default(),
        },
        start,
    ));
}

struct ScopedDeepSeekApiKey {
    previous: Option<OsString>,
}

impl ScopedDeepSeekApiKey {
    fn set(value: &str) -> Self {
        let previous = std::env::var_os("DEEPSEEK_API_KEY");
        // Safety: tests using this helper serialize with lock_test_env() and
        // restore the original value in Drop.
        unsafe {
            std::env::set_var("DEEPSEEK_API_KEY", value);
        }
        Self { previous }
    }
}

impl Drop for ScopedDeepSeekApiKey {
    fn drop(&mut self) {
        // Safety: tests using this helper serialize with lock_test_env().
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("DEEPSEEK_API_KEY", previous);
            } else {
                std::env::remove_var("DEEPSEEK_API_KEY");
            }
        }
    }
}

fn catalog_tool(name: &str) -> Tool {
    Tool {
        tool_type: None,
        name: name.to_string(),
        description: String::new(),
        input_schema: json!({"type": "object"}),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }
}

#[test]
fn shell_denial_filters_search_catalog_without_expanding_allow_grants() {
    let raw_names = [
        "bash",
        "Bash",
        "exec_shell",
        "task_shell_start",
        "task_gate_run",
        "terminal/run",
        "terminal/send",
        "terminal/reset",
        "exec_shell_interact",
        "exec_interact",
        "code_execution",
        "js_execution",
        "rlm_eval",
        "start_mcp_server",
        "start_registry_mcp_server",
    ];
    for rule in ["Bash", "eXeC_sHeLl", "baSH*", "exec_shell*"] {
        let surface = policy_for_catalog(
            raw_names.into_iter().map(catalog_tool).collect(),
            None,
            Some(vec![rule.into()]),
        );
        for name in raw_names {
            assert!(surface.denies_call(name, &json!({})), "{rule}: {name}");
            assert!(
                surface.catalog.iter().all(|tool| tool.name != name),
                "{rule}: {name}"
            );
        }
    }
    let allowed = policy_for_catalog(
        raw_names.into_iter().map(catalog_tool).collect(),
        Some(vec!["Bash".into()]),
        None,
    );
    for name in raw_names.into_iter().skip(3) {
        assert!(
            !allowed.passes_allow_list(name),
            "Bash must not grant {name}"
        );
    }
}

#[test]
fn shell_denial_preserves_task_reads_and_bounded_verification_actions() {
    let mut tasks = catalog_tool("tasks");
    tasks.input_schema =
        json!({"type":"object", "properties":{"action":{"enum":["list", "read", "gate_run"]}}});
    let surface = policy_for_catalog(
        vec![
            tasks,
            catalog_tool("Run"),
            catalog_tool("task_shell_wait"),
            catalog_tool("terminal/cancel"),
        ],
        None,
        Some(vec!["Bash".into()]),
    );
    let tasks = surface
        .catalog
        .iter()
        .find(|tool| tool.name == "tasks")
        .unwrap();
    assert_eq!(
        tasks.input_schema["properties"]["action"]["enum"],
        json!(["list", "read"])
    );
    assert!(!surface.denies_call("tasks", &json!({"action":"list"})));
    assert!(surface.denies_call("tasks", &json!({"action":"gate_run"})));
    assert!(!surface.denies_call("task_shell_wait", &json!({})));
    assert!(!surface.denies_call("terminal/cancel", &json!({})));
    assert!(!surface.denies_call(
        "Run",
        &json!({"action":"tests", "args":"-p fixture selected_test"})
    ));
    assert!(!surface.denies_call("Run", &json!({"action":"verifiers", "commands":[]})));
    assert!(surface.denies_call(
        "Run",
        &json!({"action":"tests", "args":"--config build.rustc=malicious"})
    ));
    assert!(surface.denies_call(
        "Run",
        &json!({"action":"verifiers", "commands":[{"program":"sh"}]})
    ));
}

#[test]
fn shell_denial_applies_to_new_durable_execution_without_hiding_management() {
    use crate::tools::execution_envelope::tool_call_denied;
    let rules = vec!["Bash".to_string()];
    for (family, actions) in [
        ("tasks", vec!["create", "gate_run"]),
        ("automation", vec!["create", "update", "resume", "run"]),
    ] {
        for action in actions {
            assert!(
                tool_call_denied(Some(&rules), family, &json!({"action": action})),
                "{family}/{action}"
            );
        }
    }
    for (family, actions) in [
        ("tasks", vec!["list", "read", "cancel"]),
        ("automation", vec!["list", "read", "pause", "delete"]),
    ] {
        for action in actions {
            assert!(
                !tool_call_denied(Some(&rules), family, &json!({"action": action})),
                "{family}/{action}"
            );
        }
    }
    let fetch_rules = vec!["fetch_url".to_string()];
    for family in ["rlm", "rlm_open"] {
        assert!(tool_call_denied(
            Some(&fetch_rules),
            family,
            &json!({"action":"open", "url":"https://example.com/document"})
        ));
        assert!(!tool_call_denied(
            Some(&fetch_rules),
            family,
            &json!({"action":"open", "content":"local fixture"})
        ));
    }
}

fn policy_for_catalog(
    catalog: Vec<Tool>,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Option<Vec<String>>,
) -> ToolSurfacePolicy {
    ToolSurfacePolicy::new(
        crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(PathBuf::from("."))),
        Some(catalog),
        AppMode::Agent,
        &HashSet::new(),
        &[],
        false,
        allowed_tools,
        disallowed_tools,
        None,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    )
}

#[test]
fn tool_catalog_scenario() {
    // Scenario consolidation of: tool_catalog_filter_applies_allow_and_deny_gates, tool_catalog_filter_is_inert_without_gates
    // from tool_catalog_filter_applies_allow_and_deny_gates
    {
        // #3027 AC1: the advertised catalog must not contain tools the execution
        // gates would deny; deny wins over allow.
        let catalog = vec![
            catalog_tool("read_file"),
            catalog_tool("exec_shell"),
            catalog_tool("grep_files"),
        ];
        let surface = policy_for_catalog(
            catalog,
            Some(vec!["read_file".to_string(), "exec_shell".to_string()]),
            Some(vec!["exec_shell".to_string()]),
        );
        let names: Vec<&str> = surface.catalog.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read_file"]);
    }
    // from tool_catalog_filter_is_inert_without_gates
    {
        let surface = policy_for_catalog(
            vec![catalog_tool("read_file"), catalog_tool("exec_shell")],
            None,
            None,
        );
        assert!(surface.catalog.iter().any(|tool| tool.name == "read_file"));
        assert!(surface.catalog.iter().any(|tool| tool.name == "exec_shell"));
    }
}

#[test]
fn tool_catalog_shell_only_benchmark_surface_hides_native_tools() {
    let catalog = vec![
        catalog_tool("exec_shell"),
        catalog_tool("exec_shell_wait"),
        catalog_tool("exec_shell_interact"),
        catalog_tool("read_file"),
        catalog_tool("write_file"),
        catalog_tool("list_dir"),
        catalog_tool("git_status"),
        catalog_tool("work_update"),
    ];
    let shell_only = [
        "exec_shell".to_string(),
        "exec_shell_wait".to_string(),
        "exec_shell_interact".to_string(),
    ];

    let surface = policy_for_catalog(catalog, Some(shell_only.to_vec()), None);

    let names: Vec<&str> = surface.catalog.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["exec_shell", "exec_shell_wait", "exec_shell_interact"]
    );
}

#[test]
fn tool_surface_policy_never_reintroduces_denied_synthetic_tools() {
    let denied = vec![
        TOOL_SEARCH_NAME.to_string(),
        CODE_EXECUTION_TOOL_NAME.to_string(),
        JS_EXECUTION_TOOL_NAME.to_string(),
    ];
    let surface = policy_for_catalog(
        vec![
            catalog_tool("read_file"),
            catalog_tool(CODE_EXECUTION_TOOL_NAME),
            catalog_tool(JS_EXECUTION_TOOL_NAME),
        ],
        Some(vec![
            TOOL_SEARCH_NAME.to_string(),
            CODE_EXECUTION_TOOL_NAME.to_string(),
            JS_EXECUTION_TOOL_NAME.to_string(),
        ]),
        Some(denied),
    );

    for denied_name in [
        TOOL_SEARCH_NAME,
        CODE_EXECUTION_TOOL_NAME,
        JS_EXECUTION_TOOL_NAME,
    ] {
        assert!(surface.denies_tool(denied_name));
        assert!(surface.passes_allow_list(denied_name));
        assert!(
            !surface.allows_tool(denied_name),
            "deny must win over allow for {denied_name}"
        );
        assert!(
            surface.catalog.iter().all(|tool| tool.name != denied_name),
            "{denied_name} must not reappear after policy narrowing"
        );
        assert!(!surface.active_names.contains(denied_name));
    }
}

#[tokio::test]
async fn denied_synthetic_tool_is_blocked_by_the_same_turn_policy_at_execution() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-denied-search",
            TOOL_SEARCH_NAME,
            r#"{"query":"File"}"#,
        ),
        canned::simple_text_turn("Denied tool handled."),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock;
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let policy = policy_for_catalog(
        vec![catalog_tool("read_file")],
        Some(vec![TOOL_SEARCH_NAME.to_string()]),
        Some(vec![TOOL_SEARCH_NAME.to_string()]),
    );
    assert!(!policy.allows_tool(TOOL_SEARCH_NAME));
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, policy, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let mut events = handle.rx_event.write().await;
    let denied = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        Event::ToolCallComplete { name, result, .. } if name == TOOL_SEARCH_NAME => Some(result),
        _ => None,
    });
    let error = denied
        .expect("denied synthetic tool completion")
        .expect_err("denied synthetic tool must not execute");
    assert!(
        error.to_string().contains("disallowed-tools list"),
        "{error:?}"
    );
}

#[tokio::test]
async fn healthy_owned_children_do_not_force_another_parent_model_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for max_steps in [1, 4] {
        let workspace = tempdir().expect("tempdir");
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::simple_text_turn("The workflow is running; I will report its result."),
            canned::tool_call_turn("must-not-run", "read_file", r#"{"path":"state.txt"}"#),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let config = EngineConfig {
            max_steps,
            ..deterministic_engine_config(workspace.path())
        };
        let (mut engine, _handle) =
            Engine::new_with_model_client(config, &Config::default(), client);
        let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
            workspace.path().to_path_buf(),
        ));
        let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
        let children = Arc::new(ForegroundChildRegistry::new());
        let child_cancel = CancellationToken::new();
        let registration = children
            .register(child_cancel.clone(), "agent_child")
            .expect("child registered");
        let mut turn = crate::core::turn::TurnContext::new(max_steps);

        let (status, error) = engine
            .run_turn(&mut turn, surface, Some(Arc::clone(&children)), None)
            .await;

        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        assert_eq!(mock.call_count(), 1, "no forced coordination request");
        assert_eq!(mock.remaining_turns(), 1, "extra tool turn remains unused");
        assert_eq!(children.active_count(), 1);
        assert!(!child_cancel.is_cancelled());
        assert!(!engine.session.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Text { text, .. }
                    if text.contains("turn_owned_children_active"))
            })
        }));
        drop(registration);
    }
}

#[tokio::test]
async fn user_steer_during_parent_answer_still_gets_a_reply_with_healthy_children() {
    use crate::core::model_client::ModelClient;
    use crate::llm_client::mock::{MockLlmClient, canned};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SteeredClient {
        mock: Arc<MockLlmClient>,
        requests: StdMutex<Vec<codewhale_models::MessageRequest>>,
        calls: AtomicUsize,
        opening: bool,
        first_events: Vec<StreamEvent>,
        entered: Arc<tokio::sync::Notify>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ModelClient for SteeredClient {
        fn provider_name(&self) -> &str {
            "mock"
        }
        fn model(&self) -> &str {
            "mock-model"
        }
        async fn create_message(
            &self,
            request: codewhale_models::MessageRequest,
        ) -> anyhow::Result<codewhale_models::MessageResponse> {
            ModelClient::create_message(self.mock.as_ref(), request).await
        }
        async fn create_message_stream(
            &self,
            request: codewhale_models::MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            self.requests.lock().unwrap().push(request.clone());
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let drop_signal = DropSignal(Arc::clone(&self.dropped));
                if self.opening {
                    self.entered.notify_one();
                    let _drop_signal = drop_signal;
                    return std::future::pending().await;
                }
                let events = self.first_events.clone();
                let entered = Arc::clone(&self.entered);
                return Ok(Box::pin(async_stream::try_stream! {
                    let _drop_signal = drop_signal;
                    for event in events { yield event; }
                    entered.notify_one();
                    std::future::pending::<()>().await;
                }));
            }
            ModelClient::create_message_stream(self.mock.as_ref(), request).await
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    for phase in ["opening", "silent", "reasoning", "text", "partial_tool"] {
        let workspace = tempdir().expect("tempdir");
        fs::write(workspace.path().join("state.txt"), "steer-proof\n").expect("fixture");
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-steer-read", "read_file", r#"{"path":"state.txt"}"#),
            canned::simple_text_turn("The user-requested evidence is steer-proof."),
        ]));
        let first_events = match phase {
            "reasoning" => vec![
                canned::message_start("interrupted"),
                StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: codewhale_models::ContentBlockStart::Thinking {
                        thinking: String::new(),
                    },
                },
                canned::thinking_delta(0, "unfinished reasoning"),
            ],
            "text" => vec![
                canned::message_start("interrupted"),
                canned::text_block_start(0),
                canned::text_delta(0, "The workflow is still running."),
            ],
            "partial_tool" => vec![
                canned::message_start("interrupted"),
                canned::tool_use_block_start(0, "call-must-not-run", "read_file"),
                canned::tool_input_delta(0, r#"{"path":"state.txt""#),
            ],
            _ => Vec::new(),
        };
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = Arc::new(SteeredClient {
            mock: Arc::clone(&mock),
            requests: StdMutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            opening: phase == "opening",
            first_events,
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
        });
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            client.clone(),
        );
        let mut registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
            workspace.path().to_path_buf(),
        ));
        registry.register(Arc::new(crate::tools::file::ReadFileTool));
        let tools = Some(registry.to_api_tools_with_cache(true));
        let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
        let children = Arc::new(ForegroundChildRegistry::new());
        let child_cancel = CancellationToken::new();
        let registration = children
            .register(child_cancel.clone(), "agent_child")
            .expect("child registered");
        let mut turn = crate::core::turn::TurnContext::new(8);
        let steer = async {
            entered.notified().await;
            let turn_id = handle
                .turn_controls
                .lock()
                .unwrap()
                .active
                .as_ref()
                .map(|control| control.id);
            let (outcome, accepted) = tokio::sync::oneshot::channel();
            handle
                .tx_steer
                .send(handle::SteerInput {
                    replace_pending: false,
                    turn_id,
                    content: "Also read state.txt and include its evidence.".to_string(),
                    outcome: Some(outcome),
                })
                .await
                .expect("steer channel open");
            assert_eq!(accepted.await.unwrap(), handle::SteerOutcome::Accepted);
            assert!(
                dropped.load(Ordering::SeqCst),
                "{phase}: obsolete request must close before steer acceptance"
            );
        };
        let ((status, error), ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                engine.run_turn(&mut turn, surface, Some(Arc::clone(&children)), None),
                steer
            )
        })
        .await
        .expect("steering must preempt an indefinitely silent request");
        assert_eq!(status, TurnOutcomeStatus::Completed, "{phase}: {error:?}");
        assert_eq!(client.calls.load(Ordering::SeqCst), 3, "{phase}");
        let requests = client.requests.lock().unwrap();
        assert!(requests[1].messages.iter().any(|message| message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text, .. } if text.contains("Also read state.txt and include its evidence."))
        })), "{phase}");
        assert!(requests[2].messages.iter().any(|message| message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call-steer-read")
        })), "{phase}");
        assert!(
            requests.iter().all(|request| {
                request.messages.iter().all(|message| {
                    message.content.iter().all(|block| {
                    !matches!(block, ContentBlock::ToolUse { id, .. } if id == "call-must-not-run")
                })
                })
            }),
            "{phase}"
        );
        assert!(
            !serde_json::to_string(&requests[1])
                .unwrap()
                .contains("unfinished reasoning"),
            "{phase}"
        );
        assert_eq!(
            requests[0].system, requests[1].system,
            "{phase}: pinned system prefix changed"
        );
        drop(requests);
        let mut events = handle.rx_event.write().await;
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().all(|event| !matches!(event, Event::ToolCallStarted { model_call: Some(call), .. } if call.provider_id == "call-must-not-run")), "{phase}");
        assert!(events.iter().any(|event| matches!(event, Event::ProviderRequestReceipt { receipt } if receipt["outcome"] == if phase == "opening" { "steered_before_stream" } else { "steered" })), "{phase}");
        if phase == "text" {
            assert!(
                engine
                    .session
                    .messages
                    .iter()
                    .any(|message| message.role == Role::InterruptedAssistant)
            );
        }
        assert_eq!(children.active_count(), 1);
        assert!(!child_cancel.is_cancelled());
        assert_eq!(mock.remaining_turns(), 0);
        drop(registration);
    }
}

/// Compose one assistant turn that proposes `calls` as a single parallel
/// tool-call batch: `(call_id, tool_name, args_json)` per block, in order.
fn tool_batch_turn(calls: &[(&str, &str, &str)]) -> Vec<codewhale_models::StreamEvent> {
    use crate::llm_client::mock::canned;

    let mut events = vec![canned::message_start("mock_tool_batch")];
    for (index, (call_id, tool_name, args_json)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("test batch index fits u32");
        events.push(canned::tool_use_block_start(index, call_id, tool_name));
        events.push(canned::tool_input_delta(index, args_json));
        events.push(canned::block_stop(index));
    }
    events.push(canned::message_delta("tool_use", None));
    events.push(canned::message_stop());
    events
}

/// Drive one engine turn against the scripted `mock` turns with a registry
/// that only serves `read_file`, collecting every `ToolCallComplete` event as
/// `(call_id, result)` in emission order.
async fn run_budgeted_read_turn(
    workspace: &Path,
    max_tool_calls: Option<u32>,
    mock: std::sync::Arc<crate::llm_client::mock::MockLlmClient>,
) -> (
    TurnOutcomeStatus,
    Option<String>,
    Vec<(String, Result<ToolResult, ToolError>)>,
) {
    let mut engine_config = deterministic_engine_config(workspace);
    engine_config.max_tool_calls = max_tool_calls;
    let client: crate::core::model_client::SharedModelClient = mock;
    let (mut engine, handle) =
        Engine::new_with_model_client(engine_config, &Config::default(), client);
    let context = crate::tools::ToolContext::new(workspace.to_path_buf());
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    let mut events = handle.rx_event.write().await;
    let completions = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => Some((model_call.provider_id, result)),
            _ => None,
        })
        .collect::<Vec<_>>();
    (status, error, completions)
}

#[tokio::test]
async fn provider_id_reuse_mints_distinct_execution_identity_in_all_dispatch_modes() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::spec::{ToolCapability, ToolSpec};

    struct IdentityTool {
        parallel: bool,
        observed: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ToolSpec for IdentityTool {
        fn name(&self) -> &str {
            "fixture_identity"
        }
        fn description(&self) -> &str {
            "Record the admitted execution identity."
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
        async fn execute(&self, _: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
            let id = context
                .origin_tool_call_id
                .as_ref()
                .expect("host origin")
                .clone();
            self.observed.lock().unwrap().push(id.clone());
            Ok(ToolResult::success(id))
        }
    }
    for parallel in [false, true] {
        let workspace = tempdir().unwrap();
        let calls = [
            ("reused", "fixture_identity", "{}"),
            ("peer", "fixture_identity", "{}"),
        ];
        let mock = Arc::new(MockLlmClient::new(vec![
            tool_batch_turn(&calls),
            tool_batch_turn(&calls),
            canned::simple_text_turn("done"),
        ]));
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            mock.clone(),
        );
        let observed = Arc::new(StdMutex::new(Vec::new()));
        let mut registry =
            crate::tools::ToolRegistry::new(ToolContext::new(workspace.path().to_path_buf()));
        registry.register(Arc::new(IdentityTool {
            parallel,
            observed: observed.clone(),
        }));
        let tools = Some(registry.to_api_tools_with_cache(true));
        let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(4);
        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        let ids: HashSet<_> = observed.lock().unwrap().iter().cloned().collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()));
        let mut starts = HashMap::new();
        let mut completes = HashMap::new();
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::ToolCallStarted {
                    id,
                    model_call: Some(correlation),
                    ..
                } => {
                    assert!(starts.insert(id, correlation).is_none());
                }
                Event::ToolCallComplete {
                    id,
                    model_call: Some(correlation),
                    result,
                    ..
                } => {
                    assert_eq!(result.unwrap().content, id);
                    assert!(completes.insert(id, correlation).is_none());
                }
                _ => {}
            }
        }
        assert_eq!(starts, completes);
        assert_eq!(starts.keys().cloned().collect::<HashSet<_>>(), ids);
        let requests = mock.captured_requests();
        let history = &requests[2].messages;
        let mut pairs = HashMap::<String, (usize, usize)>::new();
        for block in history.iter().flat_map(|message| &message.content) {
            match block {
                ContentBlock::ToolUse {
                    id,
                    execution_id: Some(local),
                    ..
                } => {
                    assert_eq!(starts[local].provider_id, *id);
                    pairs.entry(local.clone()).or_default().0 += 1;
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    execution_id: Some(local),
                    ..
                } => {
                    assert_eq!(starts[local].provider_id, *tool_use_id);
                    pairs.entry(local.clone()).or_default().1 += 1;
                }
                _ => {}
            }
        }
        assert_eq!(pairs.len(), 4);
        assert!(pairs.values().all(|pair| *pair == (1, 1)));
    }
}

#[tokio::test]
async fn invalid_provider_tool_batch_is_rejected_before_observation_or_history() {
    use crate::llm_client::mock::MockLlmClient;
    for ids in [["duplicate", "duplicate"], ["valid", ""], ["valid", "   "]] {
        let workspace = tempdir().unwrap();
        let mock = Arc::new(MockLlmClient::new(vec![tool_batch_turn(&[
            (ids[0], "read_file", r#"{"path":"never-read"}"#),
            (ids[1], "read_file", r#"{"path":"never-read"}"#),
        ])]));
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            mock.clone(),
        );
        let mut registry =
            crate::tools::ToolRegistry::new(ToolContext::new(workspace.path().to_path_buf()));
        registry.register(Arc::new(crate::tools::file::ReadFileTool));
        let tools = Some(registry.to_api_tools_with_cache(true));
        let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(4);
        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
        assert_eq!(status, TurnOutcomeStatus::Failed);
        assert!(error.unwrap().contains("pairing id"));
        assert_eq!(mock.call_count(), 1);
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                Event::ToolCallStarted { .. }
                    | Event::ToolCallComplete { .. }
                    | Event::ApprovalRequired { .. }
                    | Event::ToolGateDecision { .. }
            ));
        }
        assert!(
            !engine
                .session
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .any(|block| matches!(
                    block,
                    ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                ))
        );
    }
}

/// B1: tool output is redacted once, as it enters the transcript, so the
/// session messages (and the session JSON built from them) never hold a live
/// credential a tool printed.
#[tokio::test]
async fn tool_output_credentials_are_redacted_when_they_enter_the_transcript() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const TOKEN: &str = "sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefghij";
    let workspace = tempdir().expect("tempdir");
    fs::write(
        workspace.path().join("auth.json"),
        format!("{{\n  \"access_token\": \"{TOKEN}\",\n  \"note\": \"keep me\"\n}}\n"),
    )
    .expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-read", "read_file", r#"{"path":"auth.json"}"#),
        canned::simple_text_turn("done"),
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
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let stored = engine
        .session
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("the read result is in the transcript");
    assert!(!stored.contains(TOKEN), "{stored}");
    assert!(stored.contains("keep me"), "ordinary bytes stay: {stored}");
    let serialized = serde_json::to_string(&engine.session.messages.iter().collect::<Vec<_>>())
        .expect("serialize");
    assert!(!serialized.contains(TOKEN));
}

/// #4415 AC(a): an 8-call cap admits exactly 8 calls; the 9th is rejected
/// with the typed reason carrying `remaining=0` and is never executed.
#[tokio::test]
async fn tool_call_budget_admits_exactly_the_cap_and_rejects_the_ninth() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mut calls = Vec::new();
    for index in 1..=9 {
        let name = format!("fixture-{index}.txt");
        fs::write(workspace.path().join(&name), format!("fixture-{index}\n"))
            .expect("write fixture");
        calls.push((
            format!("call-{index}"),
            "read_file".to_string(),
            format!(r#"{{"path":"{name}"}}"#),
        ));
    }
    let call_refs = calls
        .iter()
        .map(|(id, name, args)| (id.as_str(), name.as_str(), args.as_str()))
        .collect::<Vec<_>>();
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&call_refs),
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), Some(8), mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(mock.call_count(), 2, "batch turn then the final text turn");
    assert_eq!(
        completions.len(),
        9,
        "every proposed call reports a completion"
    );

    for (id, result) in &completions {
        let index = id.strip_prefix("call-").expect("call id");
        if index == "9" {
            let rejection = result.as_ref().expect_err("the 9th call must be rejected");
            let reason = rejection.to_string();
            assert!(reason.contains("budget of 8"), "{reason}");
            assert!(reason.contains("remaining=0"), "{reason}");
            assert!(reason.contains("not executed"), "{reason}");
        } else {
            let outcome = result.as_ref().expect("calls within budget execute");
            assert!(
                outcome.content.contains(&format!("fixture-{index}")),
                "call {id} must return its file contents: {outcome:?}"
            );
        }
    }
}

/// #5170: a call stopped by an admission gate never executes, so its
/// debited budget slot is refunded — the cap counts admitted calls only.
/// With a cap of 1, a blocked first proposal must leave room for the
/// second proposal to run.
#[tokio::test]
async fn tool_call_budget_refunds_calls_blocked_by_admission_gates() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("fixture.txt"), "fixture\n").expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&[
            (
                "call-blocked",
                "definitely_not_a_tool",
                r#"{"path":"fixture.txt"}"#,
            ),
            ("call-admitted", "read_file", r#"{"path":"fixture.txt"}"#),
        ]),
        canned::simple_text_turn("done"),
    ]));

    let (status, error, completions) =
        run_budgeted_read_turn(workspace.path(), Some(1), mock.clone()).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(
        completions.len(),
        2,
        "every proposed call reports a completion"
    );

    let blocked = completions[0]
        .1
        .as_ref()
        .expect_err("the unknown tool must be blocked by the missing-tool gate");
    assert!(
        blocked.to_string().contains("definitely_not_a_tool"),
        "{blocked}"
    );
    let admitted = completions[1]
        .1
        .as_ref()
        .expect("a gate-blocked call refunds its slot, so the second call still fits the cap of 1");
    assert!(
        admitted.content.contains("fixture"),
        "the admitted call must return its file contents: {admitted:?}"
    );
}

/// An approval card that expires unanswered is a timeout, not the user's
/// denial: the model is told so, and the call — which never ran — gives its
/// tool-call budget slot back, so a cap of 1 still admits the next call.
#[tokio::test]
async fn approval_timeout_is_reported_as_timeout_and_refunds_the_budget() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("fixture.txt"), "fixture\n").expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-timeout", "bash", r#"{"command":"echo first"}"#),
        canned::tool_call_turn("call-after", "read_file", r#"{"path":"fixture.txt"}"#),
        canned::simple_text_turn("done"),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let config = Config::default();
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.exec_policy_engine = ask_rule_engine("echo first");
    engine_config.max_tool_calls = Some(1);
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Run the command, then read the fixture.",
            AppMode::Agent,
            &config,
        ))
        .await
        .expect("send turn");

    let mut timed_out = None;
    let mut after = None;
    let mut rx = handle.rx_event.write().await;
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for the turn")
            .expect("engine event stream closed");
        match event {
            Event::ApprovalRequired { id, tool_name, .. }
                if matches!(tool_name.as_str(), "bash" | "Bash") =>
            {
                handle
                    .deny_tool_call_timed_out(&id)
                    .await
                    .expect("expire the approval card");
            }
            Event::ApprovalRequired { id, .. } => {
                handle.approve_tool_call(&id).await.expect("approve");
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } if model_call.provider_id == "call-timeout" => {
                timed_out = Some(result);
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } if model_call.provider_id == "call-after" => {
                after = Some(result);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    let timeout = timed_out
        .expect("the expired call reports a completion")
        .expect_err("an expired approval never runs the call")
        .to_string();
    assert!(timeout.contains("timed out"), "{timeout}");
    assert!(timeout.contains("did not deny"), "{timeout}");
    assert!(
        !timeout.contains("denied by user"),
        "a timeout must not read as the user's refusal: {timeout}"
    );
    let after = after
        .expect("the next call reports a completion")
        .expect("the expired call refunded its slot, so the cap of 1 admits this call");
    assert!(after.content.contains("fixture"), "{after:?}");
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

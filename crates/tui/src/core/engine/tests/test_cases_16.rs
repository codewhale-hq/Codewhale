#[tokio::test]
async fn trust_warning_is_internal_and_tracks_current_state() {
    let _lock = lock_test_env();
    let tmp = tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("home"));
    let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", tmp.path().join("config.toml"));
    fs::create_dir_all(tmp.path().join(".claude/skills")).unwrap();
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            ..Default::default()
        },
        &Config::default(),
    );
    let context = engine.installed_next_turn_prompt_context();
    engine.refresh_pinned_header_for_turn(&context).unwrap();
    let frozen = engine.session.system_prompt.clone();
    let warning = engine
        .session
        .messages
        .last()
        .expect("logged warning")
        .clone();
    assert_eq!(warning.role, Role::User);
    assert!(crate::runtime_handoff::is_internal_runtime_handoff(
        &warning
    ));
    assert!(codewhale_models::turn_classify::is_runtime_owned_user_message(&warning));
    // The transcript side (no history cell for it) is pinned in
    // `tui::history::tests`, keeping this runtime test off the UI crate path.
    assert!(
        crate::compaction::retained_user_messages(std::slice::from_ref(&warning), 4096).is_empty()
    );
    let prompt = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Review the code".into(),
            cache_control: None,
        }],
    };
    assert_eq!(
        crate::session_manager::conversation_title_prompt(&[warning.clone(), prompt.clone()]),
        Some("Review the code")
    );
    // Quoting the envelope without engine provenance remains an ordinary user turn.
    let quoted = Message {
        role: Role::User,
        content: vec![warning.content[0].clone()],
    };
    assert!(!crate::runtime_handoff::is_internal_runtime_handoff(
        &quoted
    ));
    assert!(!codewhale_models::turn_classify::is_runtime_owned_user_message(&quoted));
    engine.session.add_message(prompt.clone());
    let initial = engine.session.messages.len();
    engine.refresh_pinned_header_for_turn(&context).unwrap();
    assert_eq!(engine.session.messages.len(), initial);

    codewhale_runtime::config::save_workspace_trust(tmp.path()).unwrap();
    engine.refresh_pinned_header_for_turn(&context).unwrap();
    let resolved = engine.session.messages.last().unwrap().clone();
    assert_ne!(resolved, warning);
    assert!(crate::runtime_handoff::is_internal_runtime_handoff(
        &resolved
    ));
    assert!(
        matches!(&resolved.content[0], ContentBlock::Text { text, .. } if text.contains("no longer applies"))
    );
    engine.refresh_pinned_header_for_turn(&context).unwrap();
    assert_eq!(engine.session.messages.len(), initial + 1);
    codewhale_runtime::config::set_workspace_trust(tmp.path(), false).unwrap();
    engine.refresh_pinned_header_for_turn(&context).unwrap();
    assert_eq!(engine.session.messages.last(), Some(&warning));
    assert_eq!(
        engine.session.messages.len(),
        initial + 2,
        "revocation must append a new warning after the correction"
    );
    assert_eq!(
        engine.session.system_prompt, frozen,
        "volatile trust facts never modify the frozen prefix"
    );
    assert!(
        !codewhale_core::prefix_cache::system_prompt_text(frozen.as_ref())
            .contains("kind=\"workspace_trust\"")
    );

    engine.refresh_system_prompt_with_reason("model");
    engine.emit_session_updated().await;
    let event = handle.rx_event.write().await.recv().await.unwrap();
    let Event::SessionUpdated { messages, .. } = event else {
        panic!("expected session update");
    };
    assert_eq!(
        messages.as_slice(),
        &[warning.clone(), prompt, resolved, warning]
    );
}

fn context_update_messages(engine: &Engine) -> Vec<String> {
    engine
        .session
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| match m.content.first() {
            Some(ContentBlock::Text { text, .. }) if text.starts_with("<context_update>") => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn workspace_drift_arrives_as_one_context_update_and_never_moves_the_header() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        project_context_pack_enabled: true,
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let context = engine.installed_next_turn_prompt_context();

    // Turn 1: establishes the pin key; nothing to report.
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );
    let pinned = engine.session.system_prompt.clone();
    let pinned_hash = engine.session.last_system_prompt_hash;
    engine.session.pending_prefix_change_reason = None;

    // No change → no snapshot.
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );

    // The agent writes a file "mid-turn"; nothing moves until the next user turn.
    fs::write(tmp.path().join("NEWFILE.md"), "brand new content").expect("write");
    assert_eq!(engine.session.system_prompt, pinned);

    // Next user turn: header byte-identical, exactly one snapshot with the delta.
    let update = engine
        .refresh_pinned_header_for_turn(&context)
        .unwrap()
        .expect("workspace drift produces a context update");
    assert!(update.starts_with("<context_update>"), "{update}");
    assert!(update.contains("NEWFILE.md"), "{update}");
    assert_eq!(engine.session.system_prompt, pinned);
    assert_eq!(engine.session.last_system_prompt_hash, pinned_hash);
    assert_eq!(engine.session.pending_prefix_change_reason, None);
    assert_eq!(
        engine
            .session
            .prefix_stability
            .as_ref()
            .unwrap()
            .context_update_count(),
        1
    );

    // The same delta is not re-sent on the following turn.
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );
}

#[test]
fn agents_md_edit_arrives_as_context_update_carrying_the_new_instructions() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    fs::write(tmp.path().join("AGENTS.md"), "# Rules\n\nAlways run fmt.\n").expect("write");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let context = engine.installed_next_turn_prompt_context();
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );
    let pinned = engine.session.system_prompt.clone();

    fs::write(
        tmp.path().join("AGENTS.md"),
        "# Rules\n\nAlways run fmt.\nNever push to main.\n",
    )
    .expect("write");
    let update = engine
        .refresh_pinned_header_for_turn(&context)
        .unwrap()
        .expect("AGENTS.md edit produces a context update");
    assert!(update.contains("+ Never push to main."), "{update}");
    assert_eq!(engine.session.system_prompt, pinned);
}

#[test]
fn explicit_input_change_repins_instead_of_snapshotting() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let context = engine.installed_next_turn_prompt_context();
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );
    engine.session.pending_prefix_change_reason = None;

    let mut next = context.clone();
    next.goal_objective = Some("ship 0.9.8".to_string());
    assert_eq!(engine.refresh_pinned_header_for_turn(&next).unwrap(), None);
    assert_eq!(
        engine.session.pending_prefix_change_reason.as_deref(),
        Some("goal")
    );
    assert_eq!(engine.session.pinned_prompt_context.as_ref(), Some(&next));
}

#[tokio::test]
async fn submitted_turn_appends_context_update_before_the_user_message() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        project_context_pack_enabled: true,
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    // Seed the pin key exactly as the first turn would.
    let context = engine.installed_next_turn_prompt_context();
    assert_eq!(
        engine.refresh_pinned_header_for_turn(&context).unwrap(),
        None
    );
    fs::write(tmp.path().join("NEWFILE.md"), "brand new content").expect("write");

    // Drive the real submit path (no client → it stops before any request,
    // but only after history is assembled) and inspect the order.
    let update = engine
        .refresh_pinned_header_for_turn(&context)
        .unwrap()
        .unwrap();
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: update,
            cache_control: None,
        }],
    });
    engine
        .session
        .add_message(engine.user_text_message_with_turn_metadata("hello".into()));
    let updates = context_update_messages(&engine);
    assert_eq!(updates.len(), 1);
    let last_two: Vec<&Message> = engine.session.messages.iter().rev().take(2).collect();
    assert!(matches!(
        last_two[1].content.first(),
        Some(ContentBlock::Text { text, .. }) if text.starts_with("<context_update>")
    ));
    assert!(matches!(
        last_two[0].content.first(),
        Some(ContentBlock::Text { text, .. }) if text == "hello"
    ));
}

#[test]
fn engine_prompt_keeps_reasoning_on_the_user_language_contract() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        locale_tag: "zh-Hans".to_string(),
        ..Default::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());
    let prompt = match engine.session.system_prompt.as_ref() {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        None => panic!("expected system prompt"),
    };

    assert!(prompt.contains("## Language"));
    assert!(prompt.contains("latest\nuser message"));
    assert!(prompt.contains("reasoning_content"));
    assert!(prompt.contains("## 语言再次提醒"));
    assert!(!prompt.contains("## Hidden Thinking Language"));
}

fn sync_runtime_system_prompt_override(engine: &mut Engine, system_prompt: SystemPrompt) {
    engine.session.compaction_summary_prompt =
        extract_compaction_summary_prompt(Some(system_prompt.clone()));
    engine.session.system_prompt = Some(system_prompt);
    engine.session.system_prompt_override = true;
}

#[test]
fn text_system_prompt_override_via_runtime_sync_survives_refresh() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let prompt = SystemPrompt::Text("TANGERINE-7".to_string());
    let expected = Some(prompt.clone());

    sync_runtime_system_prompt_override(&mut engine, prompt);
    engine.refresh_system_prompt();

    assert_eq!(engine.session.system_prompt, expected);
}

#[test]
fn blocks_system_prompt_override_via_runtime_sync_survives_mode_change_refresh() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let prompt = SystemPrompt::Blocks(vec![SystemBlock {
        block_type: "text".to_string(),
        text: "TANGERINE-7".to_string(),
        cache_control: None,
    }]);
    let expected = Some(prompt.clone());

    sync_runtime_system_prompt_override(&mut engine, prompt);
    engine.refresh_system_prompt();

    assert_eq!(engine.session.system_prompt, expected);
}

#[test]
fn compaction_checkpoint_stays_out_of_stable_system_prompt() {
    let tmp = tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
    fs::write(tmp.path().join("src/main.rs"), "fn main() {}").expect("write");

    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    engine
        .session
        .working_set
        .observe_user_message("continue in src/main.rs", tmp.path());
    engine.refresh_system_prompt();
    let stable_before = engine.session.system_prompt.clone();
    engine.commit_compaction_checkpoint(Some(SystemPrompt::Blocks(vec![SystemBlock {
        block_type: "text".to_string(),
        text: format!("{COMPACTION_SUMMARY_MARKER}\nsummary"),
        cache_control: None,
    }])));

    let prompt = match &engine.session.system_prompt {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => panic!("expected system prompt"),
    };

    assert_eq!(engine.session.system_prompt, stable_before);
    assert!(!prompt.contains(COMPACTION_SUMMARY_MARKER));
    assert!(!prompt.contains(WORKING_SET_SUMMARY_MARKER));
    assert!(
        engine
            .rendered_compaction_summary()
            .expect("checkpoint")
            .contains("summary")
    );
}

/// Repeated compaction replaces the host-persistence copy while the stable
/// system prefix remains byte-for-byte unchanged.
#[test]
fn repeated_compaction_replaces_checkpoint_without_prefix_churn() {
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    engine.session.system_prompt = Some(SystemPrompt::Text("stable base prompt".to_string()));

    let flatten = |prompt: &Option<SystemPrompt>| match prompt {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    };

    let stable_before = engine.session.system_prompt.clone();
    for round in 0..3 {
        engine.commit_compaction_checkpoint(Some(SystemPrompt::Text(format!(
            "{COMPACTION_SUMMARY_MARKER}\nround-{round} summary body"
        ))));
        assert_eq!(engine.session.system_prompt, stable_before);
        let prompt = flatten(&engine.session.compaction_summary_prompt);
        assert_eq!(
            prompt.matches(COMPACTION_SUMMARY_MARKER).count(),
            1,
            "round {round}: exactly one checkpoint: {prompt}"
        );
        assert!(
            prompt.contains(&format!("round-{round} summary body")),
            "{prompt}"
        );
    }
}

#[test]
fn caller_policy_defaults_to_direct() {
    let tool = Tool {
        tool_type: None,
        name: "read_file".to_string(),
        description: "Read".to_string(),
        input_schema: json!({"type":"object"}),
        allowed_callers: Some(vec!["direct".to_string()]),
        defer_loading: Some(false),
        input_examples: None,
        strict: None,
        cache_control: None,
    };
    let direct = ToolCaller {
        caller_type: "direct".to_string(),
        tool_id: None,
    };
    let code = ToolCaller {
        caller_type: "code_execution_20250825".to_string(),
        tool_id: Some("srvtoolu_1".to_string()),
    };
    assert!(caller_allowed_for_tool(Some(&direct), Some(&tool)));
    assert!(!caller_allowed_for_tool(Some(&code), Some(&tool)));
    assert!(caller_allowed_for_tool(None, Some(&tool)));
}

#[test]
fn tool_search_activates_discovered_deferred_tools() {
    let mut catalog = vec![
        Tool {
            tool_type: None,
            name: "read_file".to_string(),
            description: "Read files".to_string(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(true),
            input_examples: None,
            strict: None,
            cache_control: None,
        },
        Tool {
            tool_type: None,
            name: "grep_files".to_string(),
            description: "Search files".to_string(),
            input_schema: json!({"type":"object","properties":{"pattern":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(true),
            input_examples: None,
            strict: None,
            cache_control: None,
        },
    ];
    let always_load = HashSet::new();
    ensure_advanced_tooling(
        &mut catalog,
        AppMode::Agent,
        &always_load,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    let mut active = initial_active_tools(&catalog);
    let result = execute_tool_search(
        TOOL_SEARCH_NAME,
        &json!({"query":"read file"}),
        &catalog,
        &mut active,
    )
    .expect("search succeeds");
    assert!(result.success);
    assert!(active.contains("read_file"));
}

#[test]
fn tool_search_scenario() {
    // Scenario consolidation of: tool_search_can_discover_request_user_input_modal_tool, tool_search_defaults_to_eight_results_for_regex_and_bm25, tool_search_respects_and_caps_max_results, tool_search_schema_exposes_max_results_default_and_cap
    // from tool_search_can_discover_request_user_input_modal_tool
    {
        let always_load = HashSet::new();
        let mut catalog = build_model_tool_catalog(
            vec![api_tool(REQUEST_USER_INPUT_NAME)],
            Vec::new(),
            AppMode::Agent,
            &always_load,
        );
        ensure_advanced_tooling(
            &mut catalog,
            AppMode::Agent,
            &always_load,
            crate::core::engine::tool_catalog::ToolMode::Direct,
        );

        let mut active = initial_active_tools(&catalog);
        assert!(!active.contains(REQUEST_USER_INPUT_NAME));

        let result = execute_tool_search(
            TOOL_SEARCH_NAME,
            &json!({"query":"ask user question"}),
            &catalog,
            &mut active,
        )
        .expect("search succeeds");

        assert!(result.success);
        assert!(active.contains(REQUEST_USER_INPUT_NAME));
    }
    // from tool_search_defaults_to_eight_results_for_regex_and_bm25
    {
        let catalog = tool_search_catalog_with_matches(25);

        for match_kind in ["regex", "bm25"] {
            let mut active = initial_active_tools(&catalog);
            let result = execute_tool_search(
                TOOL_SEARCH_NAME,
                &json!({"query":"matching","match":match_kind}),
                &catalog,
                &mut active,
            )
            .expect("search succeeds");

            assert_eq!(tool_search_reference_count(&result), 8);
        }
    }
    // from tool_search_respects_and_caps_max_results
    {
        let catalog = tool_search_catalog_with_matches(120);

        let mut active = initial_active_tools(&catalog);
        let limited = execute_tool_search(
            TOOL_SEARCH_NAME,
            &json!({"query":"matching","max_results":7}),
            &catalog,
            &mut active,
        )
        .expect("search succeeds");
        assert_eq!(tool_search_reference_count(&limited), 7);

        let mut active = initial_active_tools(&catalog);
        let capped = execute_tool_search(
            TOOL_SEARCH_NAME,
            &json!({"query":"matching","match":"regex","max_results":999}),
            &catalog,
            &mut active,
        )
        .expect("search succeeds");
        assert_eq!(tool_search_reference_count(&capped), 8);
    }
    // from tool_search_schema_exposes_max_results_default_and_cap
    {
        let mut catalog = Vec::new();
        let always_load = HashSet::new();
        ensure_advanced_tooling(
            &mut catalog,
            AppMode::Agent,
            &always_load,
            crate::core::engine::tool_catalog::ToolMode::Direct,
        );

        let tool = catalog
            .iter()
            .find(|tool| tool.name == TOOL_SEARCH_NAME)
            .expect("tool search definition exists");
        let schema = &tool.input_schema["properties"]["max_results"];

        assert_eq!(schema["default"], 8);
        assert_eq!(schema["maximum"], 8);
        assert_eq!(schema["minimum"], 1);
        assert_eq!(tool.input_schema["properties"]["match"]["default"], "bm25");
    }
}

fn tool_search_catalog_with_matches(count: usize) -> Vec<Tool> {
    let mut catalog = (0..count)
        .map(|idx| Tool {
            tool_type: None,
            name: format!("matching_tool_{idx:03}"),
            description: "Matching deferred test tool".to_string(),
            input_schema: json!({"type":"object","properties":{"query":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(true),
            input_examples: None,
            strict: None,
            cache_control: None,
        })
        .collect::<Vec<_>>();
    let always_load = HashSet::new();
    ensure_advanced_tooling(
        &mut catalog,
        AppMode::Agent,
        &always_load,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    catalog
}

fn tool_search_reference_count(result: &ToolResult) -> usize {
    result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("tool_references"))
        .and_then(|references| references.as_array())
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn execute_tools_dispatches_through_common_executor() {
    use crate::tools::file_tool::ReadTool;
    use crate::tools::registry::ToolRegistryBuilder;
    use crate::tools::spec::ToolContext;

    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("note.txt"), "alpha\n").expect("write note");
    let context = ToolContext::new(tmp.path());
    let registry = ToolRegistryBuilder::new()
        .with_tool(Arc::new(ReadTool))
        .build(context.clone());
    let path = tmp
        .path()
        .join("note.txt")
        .to_string_lossy()
        .replace('\\', "\\\\");
    let code = format!(
        "const r = await tools.call('read', {{ path: '{path}' }}); return JSON.stringify(r).includes('alpha');"
    );
    let (tx_event, _rx_event) = mpsc::channel(8);
    let result = Engine::execute_tool_with_lock(
        Arc::new(RwLock::new(())),
        false,
        false,
        tx_event,
        None,
        EXECUTE_TOOLS_TOOL_NAME.to_string(),
        None,
        json!({"code": code}),
        tmp.path().to_path_buf(),
        Some(&registry),
        None,
        Some(context),
    )
    .await
    .expect("execute_tools should dispatch");
    assert!(result.content.contains("\"nested_calls\":1"));
    assert!(result.content.contains("true"));
}

#[tokio::test]
async fn dispatch_reports_typed_operation_activity_with_safe_identity_without_arguments() {
    use crate::tools::file_tool::ReadTool;
    use crate::tools::registry::ToolRegistryBuilder;
    use crate::tools::spec::ToolContext;
    use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerOperationOutcome};

    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("private-note.txt"), "alpha\n").expect("write note");
    let context = ToolContext::new(tmp.path());
    let registry = ToolRegistryBuilder::new()
        .with_tool(Arc::new(ReadTool))
        .build(context.clone());

    let run = |name: &'static str, span: Option<&'static str>, input: serde_json::Value| {
        let registry = &registry;
        let context = context.clone();
        let workspace = tmp.path().to_path_buf();
        async move {
            let (tx_event, mut rx_event) = mpsc::channel(16);
            let _ = Engine::execute_tool_with_lock(
                Arc::new(RwLock::new(())),
                false,
                false,
                tx_event,
                None,
                name.to_string(),
                span.map(str::to_string),
                input,
                workspace,
                Some(registry),
                None,
                Some(context),
            )
            .await;
            let mut events = Vec::new();
            while let Ok(event) = rx_event.try_recv() {
                if matches!(
                    event,
                    Event::OperationActivityStarted { .. }
                        | Event::OperationActivityCompleted { .. }
                ) {
                    events.push(event);
                }
            }
            events
        }
    };

    let events = run("read", Some("call-1"), json!({"path": "private-note.txt"})).await;
    assert!(
        matches!(
            events.as_slice(),
            [
                Event::OperationActivityStarted { span_id: started, activity_kind: OwnerActivityKind::Reading, action_id: Some(action) },
                Event::OperationActivityCompleted {
                    span_id: completed,
                    activity_kind: OwnerActivityKind::Reading,
                    outcome: OwnerOperationOutcome::Succeeded, action_id: Some(completed_action) },
            ] if started == completed && started.starts_with("call-1#") && action == "read_file" && completed_action == action
        ),
        "unexpected activity: {events:?}"
    );
    // A repeated model call id (gateways that elide ids fall back to
    // `call_{block_index}`) still gets a fresh span, so a consumer that
    // deduplicates completed spans sees the second call.
    let again = run("read", Some("call-1"), json!({"path": "private-note.txt"})).await;
    let span_of = |events: &[Event]| match events.first() {
        Some(Event::OperationActivityStarted { span_id, .. }) => span_id.clone(),
        other => panic!("unexpected activity: {other:?}"),
    };
    assert_ne!(span_of(&events), span_of(&again));
    let wire = format!("{events:?}");
    assert!(!wire.contains("private-note"), "arguments leaked: {wire}");

    // A failed read still reports its kind, with a typed outcome only.
    let events = run("read", Some("call-2"), json!({"path": "missing.txt"})).await;
    assert!(
        matches!(
            events.last(),
            Some(Event::OperationActivityCompleted {
                outcome: OwnerOperationOutcome::Failed,
                ..
            })
        ),
        "unexpected activity: {events:?}"
    );

    // No span id (internal/unattributed dispatch), an unregistered name, and
    // the code-mode wrapper itself report nothing.
    assert!(
        run("read", None, json!({"path": "private-note.txt"}))
            .await
            .is_empty()
    );
    assert!(
        run("not_a_tool", Some("call-3"), json!({}))
            .await
            .is_empty()
    );
    assert!(
        run(
            EXECUTE_TOOLS_TOOL_NAME,
            Some("call-4"),
            json!({"code": "return 1;"})
        )
        .await
        .is_empty()
    );

    // A call refused by the cancel gate never ran, so it reports nothing.
    let (tx_event, mut rx_event) = mpsc::channel(16);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let refused = Engine::execute_tool_with_lock(
        Arc::new(RwLock::new(())),
        false,
        false,
        tx_event,
        Some(cancelled),
        "read".to_string(),
        Some("call-5".to_string()),
        json!({"path": "private-note.txt"}),
        tmp.path().to_path_buf(),
        Some(&registry),
        None,
        Some(context.clone()),
    )
    .await;
    assert!(refused.is_err());
    while let Ok(event) = rx_event.try_recv() {
        assert!(
            !matches!(
                event,
                Event::OperationActivityStarted { .. } | Event::OperationActivityCompleted { .. }
            ),
            "a refused call reported activity: {event:?}"
        );
    }
}

#[tokio::test]
async fn dropped_operation_span_completes_as_cancelled() {
    use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerOperationOutcome};

    // The turn loop drops an in-flight tool future on cancel; the span it
    // opened must still close, or every host leaks an active operation.
    let (tx_event, mut rx_event) = mpsc::channel(16);
    let span = super::tool_execution::OperationSpanGuard::start(
        &tx_event,
        "call-x",
        OwnerActivityKind::Editing,
        Some("edit_file".into()),
        None,
    )
    .await;
    drop(span);
    let started = match rx_event.try_recv() {
        Ok(Event::OperationActivityStarted { span_id, .. }) => span_id,
        other => panic!("unexpected event: {other:?}"),
    };
    match rx_event.try_recv() {
        Ok(Event::OperationActivityCompleted {
            span_id,
            activity_kind: OwnerActivityKind::Editing,
            outcome: OwnerOperationOutcome::Cancelled,
            ..
        }) => assert_eq!(span_id, started),
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx_event.try_recv().is_err(), "exactly one Completed");
}

#[tokio::test]
async fn code_execution_does_not_inherit_parent_secret_env() {
    use codewhale_dependencies::ExternalTool as _;
    if !codewhale_dependencies::Python::available() {
        // `dependencies::tests::runtime_commands_do_not_inherit_parent_secret_env`
        // still covers the scrubbed Python constructor without Python.
        return;
    }
    let _env_lock = lock_test_env();
    let _secret = EnvVarGuard::set("CODEWHALE_TEST_FAKE_API_KEY", "sk-test-sentinel");
    let tmp = tempdir().expect("tempdir");
    let result = execute_code_execution_tool(
        &json!({"code":"import os; print(os.environ.get('CODEWHALE_TEST_FAKE_API_KEY', 'absent'))"}),
        tmp.path(),
        &crate::tools::spec::ToolContext::new(tmp.path()),
    )
    .await
    .expect("code execution should run");
    let stdout = result.metadata.as_ref().expect("payload")["stdout"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(stdout.trim(), "absent", "{}", result.content);
    assert!(!result.content.contains("sk-test-sentinel"));
}

#[tokio::test]
async fn code_execution_scenario() {
    // Scenario consolidation of: code_execution_runs_python_and_returns_result_payload, code_execution_runs_through_common_executor_after_approval_gate
    // from code_execution_runs_python_and_returns_result_payload
    {
        let tmp = tempdir().expect("tempdir");
        let result = execute_code_execution_tool(
            &json!({"code":"print('hello from code exec')"}),
            tmp.path(),
            &crate::tools::spec::ToolContext::new(tmp.path()),
        )
        .await
        .expect("code execution should run");
        assert!(result.content.contains("hello from code exec"));
        assert!(result.content.contains("return_code"));
    }
    // from code_execution_runs_through_common_executor_after_approval_gate
    {
        let tmp = tempdir().expect("tempdir");
        let (tx_event, _rx_event) = mpsc::channel(8);
        let result = Engine::execute_tool_with_lock(
            Arc::new(RwLock::new(())),
            false,
            false,
            tx_event,
            None,
            CODE_EXECUTION_TOOL_NAME.to_string(),
            None,
            json!({"code":"print('common executor code exec')"}),
            tmp.path().to_path_buf(),
            None,
            None,
            Some(crate::tools::spec::ToolContext::new(tmp.path())),
        )
        .await
        .expect("code_execution should run through common executor");

        assert!(result.result.content.contains("common executor code exec"));
        assert!(result.result.content.contains("return_code"));
    }
}

#[tokio::test]
async fn interpreter_execution_policy_preserves_readonly_and_exact_call_elevation() {
    use crate::tools::spec::ToolContext;
    use codewhale_sandbox::SandboxPolicy;
    for tool in [CODE_EXECUTION_TOOL_NAME, JS_EXECUTION_TOOL_NAME] {
        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside workspace");
        let context = ToolContext::new(workspace.path())
            .with_elevated_sandbox_policy(SandboxPolicy::read_only());
        for (name, policy, allowed) in [
            ("ordinary.txt", SandboxPolicy::read_only(), false),
            ("approved.txt", SandboxPolicy::DangerFullAccess, true),
            ("ordinary-again.txt", SandboxPolicy::read_only(), false),
        ] {
            let path = outside.path().join(name);
            let literal = json!(path.to_string_lossy()).to_string();
            let code = if tool == CODE_EXECUTION_TOOL_NAME {
                format!("open({literal}, 'w').write('proof')")
            } else {
                format!("require('fs').writeFileSync({literal}, 'proof')")
            };
            let result = Engine::execute_tool_with_lock(
                Arc::new(RwLock::new(())),
                false,
                false,
                mpsc::channel(8).0,
                None,
                tool.to_string(),
                None,
                json!({"code": code}),
                workspace.path().to_path_buf(),
                None,
                None,
                Some(context.clone().with_elevated_sandbox_policy(policy)),
            )
            .await;
            if allowed {
                let result = result.expect("explicitly elevated execution");
                assert!(result.result.success, "{tool}: {:?}", result.result);
            } else if let Ok(result) = result {
                assert!(!result.result.success, "{tool} ran a read-only write");
            }
            assert_eq!(path.exists(), allowed, "{tool}: {name}");
        }
    }
}

#[tokio::test]
async fn interpreter_execution_policy_refuses_external_backend_local_fallback() {
    use crate::tools::spec::ToolContext;
    let workspace = tempdir().expect("workspace");
    let backend = codewhale_sandbox::backend::create_backend(&{
        let __cw_config_init_0_0 = Some("unsupported-regression-backend".to_string());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.sandbox_backend = __cw_config_init_0_0;
        __cw_config_init_0_value
    })
    .expect("backend policy")
    .expect("external boundary");
    let context = ToolContext::new(workspace.path()).with_sandbox_backend(Arc::from(backend));
    for tool in [CODE_EXECUTION_TOOL_NAME, JS_EXECUTION_TOOL_NAME] {
        let code = if tool == CODE_EXECUTION_TOOL_NAME {
            "open('external-fallback.txt', 'w').write('bad')"
        } else {
            "require('fs').writeFileSync('external-fallback.txt', 'bad')"
        };
        let error = Engine::execute_tool_with_lock(
            Arc::new(RwLock::new(())),
            false,
            false,
            mpsc::channel(8).0,
            None,
            tool.to_string(),
            None,
            json!({"code": code}),
            workspace.path().to_path_buf(),
            None,
            None,
            Some(context.clone()),
        )
        .await
        .expect_err("external backend cannot run a local interpreter");
        assert!(error.to_string().contains("external sandbox"), "{error}");
    }
    assert!(!workspace.path().join("external-fallback.txt").exists());
}

#[test]
fn plan_mode_catalog_skips_code_execution_tool_but_agent_keeps_it() {
    let mut plan_catalog = vec![api_tool("read_file")];
    let always_load = HashSet::new();
    ensure_advanced_tooling(
        &mut plan_catalog,
        AppMode::Plan,
        &always_load,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    assert!(
        !plan_catalog
            .iter()
            .any(|tool| tool.name == CODE_EXECUTION_TOOL_NAME),
        "Plan mode must not expose code_execution"
    );

    let mut agent_catalog = vec![api_tool("read_file")];
    ensure_advanced_tooling(
        &mut agent_catalog,
        AppMode::Agent,
        &always_load,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    assert!(
        agent_catalog
            .iter()
            .any(|tool| tool.name == CODE_EXECUTION_TOOL_NAME),
        "Agent mode should still expose code_execution"
    );
}

#[test]
fn missing_tool_error_message_offers_suggestions() {
    let catalog = vec![
        Tool {
            tool_type: None,
            name: "read_file".to_string(),
            description: "Read file contents".to_string(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        },
        Tool {
            tool_type: None,
            name: "grep_files".to_string(),
            description: "Search file contents".to_string(),
            input_schema: json!({"type":"object","properties":{"pattern":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        },
    ];

    let message = missing_tool_error_message("reed_file", &catalog);
    assert!(message.contains("Did you mean:"));
    assert!(message.contains("read_file"));
    assert!(message.contains(TOOL_SEARCH_NAME));
}

#[test]
fn missing_tool_scenario() {
    // Scenario consolidation of: missing_tool_error_message_includes_discovery_guidance_when_no_match, missing_tool_error_message_redirects_checklist_item_miscalls, missing_tool_error_message_names_exec_shell_rename
    // from missing_tool_error_message_includes_discovery_guidance_when_no_match
    {
        let catalog = vec![Tool {
            tool_type: None,
            name: "read_file".to_string(),
            description: "Read file contents".to_string(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        }];

        let message = missing_tool_error_message("totally_unknown_tool", &catalog);
        assert!(message.contains("not available in the current tool catalog"));
        assert!(message.contains(TOOL_SEARCH_NAME));
    }
    // from missing_tool_error_message_redirects_checklist_item_miscalls
    {
        let catalog = vec![api_tool("note"), api_tool("tts")];

        for tool_name in ["item", "items", "todo", "checklist_item"] {
            let message = missing_tool_error_message(tool_name, &catalog);
            assert!(message.contains("todo_write"), "{tool_name}: {message}");
            assert!(
                !message.contains("Did you mean"),
                "fuzzy suggestions are misleading for checklist mis-calls: {message}"
            );
        }
    }
    // from missing_tool_error_message_names_exec_shell_rename
    {
        // #5123-class: retired exec_shell must point at lowercase foreground bash,
        // not misdiagnosed as an allow_shell permission problem.
        let catalog = vec![api_tool("read_file")];

        let message = missing_tool_error_message("exec_shell", &catalog);
        assert!(message.contains("replaced by `bash`"), "{message}");
        assert!(message.contains("`command`"), "{message}");

        for tool_name in [
            "exec_shell_wait",
            "exec_shell_interact",
            "exec_shell_cancel",
        ] {
            let message = missing_tool_error_message(tool_name, &catalog);
            assert!(message.contains("not available in the current tool catalog"));
            assert!(
                message.contains("foreground-only"),
                "{tool_name}: {message}"
            );
            assert!(message.contains(TOOL_SEARCH_NAME), "{tool_name}: {message}");
        }
    }
}

#[test]
fn missing_shell_scenario() {
    // Scenario consolidation of: missing_shell_tool_error_message_names_allow_shell_gate, missing_shell_tool_error_message_keeps_allow_shell_hint_with_suggestions
    // from missing_shell_tool_error_message_names_allow_shell_gate
    {
        let catalog = vec![api_tool("read_file")];

        for tool_name in ["task_shell_start", "task_shell_wait"] {
            let message = missing_tool_error_message(tool_name, &catalog);
            assert!(message.contains("not available in the current tool catalog"));
            assert!(
                message.contains("allow_shell = false"),
                "{tool_name}: {message}"
            );
            assert!(message.contains("allow_shell"), "{tool_name}: {message}");
            assert!(
                message.contains("/config allow_shell true"),
                "{tool_name}: {message}"
            );
            assert!(message.contains("--save"), "{tool_name}: {message}");
            assert!(message.contains("Work mode"), "{tool_name}: {message}");
            assert!(
                message.contains("approval gating"),
                "{tool_name}: {message}"
            );
            assert!(!message.contains("YOLO"), "{tool_name}: {message}");
            assert!(!message.contains("auto-approve"), "{tool_name}: {message}");
            assert!(message.contains(TOOL_SEARCH_NAME), "{tool_name}: {message}");
        }
    }
    // from missing_shell_tool_error_message_keeps_allow_shell_hint_with_suggestions
    {
        let catalog = vec![api_tool("task_shell_starter")];

        let message = missing_tool_error_message("task_shell_start", &catalog);

        assert!(message.contains("Did you mean:"));
        assert!(message.contains("task_shell_starter"));
        assert!(message.contains("allow_shell = false"));
        assert!(message.contains("allow_shell"));
        assert!(message.contains("/config allow_shell true"));
        assert!(message.contains("--save"));
        assert!(message.contains("Work mode"));
        assert!(!message.contains("YOLO"));
        assert!(!message.contains("auto-approve"));
        assert!(message.contains(TOOL_SEARCH_NAME));
    }
}

#[test]
fn filter_tool_scenario() {
    // Scenario consolidation of: filter_tool_call_delta_strips_bracket_marker, filter_tool_call_delta_strips_deepseek_xml_marker, filter_tool_call_delta_strips_deepseek_native_tool_tokens, filter_tool_call_delta_strips_deepseek_native_token_split_across_chunks, filter_tool_call_delta_strips_generic_tool_call_marker, filter_tool_call_delta_strips_invoke_marker, filter_tool_call_delta_strips_function_calls_marker, filter_tool_call_delta_strips_siliconflow_v4_dsml_content_fixture
    // from filter_tool_call_delta_strips_bracket_marker
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "intro [TOOL_CALL]\n{\"tool\":\"x\"}\n[/TOOL_CALL] outro",
            &mut in_block,
        );
        assert!(!in_block);
        assert!(!visible.contains("[TOOL_CALL]"));
        assert!(!visible.contains("[/TOOL_CALL]"));
        assert!(!visible.contains("\"tool\":\"x\""));
        assert!(visible.contains("intro"));
        assert!(visible.contains("outro"));
    }
    // from filter_tool_call_delta_strips_deepseek_xml_marker
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "before <codewhale:tool_call name=\"x\">payload</codewhale:tool_call> after",
            &mut in_block,
        );
        assert!(!in_block);
        for marker in TOOL_CALL_START_MARKERS {
            assert!(
                !visible.contains(marker),
                "visible text leaked start marker `{marker}`: {visible:?}"
            );
        }
        assert!(visible.contains("before"));
        assert!(visible.contains("after"));
    }
    // from filter_tool_call_delta_strips_deepseek_native_tool_tokens
    {
        // #3880: DeepSeek's chat template separates words with `▁` (U+2581), so
        // `<｜tool▁calls▁begin｜>` matched no DSML entry and reached the user as
        // visible text that interrupted the task.
        for (start, end) in [
            ("<｜tool▁calls▁begin｜>", "<｜tool▁calls▁end｜>"),
            ("<｜tool▁call▁begin｜>", "<｜tool▁call▁end｜>"),
            ("<|tool▁calls▁begin|>", "<|tool▁calls▁end|>"),
            ("<｜tool_calls_begin｜>", "<｜tool_calls_end｜>"),
            ("<|tool_call_begin|>", "<|tool_call_end|>"),
            ("<｜tool▁outputs▁begin｜>", "<｜tool▁outputs▁end｜>"),
        ] {
            let mut in_block = false;
            let visible = filter_tool_call_delta(
                &format!("before {start}function<｜tool▁sep｜>read_file\n{{}}{end} after"),
                &mut in_block,
            );
            assert!(!in_block, "state stuck inside block for {start}");
            assert!(
                !visible.contains("tool▁") && !visible.contains("tool_calls"),
                "leaked {start} into visible text: {visible:?}"
            );
            assert!(visible.contains("before"), "{visible:?}");
            assert!(visible.contains("after"), "{visible:?}");
        }
    }
    // DeepSeek's doubled-delimiter DSML form, emitted when the request offers
    // no tools: one-shot `codewhale exec` printed it verbatim as the answer.
    {
        let text = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"read_file\">\n<｜｜DSML｜｜ parameter name=\"path\" string=\"true\">note.txt</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>\n";
        assert!(contains_fake_tool_wrapper(text));
        for cut in 1..text.len() {
            if !text.is_char_boundary(cut) {
                continue;
            }
            let mut state = ToolCallDeltaFilterState::default();
            let mut visible = filter_tool_call_delta_with_state(&text[..cut], &mut state);
            visible.push_str(&filter_tool_call_delta_with_state(&text[cut..], &mut state));
            visible.push_str(&flush_tool_call_delta_state(&mut state));
            assert_eq!(visible.trim(), "", "cut {cut} leaked DSML: {visible:?}");
        }
    }
    // from filter_tool_call_delta_strips_deepseek_native_token_split_across_chunks
    {
        // The streaming filter carries a partial marker across chunk boundaries.
        // These markers are multi-byte, so a split partway through one is the case
        // most likely to slip past the carry buffer.
        let mut state = ToolCallDeltaFilterState::default();
        let full = "before <｜tool▁calls▁begin｜>payload<｜tool▁calls▁end｜> after";
        let cut = full.find("calls").expect("marker present") + 2;
        let mut visible = filter_tool_call_delta_with_state(&full[..cut], &mut state);
        visible.push_str(&filter_tool_call_delta_with_state(&full[cut..], &mut state));

        assert!(
            !visible.contains("tool▁") && !visible.contains("payload"),
            "chunk-split marker leaked: {visible:?}"
        );
        assert!(visible.contains("before"), "{visible:?}");
        assert!(visible.contains("after"), "{visible:?}");
    }
    // from filter_tool_call_delta_strips_generic_tool_call_marker
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "lead <tool_call>\n{\"name\":\"do\"}\n</tool_call> tail",
            &mut in_block,
        );
        assert!(!in_block);
        assert!(!visible.contains("<tool_call"));
        assert!(!visible.contains("</tool_call>"));
        assert!(visible.contains("lead"));
        assert!(visible.contains("tail"));
    }
    // from filter_tool_call_delta_strips_invoke_marker
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "alpha <invoke name=\"x\"><parameter name=\"k\">v</parameter></invoke> beta",
            &mut in_block,
        );
        assert!(!in_block);
        assert!(!visible.contains("<invoke "));
        assert!(!visible.contains("</invoke>"));
        assert!(visible.contains("alpha"));
        assert!(visible.contains("beta"));
    }
    // from filter_tool_call_delta_strips_function_calls_marker
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "head <function_calls>\n{\"name\":\"x\"}\n</function_calls> tail",
            &mut in_block,
        );
        assert!(!in_block);
        assert!(!visible.contains("<function_calls>"));
        assert!(!visible.contains("</function_calls>"));
        assert!(visible.contains("head"));
        assert!(visible.contains("tail"));
    }
    // from filter_tool_call_delta_strips_siliconflow_v4_dsml_content_fixture
    {
        // #2900: a SiliconFlow CN `deepseek-ai/DeepSeek-V4-Pro` stream can leak
        // DSML/function-call markup through the ordinary content channel. Keep it
        // out of visible assistant text; do not reinterpret `<function_calls>` as
        // an executable legacy text tool call.
        let mut in_block = false;
        let visible_a = filter_tool_call_delta(
            "visible prefix <function_calls>\n{\"name\":\"exec_shell\",\"arguments\":{\"cmd\":\"echo leaked\"}}",
            &mut in_block,
        );
        assert!(in_block);
        assert_eq!(visible_a, "visible prefix ");

        let visible_b = filter_tool_call_delta("\n</function_calls> visible suffix", &mut in_block);
        assert!(!in_block);
        assert_eq!(visible_b, " visible suffix");
        assert!(!visible_b.contains("exec_shell"));
        assert!(!visible_b.contains("<function_calls>"));
    }
}

#[test]
fn marker_tables_are_consistent() {
    // Three parallel tables describe the same wrapper shapes. They drifting
    // apart is exactly how #3880's family went unhandled, so assert they
    // agree rather than trusting review.
    assert_eq!(
        TOOL_CALL_MARKER_PAIRS.len(),
        TOOL_CALL_START_MARKERS.len(),
        "start-marker table is out of sync with the pair table"
    );
    assert_eq!(
        TOOL_CALL_MARKER_PAIRS.len(),
        TOOL_CALL_END_MARKERS.len(),
        "end-marker table is out of sync with the pair table"
    );
    for (index, (start, end)) in TOOL_CALL_MARKER_PAIRS.iter().enumerate() {
        assert_eq!(
            *start, TOOL_CALL_START_MARKERS[index],
            "start marker {index} disagrees with the pair table"
        );
        assert_eq!(
            *end, TOOL_CALL_END_MARKERS[index],
            "end marker {index} disagrees with the pair table"
        );
    }
}

#[test]
fn filter_tool_scenario_2() {
    // Scenario consolidation of: filter_tool_call_delta_strips_fullwidth_dsml_invoke_fixture, filter_tool_call_delta_strips_ascii_dsml_invoke_fixture, filter_tool_call_delta_carries_split_fullwidth_dsml_marker, filter_tool_call_delta_flushes_clean_partial_marker_prefix, filter_tool_call_delta_handles_chunk_split_marker, filter_tool_call_delta_unmatched_open_suppresses_remainder, filter_tool_call_delta_passes_through_clean_text
    // from filter_tool_call_delta_strips_fullwidth_dsml_invoke_fixture
    {
        // #3717: Windows users reported SiliconFlow/DSML content leaking through
        // the ordinary text channel with fullwidth DSML wrapper tags. Treat it as
        // non-API tool markup, not visible assistant text.
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "visible prefix <｜DSML｜tool_calls>\n\
             <｜DSML｜invoke name=\"read_file\">\n\
             <｜DSML｜parameter name=\"path\" string=\"true\">backend/open_webui/utils/auth.py</｜DSML｜parameter>\n\
             </｜DSML｜invoke>\n\
             </｜DSML｜tool_calls> visible suffix",
            &mut in_block,
        );

        assert!(!in_block);
        assert_eq!(visible, "visible prefix  visible suffix");
        assert!(!visible.contains("DSML"));
        assert!(!visible.contains("read_file"));
        assert!(!visible.contains("backend/open_webui"));
    }
    // from filter_tool_call_delta_strips_ascii_dsml_invoke_fixture
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta(
            "visible prefix <|DSML|tool_calls>\n\
             <|DSML|invoke name=\"read_file\">\n\
             <|DSML|parameter name=\"path\" string=\"true\">backend/open_webui/utils/auth.py</|DSML|parameter>\n\
             </|DSML|invoke>\n\
             </|DSML|tool_calls> visible suffix",
            &mut in_block,
        );

        assert!(!in_block);
        assert_eq!(visible, "visible prefix  visible suffix");
        assert!(!visible.contains("DSML"));
        assert!(!visible.contains("read_file"));
        assert!(!visible.contains("backend/open_webui"));
    }
    // from filter_tool_call_delta_carries_split_fullwidth_dsml_marker
    {
        let mut state = ToolCallDeltaFilterState::default();

        let visible_a = filter_tool_call_delta_with_state("visible prefix <｜DS", &mut state);
        assert_eq!(visible_a, "visible prefix ");

        let visible_b = filter_tool_call_delta_with_state(
            "ML｜tool_calls>\n<｜DSML｜invoke name=\"read_file\">",
            &mut state,
        );
        assert!(
            visible_b.is_empty(),
            "split DSML opener leaked: {visible_b:?}"
        );

        let visible_c = filter_tool_call_delta_with_state(
            "</｜DSML｜invoke>\n</｜DSML｜tool_calls> visible suffix",
            &mut state,
        );
        assert_eq!(visible_c, " visible suffix");
    }
    // from filter_tool_call_delta_flushes_clean_partial_marker_prefix
    {
        let mut state = ToolCallDeltaFilterState::default();

        let visible = filter_tool_call_delta_with_state("ordinary text ending in <", &mut state);
        assert_eq!(visible, "ordinary text ending in ");

        let flushed = flush_tool_call_delta_state(&mut state);
        assert_eq!(flushed, "<");
    }
    // from filter_tool_call_delta_handles_chunk_split_marker
    {
        let mut in_block = false;
        // First chunk opens the wrapper but does not close it.
        let visible_a = filter_tool_call_delta("hello <tool_call>partial", &mut in_block);
        assert!(in_block, "filter must remember it is mid-wrapper");
        assert_eq!(visible_a, "hello ");

        // Second chunk continues inside the wrapper, then closes it and adds tail.
        let visible_b = filter_tool_call_delta("payload</tool_call> tail", &mut in_block);
        assert!(!in_block);
        assert_eq!(visible_b, " tail");
    }
    // from filter_tool_call_delta_unmatched_open_suppresses_remainder
    {
        let mut in_block = false;
        let visible = filter_tool_call_delta("ok [TOOL_CALL]rest of stream", &mut in_block);
        assert_eq!(visible, "ok ");
        assert!(
            in_block,
            "unmatched open must leave filter in tool-call mode"
        );
    }
    // from filter_tool_call_delta_passes_through_clean_text
    {
        let mut in_block = false;
        let input = "no markers here, just prose with code `<not a tag>`.";
        let visible = filter_tool_call_delta(input, &mut in_block);
        assert!(!in_block);
        assert_eq!(visible, input);
    }
}

#[test]
fn contains_fake_scenario() {
    // Scenario consolidation of: contains_fake_tool_wrapper_detects_each_marker, contains_fake_tool_wrapper_returns_false_on_clean_text
    // from contains_fake_tool_wrapper_detects_each_marker
    {
        for marker in TOOL_CALL_START_MARKERS {
            let needle = format!("noise {marker} more noise");
            assert!(
                contains_fake_tool_wrapper(&needle),
                "marker `{marker}` should be detected"
            );
        }
    }
    // from contains_fake_tool_wrapper_returns_false_on_clean_text
    {
        assert!(!contains_fake_tool_wrapper(
            "plain assistant text without wrappers"
        ));
        assert!(!contains_fake_tool_wrapper(
            "`<tool` lookalike but not a real start marker"
        ));
    }
}

#[test]
fn fake_wrapper_notice_is_compact_and_actionable() {
    // Keep this short so it fits cleanly in a single status line.
    assert!(FAKE_WRAPPER_NOTICE.len() < 120);
    assert!(FAKE_WRAPPER_NOTICE.contains("API tool channel"));
}

// ---- final_tool_input: bug-class regression for "<command>" placeholder ----
//
// Background: a streamed tool block carries its `input` in two pieces — an
// initial value at `ContentBlockStart` (often `{}`), then `InputJsonDelta`
// chunks that build up `input_buffer`. The TUI used to fire `ToolCallStarted`
// from `ContentBlockStart` with the empty initial input and never re-emit
// once args were known, so cells rendered the literal text `<command>` /
// `<file>` placeholders. The input is finalized at `ContentBlockStop`, then published after batch admission
// through `final_tool_input`, which prefers the parsed
// buffer over a stale empty placeholder.
fn tool_state(initial: serde_json::Value, buffer: &str) -> ToolUseState {
    ToolUseState {
        execution_id: uuid::Uuid::new_v4().to_string(),
        id: "t1".into(),
        name: "exec_shell".into(),
        input: initial,
        caller: None,
        thought_signature: None,
        input_buffer: buffer.into(),
        input_parse_error: None,
    }
}

#[test]
fn final_tool_scenario() {
    // Scenario consolidation of: final_tool_input_prefers_parsed_buffer_over_empty_initial, final_tool_input_falls_back_to_initial_when_buffer_empty, final_tool_input_preserves_raw_buffer_for_parse_errors
    // from final_tool_input_prefers_parsed_buffer_over_empty_initial
    {
        // The exact regression: ContentBlockStart delivered `{}`, then args
        // streamed in via InputJsonDelta. The emitted ToolCallStarted must
        // carry the parsed buffer, not the placeholder.
        let state = tool_state(json!({}), r#"{"command": "ls -la"}"#);
        assert_eq!(final_tool_input(&state), json!({"command": "ls -la"}));
    }
    // from final_tool_input_falls_back_to_initial_when_buffer_empty
    {
        // Models occasionally embed args directly in the start frame and never
        // send any InputJsonDelta. We must still report those args.
        let state = tool_state(json!({"command": "echo hi"}), "");
        assert_eq!(final_tool_input(&state), json!({"command": "echo hi"}));
    }
    // from final_tool_input_preserves_raw_buffer_for_parse_errors
    {
        let mut state = tool_state(json!({}), "{not json");
        state.input_parse_error = Some("malformed tool arguments".into());
        assert_eq!(
            final_tool_input(&state),
            json!({"raw_arguments": "{not json"})
        );
    }
    // A `write` whose stream was cut at its output limit, right after a
    // complete string value. `arg_repair` CAN make this parse by appending
    // one `}`, and before the repair ladder reported provenance that guess
    // was dispatched — writing a file containing only "first line" while the
    // model was still mid-argument. It must now take the malformed path, so
    // the model is told to re-issue instead.
    {
        let state = tool_state(json!({}), r#"{"path": "notes.md", "content": "first line""#);
        assert_eq!(
            final_tool_input(&state),
            json!({"raw_arguments": r#"{"path": "notes.md", "content": "first line""#}),
            "a truncated write must not be dispatched as a completed argument"
        );
    }
    // The guard must not fire on arguments that were merely sloppy: a
    // trailing comma is structurally complete and still has to dispatch, or
    // every DeepSeek chunk-boundary repair would start failing tool calls.
    {
        let state = tool_state(json!({}), r#"{"command": "ls -la",}"#);
        assert_eq!(final_tool_input(&state), json!({"command": "ls -la"}));
    }
}

// === #103 transparent stream-retry policy =====================================

#[test]
fn stream_retry_scenario() {
    // Scenario consolidation of: stream_retry_zero_content_then_error_is_transparently_retried, stream_retry_after_content_received_surfaces_error, stream_retry_respects_cancellation, stream_retry_budget_caps_resumes_in_mechanism, stream_retry_threshold_relaxed_to_five
    // from stream_retry_zero_content_then_error_is_transparently_retried
    {
        // Case 2 from issue #103: stream yielded ZERO content then errored.
        // The decoder hit Err on the very first poll → engine should retry
        // because DeepSeek hasn't billed and the user has seen nothing.
        assert!(
            super::should_transparently_retry_stream(
                false,
                0,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                false
            ),
            "first attempt with no content must be eligible for transparent retry"
        );
        assert!(
            super::should_transparently_retry_stream(
                false,
                1,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                false
            ),
            "second attempt (one prior retry) with no content must still be eligible"
        );
    }
    // from stream_retry_after_content_received_surfaces_error
    {
        // Case 3 from issue #103: stream yielded content then errored. We must
        // NOT transparently retry — the model has emitted billed output tokens
        // and the UI has streamed deltas; resending would double-bill and the
        // user would see the same prefix twice.
        assert!(
            !super::should_transparently_retry_stream(
                true,
                0,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                false
            ),
            "any content received → no transparent retry, even with full budget"
        );
        assert!(
            !super::should_transparently_retry_stream(
                true,
                1,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                false
            ),
            "any content received → no transparent retry on subsequent attempts"
        );
    }
    // from stream_retry_respects_cancellation
    {
        // Cancellation overrides every other condition. If the user pressed
        // Esc / Ctrl-C, do not silently re-issue the request behind their back.
        assert!(
            !super::should_transparently_retry_stream(
                false,
                0,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                true
            ),
            "cancelled turn must not be transparently retried"
        );
        assert!(
            !super::should_transparently_retry_stream(
                false,
                1,
                super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
                true
            ),
            "cancelled turn must not be transparently retried even with budget"
        );
    }
    // from stream_retry_budget_caps_resumes_in_mechanism
    {
        // "At most one bounded retry per drop" is enforced by types, not by a
        // comment: `authorize()` is the only way to spend a resume and it refuses
        // once DEFAULT_STREAM_MAX_RESUMES resumes have been issued, whatever the guard
        // predicates say. A healthy round resets the chain.
        let mut budget = super::StreamRetryBudget::default();
        assert_eq!(budget.spent(), 0);
        assert_eq!(budget.authorize(), Some(1));
        assert_eq!(budget.authorize(), Some(2));
        assert_eq!(budget.authorize(), Some(3));
        assert_eq!(
            budget.authorize(),
            None,
            "authorize() must refuse past DEFAULT_STREAM_MAX_RESUMES"
        );
        assert_eq!(budget.authorize(), None, "and keep refusing");
        assert_eq!(budget.spent(), super::DEFAULT_STREAM_MAX_RESUMES);
        budget.reset();
        assert_eq!(budget.spent(), 0);
        assert_eq!(budget.authorize(), Some(1));
    }
    // from stream_retry_threshold_relaxed_to_five
    {
        // Case 1+4 from issue #103: the consecutive-error threshold for marking
        // the turn failed was relaxed from 3 → 5 in v0.6.7 because the new
        // HTTP/2 keepalive defaults make spurious decode errors rarer.
        // This test pins the constant so a future regression to 3 fails loudly.
        assert_eq!(
            super::DEFAULT_STREAM_MAX_ERRORS,
            5,
            "the consecutive-stream-error threshold should be 5; \
             lowering it back to 3 will fail mid-turn under transient flakiness"
        );
        // And a regression guard on the transparent-retry cap.
        assert_eq!(
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
            2,
            "transparent-retry cap should be 2; raising it risks hammering the \
             provider on real outages"
        );
    }
}

#[test]
fn mode_notice_is_recorded_on_entering_plan_or_operate_and_on_leaving_it() {
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(config, &Config::default());
    let notices = |engine: &Engine| {
        engine
            .session
            .messages
            .iter()
            .filter_map(crate::runtime_handoff::mode_notice_display)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    // A Work session that never entered Plan carries no notice.
    engine.record_mode_notice(AppMode::Agent);
    assert!(notices(&engine).is_empty());

    engine.record_mode_notice(AppMode::Plan);
    engine.record_mode_notice(AppMode::Plan);
    let recorded = notices(&engine);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert!(recorded[0].starts_with("Mode: Plan."), "{recorded:?}");

    // Leaving Plan is announced so history never ends on a stale mode.
    engine.record_mode_notice(AppMode::Agent);
    let recorded = notices(&engine);
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert!(recorded[1].starts_with("Mode: Work."), "{recorded:?}");

    engine.session.messages.clear();
    engine.record_mode_notice(AppMode::Operate);
    engine.record_mode_notice(AppMode::Operate);
    assert_eq!(notices(&engine).len(), 1);
    engine
        .session
        .add_message(crate::runtime_handoff::operate_contract_runtime_message());
    engine.record_mode_notice(AppMode::Operate);
    assert_eq!(notices(&engine).len(), 1);
    engine.record_mode_notice(AppMode::Agent);
    let recorded = notices(&engine);
    assert_eq!(recorded.len(), 2);
    assert!(recorded[1].starts_with("Mode: Work."));

    engine.session.messages.clear();
    engine
        .session
        .add_message(crate::runtime_handoff::legacy_operate_contract_runtime_message());
    engine.record_mode_notice(AppMode::Agent);
    assert!(notices(&engine)[0].starts_with("Mode: Work."));
}

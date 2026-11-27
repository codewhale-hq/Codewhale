use super::*;
use crate::conversation_modes::CreatorAvailabilityGuard;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::runtime_handoff::is_creator_runtime_message;
use crate::tools::spec::ToolSpec;

const SKILL: &str = "cordis-plugin-development";

fn installed_skills_dir() -> PathBuf {
    let owned = codewhale_runtime::config::owned_skills_dir().expect("owned skills dir");
    codewhale_skills::install_system_skills(&owned, &Default::default()).expect("install skills");
    owned
}

fn engine_for(workspace: &Path, preset: Option<&str>) -> Engine {
    Engine::new(
        EngineConfig {
            workspace: workspace.to_path_buf(),
            terminal_chrome_enabled: false,
            snapshots_enabled: false,
            subagents_enabled: false,
            agent_preset: preset.map(str::to_string),
            ..EngineConfig::default()
        },
        &Config::default(),
    )
    .0
}

fn creator_events(engine: &Engine) -> Vec<String> {
    engine
        .session
        .messages
        .iter()
        .filter(|message| is_creator_runtime_message(message))
        .map(|message| format!("{message:?}"))
        .collect()
}

async fn run_turns_recording_events(
    config: EngineConfig,
    mock: Arc<MockLlmClient>,
    prompts: &[&str],
) -> (Vec<String>, bool) {
    let api_config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(config, &api_config, mock);
    let task = tokio::spawn(engine.run());
    let mut statuses = Vec::new();
    let mut saw_approval = false;
    for prompt in prompts {
        handle
            .send(external_user_message_op(
                prompt,
                AppMode::Agent,
                &api_config,
            ))
            .await
            .unwrap();
        let mut rx = handle.rx_event.write().await;
        loop {
            match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                Event::Status { message } => statuses.push(message),
                Event::ApprovalRequired { .. } => {
                    saw_approval = true;
                    break;
                }
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
    }
    if saw_approval {
        task.abort();
    } else {
        handle.send(Op::Shutdown).await.unwrap();
        task.await.unwrap();
    }
    (statuses, saw_approval)
}

fn tool_names(request: &codewhale_models::MessageRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .flatten()
        .map(|tool| tool.name.clone())
        .collect()
}

fn child_engine_from_parent(workspace: &Path, parent: &Engine) -> Engine {
    let api = Config::default().with_legacy_root(Some("cr04-child-fixture".into()), None);
    let client = parent
        .codewhale_client
        .clone()
        .unwrap_or_else(|| CodewhaleClient::new(&api).expect("stub client"));
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: DEFAULT_TEXT_MODEL.into(),
        capabilities: Default::default(),
        limits: None,
        client: Some(client.clone()),
        api_config: Box::new(api.clone()),
        locale_tag: parent.config.locale_tag.clone(),
        role_models: HashMap::new(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let policy = TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let context = parent.build_tool_context_for_turn(&policy, &route);
    let runtime = SubAgentRuntime::new(
        client,
        DEFAULT_TEXT_MODEL.into(),
        context,
        true,
        None,
        parent.subagent_manager.clone(),
    )
    .with_api_config(api.clone());
    let authority = crate::tools::subagent::engine::ChildAuthority::capture(
        runtime,
        FleetRole::Scout,
        "child-a".into(),
        "inspect".into(),
        None,
    );
    Engine::new_child_admitted(
        EngineConfig {
            workspace: workspace.to_path_buf(),
            model: DEFAULT_TEXT_MODEL.into(),
            ..Default::default()
        },
        &api,
        authority,
        SystemPrompt::Text("child".into()),
        None,
    )
    .unwrap()
    .0
}

async fn run_turns(
    config: EngineConfig,
    mock: Arc<MockLlmClient>,
    prompts: &[&str],
) -> Vec<String> {
    let (statuses, saw_approval) = run_turns_recording_events(config, mock, prompts).await;
    assert!(!saw_approval, "unexpected ApprovalRequired event");
    statuses
}

fn prompt_text(request: &codewhale_models::MessageRequest) -> String {
    codewhale_core::prefix_cache::system_prompt_text(request.system.as_ref())
}

fn creator_messages(request: &codewhale_models::MessageRequest) -> usize {
    request
        .messages
        .iter()
        .filter(|message| is_creator_runtime_message(message))
        .count()
}

#[tokio::test]
async fn extension_creator_first_turn_records_the_role_and_the_skill_once() {
    let _home = crate::test_support::SealedHome::new();
    let _available = CreatorAvailabilityGuard::set(None);
    installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let creator_mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("first"),
        canned::simple_text_turn("second"),
    ]));
    let mut config = deterministic_engine_config(workspace.path());
    config.agent_preset = Some("creator".to_string());
    run_turns(config, creator_mock.clone(), &["make a plugin", "go on"]).await;
    let requests = creator_mock.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(creator_messages(&requests[0]), 1);
    assert_eq!(creator_messages(&requests[1]), 1, "a second turn adds none");
    let event = requests[0]
        .messages
        .iter()
        .find_map(|message| {
            crate::runtime_handoff::is_creator_runtime_message(message)
                .then(|| format!("{message:?}"))
        })
        .unwrap();
    assert!(event.contains("Creator mode"));
    assert!(event.contains("The plugin-development skill is loaded below."));
    assert!(event.contains("plugin_manager"));
    assert!(event.contains("# Skill: cordis-plugin-development"));
    assert!(event.contains("agent-experience"));

    let standard_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("first")]));
    run_turns(
        deterministic_engine_config(workspace.path()),
        standard_mock.clone(),
        &["make a plugin"],
    )
    .await;
    let standard = standard_mock.captured_requests();
    assert_eq!(creator_messages(&standard[0]), 0);
    assert_eq!(
        prompt_text(&requests[0]),
        prompt_text(&standard[0]),
        "the system prompt bytes do not change with the mode"
    );

    let mut restored = engine_for(workspace.path(), Some("creator"));
    restored.creator_available = true;
    restored.session.messages = requests[1].messages.clone().into();
    restored.record_creator_first_turn();
    assert_eq!(
        creator_events(&restored).len(),
        1,
        "a restored thread adds none"
    );
}

#[tokio::test]
async fn extension_creator_skills_show_only_in_the_creator_thread() {
    let _home = crate::test_support::SealedHome::new();
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let mut standard = engine_for(workspace.path(), None);
    let mut creator = engine_for(workspace.path(), Some("creator"));
    standard.creator_available = true;
    creator.creator_available = true;
    for (engine, visible) in [(&standard, false), (&creator, true)] {
        let config = engine.conversation_skills_config();
        let context =
            ToolContext::new(workspace.path()).with_skills_config(owned.clone(), config.clone());
        let tool = crate::tools::skill::LoadSkillTool;
        let listing = tool
            .execute(json!({"name": "list"}), &context)
            .await
            .unwrap()
            .content;
        let loaded = tool.execute(json!({"name": SKILL}), &context).await;
        assert_eq!(listing.contains(SKILL), visible, "{listing}");
        assert_eq!(loaded.is_ok(), visible);
        let index = codewhale_skills::render_index(
            workspace.path(),
            Some(owned.as_path()),
            &config,
            "en",
            None,
            None,
        )
        .unwrap_or_default();
        assert_eq!(
            index.contains(SKILL),
            visible,
            "sub-agent catalogue: {index}"
        );
    }
}

#[tokio::test]
async fn extension_creator_skills_config_cannot_be_set_from_config_toml() {
    let _home = crate::test_support::SealedHome::new();
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let rejected = toml::from_str::<codewhale_runtime::config::SkillsConfig>(
        "creator_skills = true\n[bundled]\nselected = [\"cordis-plugin-development\"]\n",
    );
    assert!(
        rejected.is_err(),
        "creator_skills must be rejected as an unknown [skills] key"
    );
    let parsed: codewhale_runtime::config::SkillsConfig =
        toml::from_str("[bundled]\nselected = [\"cordis-plugin-development\"]\n").unwrap();
    let parsed = codewhale_runtime::config::SkillsConfig {
        creator_skills: true,
        ..parsed
    };
    assert!(parsed.creator_skills);
    let mut engine = engine_for(workspace.path(), None);
    engine.config.skills_config = Arc::new(parsed);
    engine.creator_available = true;
    let config = engine.conversation_skills_config();
    assert!(!config.creator_skills);
    let context = ToolContext::new(workspace.path()).with_skills_config(owned, config);
    let listing = crate::tools::skill::LoadSkillTool
        .execute(json!({"name": "list"}), &context)
        .await
        .unwrap()
        .content;
    assert!(!listing.contains(SKILL), "{listing}");
}

#[tokio::test]
async fn extension_creator_skill_files_read_without_a_card_and_refuse_writes() {
    let _home = crate::test_support::SealedHome::new();
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let mut creator = engine_for(workspace.path(), Some("creator"));
    creator.creator_available = true;
    let roots = creator.conversation_read_only_roots();
    assert_eq!(roots.len(), 4);
    let mut standard = engine_for(workspace.path(), None);
    standard.creator_available = true;
    assert!(standard.conversation_read_only_roots().is_empty());

    let reference = owned.join(SKILL).join("references/host-plugin.md");
    let template = owned.join(SKILL).join("templates/decoration/client.js");
    let context = ToolContext::new(workspace.path()).with_read_only_roots(roots);
    for file in [&reference, &template] {
        let input = json!({"path": file.to_string_lossy()});
        assert_eq!(
            crate::tools::file::ReadFileTool.approval_requirement_for(&input),
            crate::tools::spec::ApprovalRequirement::Auto
        );
        assert_eq!(
            crate::tools::file_tool::ReadTool.approval_requirement_for(&input),
            crate::tools::spec::ApprovalRequirement::Auto
        );
        let result = crate::tools::file::ReadFileTool
            .execute(input.clone(), &context)
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        let contract_result = crate::tools::file_tool::ReadTool
            .execute(input, &context)
            .await
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let bundled = fs::read_to_string(file).unwrap();
        assert!(!bundled.is_empty());
        let first_line = bundled.lines().next().unwrap();
        assert!(
            result.content.contains(first_line),
            "{}: {}",
            file.display(),
            result.content
        );
        assert!(
            contract_result.content.contains(first_line),
            "read {}: {}",
            file.display(),
            contract_result.content
        );
    }
    let listing = crate::tools::file::ListDirTool
        .execute(
            json!({"path": owned.join(SKILL).to_string_lossy()}),
            &context,
        )
        .await
        .unwrap();
    assert!(listing.content.contains("SKILL.md"));
    let grep_input = json!({
        "pattern": "Bundles and Host plugins",
        "path": owned.join(SKILL).to_string_lossy(),
    });
    assert_eq!(
        crate::tools::search::GrepFilesTool.approval_requirement_for(&grep_input),
        crate::tools::spec::ApprovalRequirement::Auto
    );
    let grep = crate::tools::search::GrepFilesTool
        .execute(grep_input, &context)
        .await
        .unwrap();
    assert!(grep.content.contains("host-plugin.md"), "{}", grep.content);

    let target = reference.to_string_lossy().to_string();
    let before = fs::read(&reference).unwrap();
    for (tool, input) in [
        ("write_file", json!({"path": target, "content": "changed"})),
        (
            "edit_file",
            json!({"path": target, "search": "a", "replace": "b"}),
        ),
        (
            "apply_patch",
            json!({
                "path": target,
                "patch": "--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-a\n+b\n",
            }),
        ),
        (
            "fim_edit",
            json!({
                "path": target,
                "prefix_anchor": "a",
                "suffix_anchor": "b",
            }),
        ),
    ] {
        let error = match tool {
            "write_file" => crate::tools::file::WriteFileTool
                .execute(input, &context)
                .await
                .unwrap_err(),
            "edit_file" => crate::tools::file::EditFileTool
                .execute(input, &context)
                .await
                .unwrap_err(),
            "apply_patch" => crate::tools::apply_patch::ApplyPatchTool
                .execute(input, &context)
                .await
                .unwrap_err(),
            _ => crate::tools::fim::FimEditTool::new(None, "model".to_string())
                .execute(input, &context)
                .await
                .unwrap_err(),
        };
        assert!(
            matches!(error, crate::tools::spec::ToolError::PathEscape { .. }),
            "{tool}: {error}"
        );
    }
    assert_eq!(fs::read(&reference).unwrap(), before);
    assert!(matches!(
        context.resolve_path(&target),
        Err(crate::tools::spec::ToolError::PathEscape { .. })
    ));
    assert!(crate::tools::spec::resolve_strict_authority_path(&context, &target).is_err());

    let outside = owned
        .join(SKILL)
        .join("..")
        .join(".system-installed-version");
    assert!(owned.join(".system-installed-version").is_file());
    assert!(
        context
            .resolve_read_path(&outside.to_string_lossy())
            .is_err(),
        "a `..` path out of the folder is refused"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let secret = elsewhere.path().join("secret.txt");
    fs::write(&secret, "outside").unwrap();
    assert!(
        context
            .resolve_read_path(&secret.to_string_lossy())
            .is_err()
    );
    #[cfg(unix)]
    {
        let link = owned.join(SKILL).join("references/escape.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(
            context.resolve_read_path(&link.to_string_lossy()).is_err(),
            "a link inside the folder pointing out is refused"
        );
        fs::remove_file(&link).unwrap();
    }
}

#[tokio::test]
async fn extension_creator_first_turn_never_loads_a_project_skill_of_the_same_name() {
    let _home = crate::test_support::SealedHome::new();
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    crate::test_support::trust_workspace(workspace.path());
    let project = workspace.path().join(".codewhale/skills").join(SKILL);
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("SKILL.md"),
        "---\nname: cordis-plugin-development\ndescription: project copy\n---\nPROJECT-SKILL-MARKER\n",
    )
    .unwrap();
    let mut engine = engine_for(workspace.path(), Some("creator"));
    engine.creator_available = true;
    engine.record_creator_first_turn();
    let events = creator_events(&engine);
    assert_eq!(events.len(), 1);
    assert!(!events[0].contains("PROJECT-SKILL-MARKER"));
    assert!(events[0].contains("# Skill: cordis-plugin-development"));

    let mut edited = engine_for(workspace.path(), Some("creator"));
    edited.creator_available = true;
    fs::write(owned.join(SKILL).join("SKILL.md"), "my own copy").unwrap();
    edited.record_creator_first_turn();
    let events = creator_events(&edited);
    assert_eq!(events.len(), 1);
    assert!(!events[0].contains("my own copy"));
    assert!(!events[0].contains("# Skill: cordis-plugin-development"));
    assert!(!events[0].contains("loaded below"));
}

#[tokio::test]
async fn extension_creator_names_do_not_raise_the_always_load_notice() {
    let _home = crate::test_support::SealedHome::new();
    let workspace = tempfile::tempdir().unwrap();
    let mut creator = engine_for(workspace.path(), Some("creator"));
    creator.creator_available = true;
    let effective = creator.effective_tools_always_load();
    for name in crate::conversation_modes::CREATOR_TOOL_NAMES {
        assert!(effective.contains(name), "{name}");
        assert!(
            !creator.config.tools_always_load.contains(name),
            "the dead-entry notice reads the configured set only"
        );
    }
}

#[tokio::test]
async fn extension_standard_thread_catalog_head_is_unchanged() {
    let _home = crate::test_support::SealedHome::new();
    let workspace = tempfile::tempdir().unwrap();
    let available_names = {
        let _available = CreatorAvailabilityGuard::set(None);
        let standard_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
        run_turns(
            deterministic_engine_config(workspace.path()),
            standard_mock.clone(),
            &["hi"],
        )
        .await;
        let unknown_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
        let mut unknown_config = deterministic_engine_config(workspace.path());
        unknown_config.agent_preset = Some("future-mode".to_string());
        run_turns(unknown_config, unknown_mock.clone(), &["hi"]).await;
        let standard_names = tool_names(&standard_mock.captured_requests()[0]);
        let unknown_names = tool_names(&unknown_mock.captured_requests()[0]);
        assert!(!standard_names.is_empty());
        assert_eq!(standard_names, unknown_names);
        standard_names
    };
    let unavailable_names = {
        let _unavailable = CreatorAvailabilityGuard::set(Some("No plugin runtime."));
        let unavailable_mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
        run_turns(
            deterministic_engine_config(workspace.path()),
            unavailable_mock.clone(),
            &["hi"],
        )
        .await;
        tool_names(&unavailable_mock.captured_requests()[0])
    };
    assert_eq!(available_names, unavailable_names);

    let mut creator_parent = engine_for(workspace.path(), Some("creator"));
    creator_parent.creator_available = true;
    let child = child_engine_from_parent(workspace.path(), &creator_parent);
    assert!(child.conversation_loadout().always_load.is_empty());
    assert!(!child.conversation_loadout().first_turn_event);
    assert_eq!(
        child.effective_tools_always_load(),
        child.config.tools_always_load
    );
}

#[tokio::test]
async fn extension_creator_thread_without_creator_says_why_and_loads_nothing() {
    let _home = crate::test_support::SealedHome::new();
    let _unavailable = CreatorAvailabilityGuard::set(Some("No plugin runtime was found."));
    installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("first"),
        canned::simple_text_turn("second"),
    ]));
    let mut config = deterministic_engine_config(workspace.path());
    config.agent_preset = Some("creator".to_string());
    let statuses = run_turns(config, mock.clone(), &["make a plugin", "again"]).await;
    let reasons = statuses
        .iter()
        .filter(|status| status.contains("No plugin runtime was found."))
        .count();
    assert_eq!(
        reasons, 1,
        "one status line, not one per turn: {statuses:?}"
    );
    for request in mock.captured_requests() {
        assert_eq!(creator_messages(&request), 0);
        assert!(
            request
                .tools
                .iter()
                .flatten()
                .all(|tool| tool.name != "plugin_manager")
        );
    }
}

#[tokio::test]
async fn extension_creator_mode_follows_sync_session() {
    let _home = crate::test_support::SealedHome::new();
    let workspace = tempfile::tempdir().unwrap();
    let mut engine = engine_for(workspace.path(), None);
    engine.creator_available = true;
    assert_eq!(
        engine.effective_tools_always_load(),
        engine.config.tools_always_load
    );
    assert!(!engine.conversation_loadout().creator_skills);
    assert!(engine.conversation_read_only_roots().is_empty());
    assert!(!engine.conversation_skills_config().creator_skills);

    engine.session.agent_preset = Some("creator".to_string());
    for name in crate::conversation_modes::CREATOR_TOOL_NAMES {
        assert!(
            engine.effective_tools_always_load().contains(name),
            "{name}"
        );
    }
    assert!(engine.conversation_skills_config().creator_skills);

    engine.session.agent_preset = None;
    assert_eq!(
        engine.effective_tools_always_load(),
        engine.config.tools_always_load
    );
    assert!(!engine.conversation_skills_config().creator_skills);
}

#[tokio::test]
async fn extension_creator_sub_agent_inherits_the_four_skills_and_can_read_their_files() {
    let _home = crate::test_support::SealedHome::new();
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let reference = owned.join(SKILL).join("references/host-plugin.md");
    for (preset, visible) in [(Some("creator"), true), (None, false)] {
        let mut parent = engine_for(workspace.path(), preset);
        parent.creator_available = true;
        let child = child_engine_from_parent(workspace.path(), &parent);
        assert_eq!(child.conversation_skills_config().creator_skills, visible);
        assert_eq!(
            child.conversation_read_only_roots().len(),
            if visible { 4 } else { 0 }
        );
        let context = ToolContext::new(workspace.path())
            .with_skills_config(owned.clone(), child.conversation_skills_config())
            .with_read_only_roots(child.conversation_read_only_roots());
        let loaded = crate::tools::skill::LoadSkillTool
            .execute(json!({"name": SKILL}), &context)
            .await;
        assert_eq!(loaded.is_ok(), visible);
        let read_ref = crate::tools::file::ReadFileTool
            .execute(json!({"path": reference.to_string_lossy()}), &context)
            .await;
        assert_eq!(read_ref.is_ok(), visible);
    }
}

#[tokio::test]
async fn extension_creator_turn_reads_skill_file_without_approval_event() {
    let _home = crate::test_support::SealedHome::new();
    let _available = CreatorAvailabilityGuard::set(None);
    let owned = installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let reference = owned.join(SKILL).join("references/host-plugin.md");
    let expected_line = fs::read_to_string(&reference)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let arguments = json!({"path": reference.to_string_lossy()}).to_string();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-skill-read", "read_file", &arguments),
        canned::simple_text_turn("done"),
    ]));
    let mut config = deterministic_engine_config(workspace.path());
    config.agent_preset = Some("creator".to_string());
    let (_, saw_approval) =
        run_turns_recording_events(config, mock.clone(), &["read the host plugin guide"]).await;
    assert!(!saw_approval);
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    let result = requests[1]
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } if tool_use_id == "call-skill-read" => Some((content.as_str(), *is_error)),
            _ => None,
        })
        .expect("tool result for read_file");
    assert_ne!(result.1, Some(true), "{}", result.0);
    assert!(result.0.contains(&expected_line), "{}", result.0);
}

#[tokio::test]
async fn extension_creator_first_turn_event_is_restored_whenever_the_log_lacks_it() {
    let _home = crate::test_support::SealedHome::new();
    installed_skills_dir();
    let workspace = tempfile::tempdir().unwrap();
    let mut engine = engine_for(workspace.path(), Some("creator"));
    engine.creator_available = true;
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "<context_update>pinned header changed</context_update>".to_string(),
            cache_control: None,
        }],
    });
    engine.record_creator_first_turn();
    assert_eq!(creator_events(&engine).len(), 1);
    engine.record_creator_first_turn();
    assert_eq!(creator_events(&engine).len(), 1);
    let kept: Vec<_> = engine
        .session
        .messages
        .iter()
        .filter(|message| !is_creator_runtime_message(message))
        .cloned()
        .collect();
    engine.session.replace_messages(kept);
    engine.record_creator_first_turn();
    assert_eq!(
        creator_events(&engine).len(),
        1,
        "a log that lost it gets it back"
    );
}

fn creator_test_manager(
    node: std::path::PathBuf,
    root: std::path::PathBuf,
) -> Arc<crate::extension_host::ExtensionHostManager> {
    Arc::new(crate::extension_host::ExtensionHostManager::new(
        crate::extension_host::ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(root),
            ..Default::default()
        },
    ))
}

async fn run_prompt_with_outcomes(
    handle: &EngineHandle,
    config: &Config,
    prompt: &str,
) -> (Vec<(String, bool, String)>, usize) {
    handle
        .send(external_user_message_op(prompt, AppMode::Agent, config))
        .await
        .unwrap();
    let mut outcomes = Vec::new();
    let mut approvals = 0;
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ApprovalRequired { id, .. } => {
                approvals += 1;
                handle.deny_tool_call(id).await.unwrap();
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => match result {
                Ok(res) => outcomes.push((model_call.provider_id, res.success, res.content)),
                Err(err) => outcomes.push((model_call.provider_id, false, err.to_string())),
            },
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    (outcomes, approvals)
}

async fn run_turn_with_tool_outcomes(
    engine: Engine,
    handle: EngineHandle,
    prompt: &str,
) -> (Vec<(String, bool, String)>, usize) {
    let config = Config::default();
    let task = tokio::spawn(engine.run());
    let result = run_prompt_with_outcomes(&handle, &config, prompt).await;
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();
    result
}

#[tokio::test]
async fn extension_creator_tool_is_found_by_search_in_a_standard_thread() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_tool_is_found_by_search_in_a_standard_thread",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node.clone(), temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let _available = CreatorAvailabilityGuard::set(None);
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("Warmed."),
        canned::tool_call_turn(
            "call-search",
            "tool_search",
            r#"{"query":"plugin_manager","match":"regex"}"#,
        ),
        canned::simple_text_turn("Found plugin_manager."),
    ]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
        &workspace,
    )));
    let (_engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
    let config = Config::default();
    let task = tokio::spawn(_engine.run());

    let (warm_outcomes, warm_approvals) = run_prompt_with_outcomes(&handle, &config, "Warm.").await;
    assert_eq!(warm_approvals, 0);
    assert!(warm_outcomes.is_empty());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !manager
        .live_tool_names()
        .iter()
        .any(|name| name == "plugin_manager")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "creator module did not start in the background"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (outcomes, approvals) = run_prompt_with_outcomes(&handle, &config, "Search.").await;
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();
    assert_eq!(approvals, 0);
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        !tool_names(&requests[0])
            .iter()
            .any(|name| name == "plugin_manager")
    );
    assert!(
        !tool_names(&requests[1])
            .iter()
            .any(|name| name == "plugin_manager")
    );
    let search_outcome = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-search")
        .expect("tool_search outcome");
    assert!(search_outcome.1 && search_outcome.2.contains("plugin_manager"));
    assert!(
        tool_names(&requests[2])
            .iter()
            .any(|name| name == "plugin_manager")
    );
    assert_eq!(
        manager.tier_spawn_attempts(crate::extension_host::tier::HostTier::Builtin),
        1
    );
    assert_eq!(
        manager.tier_spawn_attempts(crate::extension_host::tier::HostTier::Plugin),
        0
    );
    manager.shutdown().await;
    drop(_available);
    drop(_manager);

    let idle_manager = creator_test_manager(node, temp.path().join("home-off"));
    let _idle_guard = crate::extension_host::TestManagerGuard::install(Arc::clone(&idle_manager));
    let mut key_off = deterministic_engine_config(&workspace);
    key_off.features.disable(Feature::PluginCreator);
    key_off.features.disable(Feature::ExtensionHost);
    run_turns(
        key_off,
        Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")])),
        &["hi"],
    )
    .await;
    {
        let _unavail = CreatorAvailabilityGuard::set(Some("No plugin runtime."));
        let mut unavail_cfg = deterministic_engine_config(&workspace);
        unavail_cfg.features.enable(Feature::PluginCreator);
        unavail_cfg.features.disable(Feature::ExtensionHost);
        run_turns(
            unavail_cfg,
            Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")])),
            &["hi"],
        )
        .await;
    }
    assert_eq!(
        idle_manager.tier_spawn_attempts(crate::extension_host::tier::HostTier::Builtin),
        0
    );
    assert_eq!(
        idle_manager.tier_spawn_attempts(crate::extension_host::tier::HostTier::Plugin),
        0
    );
    idle_manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_thread_has_plugin_manager_in_its_first_request() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_thread_has_plugin_manager_in_its_first_request",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("Ready.")]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
        &workspace,
    )));
    cfg.agent_preset = Some("creator".to_string());
    run_turns(cfg, mock.clone(), &["hello"]).await;

    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        tool_names(&requests[0])
            .iter()
            .any(|name| name == "plugin_manager")
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_switch_off_attachment_leaves_the_plugin_registry_unchanged() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_switch_off_attachment_leaves_the_plugin_registry_unchanged",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    let plugins_dir = temp.path().join("user-plugins");
    let plugin_dir = plugins_dir.join("sample-all");
    fs::create_dir_all(plugin_dir.join("skills/sample-skill")).unwrap();
    fs::create_dir_all(plugin_dir.join("commands")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        plugin_dir.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"sample-all\"\nversion = \"1.0.0\"\n\n[skills]\npath = \"skills\"\n\n[commands]\npath = \"commands\"\n\n[mcp_servers.local]\ncommand = \"node\"\nargs = [\"server.js\"]\n",
    )
    .unwrap();
    fs::write(
        plugin_dir.join("skills/sample-skill/SKILL.md"),
        "---\nname: sample-skill\ndescription: sample skill\n---\nBody\n",
    )
    .unwrap();
    fs::write(
        plugin_dir.join("commands/sample.md"),
        "---\ndescription: sample command\n---\nPrompt\n",
    )
    .unwrap();

    let discovery = codewhale_plugins::discovery::DiscoveryConfig {
        workspace: workspace.clone(),
        user_plugins_dir: plugins_dir,
        workspace_plugins_dir: workspace.join(".codewhale/plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: temp.path().join("state/plugin-state.json"),
    };
    let mut registry = codewhale_plugins::discovery::discover_with_config(&discovery);
    registry.trust("sample-all").unwrap();
    registry.enable("sample-all").unwrap();
    let plugins = Arc::new(registry);

    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let mut cfg_on = deterministic_engine_config(&workspace);
    cfg_on.features.enable(Feature::PluginCreator);
    cfg_on.features.disable(Feature::ExtensionHost);
    cfg_on.plugin_registry = Some(Arc::clone(&plugins));
    let mock_on = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
    let (engine_on, handle_on) = Engine::new_with_model_client(cfg_on, &Config::default(), mock_on);

    let mut cfg_off = deterministic_engine_config(&workspace);
    cfg_off.features.disable(Feature::PluginCreator);
    cfg_off.features.disable(Feature::ExtensionHost);
    cfg_off.plugin_registry = Some(Arc::clone(&plugins));
    let mock_off = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("ok")]));
    let (engine_off, _handle_off) =
        Engine::new_with_model_client(cfg_off, &Config::default(), mock_off);

    let summary = |reg: &codewhale_plugins::PluginRegistry| {
        let active = reg.active_plugins();
        let skills: Vec<_> = active
            .iter()
            .flat_map(|p| p.skill_snapshots.iter().map(|s| s.name.clone()))
            .collect();
        let commands: Vec<_> = codewhale_plugins::runtime::active_component_sources(
            reg,
            codewhale_plugins::activation::PluginActivationCapability::Commands,
        )
        .0
        .into_iter()
        .map(|s| (s.plugin_name, s.path))
        .collect();
        let mcps: Vec<_> = active
            .iter()
            .filter_map(|p| {
                p.manifest
                    .mcp_servers
                    .as_ref()
                    .map(|m| (p.name().to_string(), m.keys().cloned().collect::<Vec<_>>()))
            })
            .collect();
        (skills, commands, mcps)
    };
    assert!(Arc::ptr_eq(
        &engine_on.plugin_registry,
        &engine_off.plugin_registry
    ));
    let expected = summary(&engine_off.plugin_registry);
    assert_eq!(expected.0, vec!["sample-skill".to_string()]);
    assert_eq!(expected.1.len(), 1);
    assert_eq!(
        expected.2,
        vec![("sample-all".to_string(), vec!["local".to_string()])]
    );
    assert_eq!(summary(&engine_on.plugin_registry), expected);

    let before_view = engine_on
        .extension_host
        .as_ref()
        .expect("attached for creator")
        .plugin_view();
    assert_eq!(summary(&before_view), expected);
    let _ = run_turn_with_tool_outcomes(engine_on, handle_on, "hi").await;
    assert_eq!(summary(&before_view), expected);
    assert_eq!(
        manager.tier_spawn_attempts(crate::extension_host::tier::HostTier::Plugin),
        0
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_tool_honors_disallowed_tools() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_tool_honors_disallowed_tools",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    for preset in [Some("creator"), None] {
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-pm", "plugin_manager", r#"{"action":"list_bundles"}"#),
            canned::simple_text_turn("Done."),
        ]));
        let mut cfg = deterministic_engine_config(&workspace);
        cfg.features.enable(Feature::PluginCreator);
        cfg.features.disable(Feature::ExtensionHost);
        cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
            &workspace,
        )));
        cfg.agent_preset = preset.map(str::to_string);
        cfg.disallowed_tools = Some(vec!["plugin_manager".to_string()]);
        let (engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
        let (outcomes, approvals) = run_turn_with_tool_outcomes(engine, handle, "List.").await;
        assert_eq!(approvals, 0, "preset={preset:?}");
        let requests = mock.captured_requests();
        for req in &requests {
            assert!(
                !tool_names(req).iter().any(|name| name == "plugin_manager"),
                "preset={preset:?}"
            );
        }
        let pm = outcomes
            .iter()
            .find(|(id, _, _)| id == "call-pm")
            .expect("call-pm outcome");
        assert!(!pm.1, "disallowed tool must refuse: {pm:?}");
    }
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_inspect_lists_a_new_row_without_a_card() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_inspect_lists_a_new_row_without_a_card",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let user_plugins = temp.path().join("home/.codewhale/plugins");
    fs::create_dir_all(&user_plugins).unwrap();

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-inspect-1",
            "cordis_inspect_query",
            r#"{"platform":"host","provider":"Config","method":"listConfigs","input":{}}"#,
        ),
        canned::tool_call_turn(
            "call-inspect-2",
            "cordis_inspect_query",
            r#"{"platform":"host","provider":"Config","method":"listConfigs","input":{}}"#,
        ),
        canned::simple_text_turn("All inspected."),
    ]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    let reg_config = codewhale_plugins::discovery::DiscoveryConfig {
        workspace: workspace.clone(),
        user_plugins_dir: user_plugins.clone(),
        workspace_plugins_dir: workspace.join(".codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: temp.path().join("state/plugin-state.json"),
    };
    let registry = Arc::new(codewhale_plugins::discovery::discover_with_config(
        &reg_config,
    ));
    cfg.plugin_registry = Some(Arc::clone(&registry));
    cfg.agent_preset = Some("creator".to_string());

    let (engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
    let config = Config::default();
    let task = tokio::spawn(engine.run());

    handle
        .send(external_user_message_op(
            "Inspect rows.",
            AppMode::Agent,
            &config,
        ))
        .await
        .unwrap();

    let mut outcomes = Vec::new();
    let mut approvals = 0;
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ApprovalRequired { id, .. } => {
                approvals += 1;
                handle.deny_tool_call(id).await.unwrap();
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => {
                let id = model_call.provider_id.clone();
                match result {
                    Ok(res) => outcomes.push((id.clone(), res.success, res.content)),
                    Err(err) => outcomes.push((id.clone(), false, err.to_string())),
                }
                if id == "call-inspect-1" {
                    let plugin_dir = user_plugins.join("fresh-plugin");
                    let skill_dir = plugin_dir.join("skills/fresh-skill");
                    fs::create_dir_all(&skill_dir).unwrap();
                    fs::write(
                        skill_dir.join("SKILL.md"),
                        "---\nname: fresh-skill\ndescription: Fresh skill\n---\nBody\n",
                    )
                    .unwrap();
                    fs::write(
                        plugin_dir.join("plugin.toml"),
                        "schema_version = 1\n[plugin]\nname = \"fresh-plugin\"\nversion = \"1.0.0\"\n\n[skills]\npath = \"skills\"\n",
                    )
                    .unwrap();
                    let mut fresh_registry =
                        codewhale_plugins::discovery::discover_with_config(&reg_config);
                    let _ = fresh_registry.trust("fresh-plugin");
                    let _ = fresh_registry.enable("fresh-plugin");
                    crate::extension_host::plugins_changed(Arc::new(fresh_registry));
                }
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();

    assert_eq!(approvals, 0);
    let requests = mock.captured_requests();
    assert!(!requests.is_empty());
    let first_tools = tool_names(&requests[0]);
    assert!(first_tools.iter().any(|name| name == "cordis_inspect_list"));
    assert!(
        first_tools
            .iter()
            .any(|name| name == "cordis_inspect_query")
    );
    let first_req_tools = requests[0].tools.as_ref().unwrap();
    assert_eq!(
        first_req_tools
            .iter()
            .find(|t| t.name == "cordis_inspect_list")
            .and_then(|t| t.defer_loading),
        Some(false)
    );
    assert_eq!(
        first_req_tools
            .iter()
            .find(|t| t.name == "cordis_inspect_query")
            .and_then(|t| t.defer_loading),
        Some(false)
    );

    let inspect1 = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-inspect-1")
        .expect("call-inspect-1 outcome");
    assert!(inspect1.1);
    assert!(!inspect1.2.contains("fresh-plugin"));

    let inspect2 = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-inspect-2")
        .expect("call-inspect-2 outcome");
    assert!(inspect2.1);
    assert!(
        inspect2.2.contains("fresh-plugin"),
        "expected fresh-plugin in second inspect answer: {}",
        inspect2.2
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_inspect_runs_in_plan_without_a_card() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_inspect_runs_in_plan_without_a_card",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-inspect-list", "cordis_inspect_list", "{}"),
        canned::tool_call_turn(
            "call-inspect-query",
            "cordis_inspect_query",
            r#"{"platform":"host","provider":"Service","method":"listService","input":{}}"#,
        ),
        canned::simple_text_turn("Plan inspection complete."),
    ]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
        &workspace,
    )));
    cfg.agent_preset = Some("creator".to_string());

    let (engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
    let config = Config::default();
    let task = tokio::spawn(engine.run());

    handle
        .send(external_user_message_op(
            "Plan inspection.",
            AppMode::Plan,
            &config,
        ))
        .await
        .unwrap();

    let mut outcomes = Vec::new();
    let mut approvals = 0;
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ApprovalRequired { id, .. } => {
                approvals += 1;
                handle.deny_tool_call(id).await.unwrap();
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => {
                let id = model_call.provider_id.clone();
                match result {
                    Ok(res) => outcomes.push((id, res.success, res.content)),
                    Err(err) => outcomes.push((id, false, err.to_string())),
                }
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();

    assert_eq!(approvals, 0);
    let list_outcome = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-inspect-list")
        .expect("call-inspect-list outcome");
    assert!(list_outcome.1, "list outcome failed: {list_outcome:?}");
    assert!(list_outcome.2.contains("providers"));

    let query_outcome = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-inspect-query")
        .expect("call-inspect-query outcome");
    assert!(query_outcome.1, "query outcome failed: {query_outcome:?}");
    assert!(query_outcome.2.contains("Service"));
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_inspect_tools_lists_what_the_model_can_call() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_inspect_tools_lists_what_the_model_can_call",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-tools",
            "cordis_inspect_query",
            r#"{"platform":"host","provider":"Tool","method":"listTools","input":{}}"#,
        ),
        canned::simple_text_turn("Tool inspection complete."),
    ]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
        &workspace,
    )));
    cfg.agent_preset = Some("creator".to_string());

    let (engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
    let (outcomes, approvals) = run_turn_with_tool_outcomes(engine, handle, "Check tools.").await;
    assert_eq!(approvals, 0);

    let tools_outcome = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-tools")
        .expect("call-tools outcome");
    assert!(tools_outcome.1, "tool query failed: {tools_outcome:?}");
    assert!(
        tools_outcome.2.contains("plugin_manager"),
        "expected plugin_manager in Tool list: {}",
        tools_outcome.2
    );
    assert!(
        !tools_outcome.2.contains("dsh_broker_act"),
        "hidden tool dsh_broker_act must not appear in Tool list: {}",
        tools_outcome.2
    );
    assert!(
        !tools_outcome.2.contains("dsh_broker_reach"),
        "hidden tool dsh_broker_reach must not appear in Tool list: {}",
        tools_outcome.2
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_inspect_in_a_standard_conversation() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_inspect_in_a_standard_conversation",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("Warmed."),
        canned::tool_call_turn(
            "call-search",
            "tool_search",
            r#"{"query":"cordis_inspect","match":"regex"}"#,
        ),
        canned::simple_text_turn("Found inspection tools."),
    ]));
    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    cfg.plugin_registry = Some(Arc::new(codewhale_plugins::PluginRegistry::empty(
        &workspace,
    )));
    let (engine, handle) = Engine::new_with_model_client(cfg, &Config::default(), mock.clone());
    let config = Config::default();
    let task = tokio::spawn(engine.run());

    let (warm_outcomes, warm_approvals) = run_prompt_with_outcomes(&handle, &config, "Warm.").await;
    assert_eq!(warm_approvals, 0);
    assert!(warm_outcomes.is_empty());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !manager
        .live_tool_names()
        .iter()
        .any(|name| name == "cordis_inspect_list")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "creator module did not start in the background"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (outcomes, approvals) = run_prompt_with_outcomes(&handle, &config, "Search.").await;
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();
    assert_eq!(approvals, 0);
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        !tool_names(&requests[0])
            .iter()
            .any(|name| name == "cordis_inspect_list")
    );
    assert!(
        !tool_names(&requests[1])
            .iter()
            .any(|name| name == "cordis_inspect_list")
    );
    let search_outcome = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-search")
        .expect("tool_search outcome");
    assert!(
        search_outcome.1
            && search_outcome.2.contains("cordis_inspect_list")
            && search_outcome.2.contains("cordis_inspect_query")
    );
    assert!(
        tool_names(&requests[2])
            .iter()
            .any(|name| name == "cordis_inspect_list")
    );
    assert!(
        tool_names(&requests[2])
            .iter()
            .any(|name| name == "cordis_inspect_query")
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn extension_creator_install_in_full_access_answers_applied() {
    let _home = crate::test_support::SealedHome::new();
    let Some(node) = crate::extension_host::test_support::node_for_tests(
        "extension_creator_install_in_full_access_answers_applied",
    ) else {
        return;
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(false);
    let _available = CreatorAvailabilityGuard::set(None);
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("ws");
    let user_plugins = temp.path().join("home/.codewhale/plugins");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&user_plugins).unwrap();

    let bundle_dir = workspace.join("demo-bundle");
    fs::create_dir_all(bundle_dir.join("skills/helper")).unwrap();
    fs::write(
        bundle_dir.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"demo-bundle\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(
        bundle_dir.join("skills/helper/SKILL.md"),
        "---\nname: helper\ndescription: test skill\n---\nbody\n",
    )
    .unwrap();

    let manager = creator_test_manager(node, temp.path().join("home"));
    let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));

    let install_args = json!({
        "action": "install_bundle",
        "target": bundle_dir.to_string_lossy(),
    })
    .to_string();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-list",
            "plugin_manager",
            r#"{"action":"list_bundles"}"#,
        ),
        canned::tool_call_turn("call-install", "plugin_manager", &install_args),
        canned::simple_text_turn("Installed."),
    ]));

    let mut cfg = deterministic_engine_config(&workspace);
    cfg.features.enable(Feature::PluginCreator);
    cfg.features.disable(Feature::ExtensionHost);
    let reg_config = codewhale_plugins::discovery::DiscoveryConfig {
        workspace: workspace.clone(),
        user_plugins_dir: user_plugins.clone(),
        workspace_plugins_dir: workspace.join(".codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: temp.path().join("state/plugin-state.json"),
    };
    let registry = Arc::new(codewhale_plugins::discovery::discover_with_config(
        &reg_config,
    ));
    cfg.plugin_registry = Some(Arc::clone(&registry));
    cfg.agent_preset = Some("creator".to_string());

    let (mut engine, mut handle) = Engine::new_with_model_client(cfg, &Config::default(), mock);
    engine.attach_owner_surface_for_tests(&mut handle);
    let config = Config::default();
    let task = tokio::spawn(engine.run());

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
        .unwrap();
    let mut op = external_user_message_op(
        "List bundles and install the workspace bundle.",
        AppMode::Agent,
        &config,
    );
    if let Op::SendMessage(ref mut spec) = op {
        spec.trust_mode = true;
        spec.auto_approve = true;
        spec.approval_mode = ApprovalMode::Bypass;
    }
    handle.send(op).await.unwrap();

    let mut outcomes = Vec::new();
    let mut approvals = 0usize;
    let mut rx = handle.rx_event.write().await;
    loop {
        match tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::ApprovalRequired { id, .. } => {
                approvals += 1;
                handle.deny_tool_call(id).await.unwrap();
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => match result {
                Ok(res) => outcomes.push((model_call.provider_id, res.success, res.content)),
                Err(err) => outcomes.push((model_call.provider_id, false, err.to_string())),
            },
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();

    assert_eq!(approvals, 0);
    let list = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-list")
        .expect("list_bundles outcome");
    assert!(list.1, "list_bundles failed: {list:?}");

    let install = outcomes
        .iter()
        .find(|(id, _, _)| id == "call-install")
        .expect("install_bundle outcome");
    assert!(install.1, "install_bundle failed: {install:?}");
    let parsed: Value = serde_json::from_str(&install.2).expect("install JSON");
    assert_eq!(parsed["application"], "applied", "{parsed}");
    assert_ne!(parsed["application"], "not_available", "{parsed}");
    assert!(!install.2.contains("not_available"), "{}", install.2);
    assert_eq!(parsed["changed"], true, "{parsed}");
    assert_eq!(parsed["enabled"], true, "{parsed}");
    assert_eq!(parsed["bundle"], "demo-bundle", "{parsed}");
    assert!(user_plugins.join("demo-bundle/plugin.toml").exists());

    manager.shutdown().await;
}

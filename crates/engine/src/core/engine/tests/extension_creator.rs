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

async fn run_turns(
    config: EngineConfig,
    mock: Arc<MockLlmClient>,
    prompts: &[&str],
) -> Vec<String> {
    let api_config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(config, &api_config, mock);
    let task = tokio::spawn(engine.run());
    let mut statuses = Vec::new();
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
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
    }
    handle.send(Op::Shutdown).await.unwrap();
    task.await.unwrap();
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
    let parsed: codewhale_runtime::config::SkillsConfig = toml::from_str(
        "creator_skills = true\n[bundled]\nselected = [\"cordis-plugin-development\"]\n",
    )
    .unwrap();
    assert!(!parsed.creator_skills);
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
        let result = crate::tools::file::ReadFileTool
            .execute(input, &context)
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        let bundled = fs::read_to_string(file).unwrap();
        assert!(!bundled.is_empty());
        assert!(
            result.content.contains(bundled.lines().next().unwrap()),
            "{}: {}",
            file.display(),
            result.content
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
        .join("..")
        .join(".system-installed-version");
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
    let mut standard = engine_for(workspace.path(), None);
    standard.creator_available = true;
    assert_eq!(
        standard.effective_tools_always_load(),
        standard.config.tools_always_load
    );
    let mut unknown = engine_for(workspace.path(), Some("future-mode"));
    unknown.creator_available = true;
    assert_eq!(
        unknown.effective_tools_always_load(),
        unknown.config.tools_always_load
    );
    let mut child = engine_for(workspace.path(), Some("creator"));
    child.creator_available = true;
    assert!(child.conversation_loadout().creator_skills);
    child.creator_available = false;
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
    for (preset, visible) in [(Some("creator"), true), (None, false)] {
        let mut parent = engine_for(workspace.path(), preset);
        parent.creator_available = true;
        let api = Config::default();
        let route = TurnRouteContext {
            provider: ProviderKind::Deepseek,
            model: DEFAULT_TEXT_MODEL.into(),
            capabilities: Default::default(),
            limits: None,
            client: parent.codewhale_client.clone(),
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
        assert_eq!(context.skills_config.creator_skills, visible);
        let runtime = SubAgentRuntime::new(
            parent.codewhale_client.clone().unwrap(),
            DEFAULT_TEXT_MODEL.into(),
            context,
            true,
            None,
            parent.subagent_manager.clone(),
        );
        let authority = crate::tools::subagent::engine::ChildAuthority::capture(
            runtime,
            FleetRole::Scout,
            "child-a".into(),
            "inspect".into(),
            None,
        );
        let (child, _handle) = Engine::new_child_admitted(
            EngineConfig {
                workspace: workspace.path().to_path_buf(),
                model: DEFAULT_TEXT_MODEL.into(),
                ..Default::default()
            },
            &api,
            authority,
            SystemPrompt::Text("child".into()),
            None,
        )
        .unwrap();
        assert_eq!(child.conversation_skills_config().creator_skills, visible);
        assert_eq!(
            child.conversation_read_only_roots().len(),
            if visible { 4 } else { 0 }
        );
        let context = ToolContext::new(workspace.path())
            .with_skills_config(owned.clone(), child.conversation_skills_config());
        let loaded = crate::tools::skill::LoadSkillTool
            .execute(json!({"name": SKILL}), &context)
            .await;
        assert_eq!(loaded.is_ok(), visible);
    }
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

#[test]
fn default_active_contract_keeps_discovery_and_core_tools_eager() {
    // Token diet: agent+workflow (21KB of schema) and get/update_goal
    // (meaningless without an active goal) stay searchable by default.
    const EXPECTED_NATIVE: [&str; 9] = [
        "read",
        "write",
        "edit",
        "file_search",
        "bash",
        "todo_write",
        "task_shell_wait",
        "load_skill",
        "create_goal",
    ];
    assert_eq!(
        default_active_native_tool_names(),
        EXPECTED_NATIVE.as_slice()
    );

    let always_load = HashSet::new();
    let mut catalog = build_model_tool_catalog(
        EXPECTED_NATIVE.into_iter().map(api_tool).collect(),
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
    let active = initial_active_tools(&catalog);
    let expected = EXPECTED_NATIVE
        .into_iter()
        .chain([TOOL_SEARCH_NAME])
        .map(str::to_string)
        .collect::<HashSet<_>>();

    assert_eq!(active, expected);
    assert_eq!(
        catalog
            .iter()
            .find(|tool| tool.name == TOOL_SEARCH_NAME)
            .and_then(|tool| tool.defer_loading),
        Some(false)
    );
}

#[test]
fn non_yolo_mode_retains_default_defer_policy() {
    let always_load = HashSet::new();
    // Token diet: agent stays searchable even on wide surfaces.
    for core in ["read", "write", "edit", "bash", "todo_write", "load_skill"] {
        assert!(!should_default_defer_tool(core, &always_load));
    }
    for searchable in [
        "Bash",
        "File",
        "Git",
        "Run",
        "remember",
        REQUEST_USER_INPUT_NAME,
        "read_file",
        "edit_file",
        "apply_patch",
        "git_status",
        "git_blame",
        "run_tests",
        "web_search",
    ] {
        assert!(should_default_defer_tool(searchable, &always_load));
    }
}

#[test]
fn default_defer_lookup_matches_linear_scan_over_active_native_tools() {
    // Parity guard for #4152: `should_default_defer_tool` now consults an O(1)
    // side set built from DEFAULT_ACTIVE_NATIVE_TOOLS instead of a linear
    // `.iter().any(...)` scan. Assert the set returns the SAME hit/miss as an
    // explicit linear scan over the ordered array — every array member is a hit
    // (not deferred); names outside the array miss (deferred by default).
    let always_load = HashSet::new();
    let active = default_active_native_tool_names();

    for name in active {
        // Reference linear scan == what the converted lookup must agree with.
        let linear_hit = active.iter().any(|core| core == name);
        assert!(linear_hit, "reference scan should find array member {name}");
        assert!(
            !should_default_defer_tool(name, &always_load),
            "array member {name} must stay active (not deferred)"
        );
    }

    for name in [
        "git_blame",
        "task_shell_start",
        REQUEST_USER_INPUT_NAME,
        "definitely_not_a_tool",
    ] {
        let linear_hit = active.contains(&name);
        assert!(!linear_hit, "non-member {name} should be absent from array");
        assert!(
            should_default_defer_tool(name, &always_load),
            "non-member {name} must default to deferred"
        );
    }
}

#[test]
fn model_tool_catalog_applies_native_and_mcp_deferral() {
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog(
        vec![
            api_tool("read"),
            api_tool("write"),
            api_tool("edit"),
            api_tool("bash"),
            api_tool("agent"),
            api_tool("Git"),
            api_tool("Run"),
            api_tool("remember"),
            api_tool("project_map"),
        ],
        vec![api_tool("list_mcp_resources"), api_tool("mcp_server_write")],
        AppMode::Agent,
        &always_load,
    );

    let defer_loading = |name: &str| {
        catalog
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.defer_loading)
    };

    for core in ["read", "write", "edit", "bash", "agent"] {
        assert_eq!(defer_loading(core), Some(false));
    }
    assert_eq!(defer_loading("Git"), Some(true));
    assert_eq!(defer_loading("Run"), Some(true));
    assert_eq!(defer_loading("remember"), Some(true));
    assert_eq!(defer_loading("project_map"), Some(true));
    assert_eq!(defer_loading("list_mcp_resources"), Some(true));
    assert_eq!(defer_loading("mcp_server_write"), Some(true));
}

#[test]
fn registry_sync_results_are_bounded_like_every_other_tool() {
    // The full-catalog bypass is gone: an oversized registry payload flows
    // through the same route budget as any other tool result.
    let budget = crate::route_budget::route_inline_char_budget_for_route(
        ProviderKind::Deepseek,
        "small-context-model",
        None,
    );
    let raw = format!(
        "{{\"instruction\":\"compare all\",\"servers\":[{{\"name\":\"{}\"}}]}}",
        "a".repeat(budget + 1_000),
    );
    let output = ToolResult::success(raw.clone());

    let context = compact_tool_result_for_route(
        ProviderKind::Deepseek,
        "small-context-model",
        None,
        "registry_sync",
        &output,
    );

    assert_ne!(context, raw);
    assert!(context.chars().count() <= budget);
    assert!(context.contains(crate::tools::truncate::SPILLOVER_RECOVERY_HINT));
}

#[test]
fn capability_compact_surface_defers_nonessential_core_tools() {
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog_with_surface(
        vec![
            api_tool("read"),
            api_tool("write"),
            api_tool("edit"),
            api_tool("bash"),
            api_tool("agent"),
            api_tool("Git"),
            api_tool("Run"),
            api_tool(TOOL_SEARCH_NAME),
            api_tool("update_plan"),
            api_tool("Web"),
        ],
        vec![api_tool("list_mcp_resources"), api_tool("mcp_server_write")],
        AppMode::Agent,
        &always_load,
        crate::model_profile::ToolSurfaceBudget::Compact,
    );

    let defer_loading = |name: &str| {
        catalog
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.defer_loading)
    };

    for core in ["read", "write", "edit", "bash", "agent"] {
        assert_eq!(defer_loading(core), Some(false));
    }
    assert_eq!(defer_loading("Git"), Some(true));
    assert_eq!(defer_loading("update_plan"), Some(true));
    assert_eq!(defer_loading(TOOL_SEARCH_NAME), Some(false));
    assert_eq!(defer_loading("list_mcp_resources"), Some(true));
    assert_eq!(defer_loading("Run"), Some(true));
    assert_eq!(defer_loading("Web"), Some(true));
    assert_eq!(defer_loading("mcp_server_write"), Some(true));
}

#[test]
fn capability_full_surface_preserves_small_default_head() {
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog_with_surface(
        vec![
            api_tool("read"),
            api_tool("write"),
            api_tool("edit"),
            api_tool("bash"),
            api_tool("agent"),
            api_tool("Run"),
        ],
        Vec::new(),
        AppMode::Agent,
        &always_load,
        crate::model_profile::ToolSurfaceBudget::Full,
    );

    for name in ["read", "write", "edit", "bash", "agent"] {
        assert_eq!(
            catalog
                .iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.defer_loading),
            Some(false),
            "{name} should stay eager on full tool surfaces"
        );
    }
    assert_eq!(
        catalog
            .iter()
            .find(|tool| tool.name == "Run")
            .and_then(|tool| tool.defer_loading),
        Some(true)
    );
}

#[test]
fn plugin_or_benchmark_tools_remain_searchable_not_eager() {
    let always_load = HashSet::new();
    let mut catalog = build_model_tool_catalog(
        vec![api_tool("KB_search"), api_tool("read")],
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

    let active = initial_active_tools(&catalog);
    assert!(!active.contains("KB_search"));
    assert!(active.contains("read"));
    assert_eq!(
        catalog
            .iter()
            .find(|tool| tool.name == "KB_search")
            .and_then(|tool| tool.defer_loading),
        Some(true)
    );
}

#[tokio::test]
async fn registry_discovery_and_start_handlers_exist_in_agent_and_plan_modes() {
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    engine.ensure_mcp_pool().await.expect("initialize MCP pool");

    for mode in [AppMode::Agent, AppMode::Plan] {
        let registry = engine
            .build_turn_tool_registry_builder(
                mode,
                engine.config.todos.clone(),
                engine.config.plan_state.clone(),
            )
            .build(engine.build_tool_context(mode, false));
        assert!(registry.contains("registry_sync"), "missing in {mode:?}");
        assert!(registry.contains("read_media"), "missing in {mode:?}");
        let media_tools = registry
            .to_api_tools_with_cache(true)
            .into_iter()
            .filter(|tool| tool.name == "read_media")
            .collect::<Vec<_>>();
        assert_eq!(
            media_tools.len(),
            1,
            "read_media must be registered exactly once in {mode:?}"
        );
        assert_eq!(
            media_tools[0].defer_loading,
            Some(true),
            "read_media must remain default-off/deferred in {mode:?}"
        );
        assert!(
            registry.contains("start_registry_mcp_server"),
            "missing in {mode:?}"
        );
        assert!(!registry.contains("registry_install_run_info"));
    }
}

#[test]
fn catalog_consistency_self_check_flags_registered_core_tool_missing_from_catalog() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let registry = engine
        .build_turn_tool_registry_builder(
            AppMode::Agent,
            engine.config.todos.clone(),
            engine.config.plan_state.clone(),
        )
        .build(engine.build_tool_context(AppMode::Agent, false));
    let always_load = HashSet::new();
    let mut catalog = build_model_tool_catalog(
        registry.to_api_tools_with_cache(true),
        vec![],
        AppMode::Agent,
        &always_load,
    );
    catalog.retain(|tool| tool.name != "read");

    let issues = tool_catalog_consistency_issues(&catalog, &registry);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("registered core tool 'read'")),
        "missing registered read should be reported: {issues:?}"
    );
}

fn assert_exec_shell_is_not_discoverable(match_kind: &str) {
    let catalog = vec![api_tool("read_file")];
    let mut active = initial_active_tools(&catalog);

    let result = execute_tool_search(
        TOOL_SEARCH_NAME,
        &json!({ "query": "exec_shell", "match": match_kind }),
        &catalog,
        &mut active,
    )
    .expect("tool search succeeds");

    assert!(!active.contains("exec_shell"));
    let metadata = result.metadata.as_ref().expect("search metadata");
    let references = metadata["tool_references"]
        .as_array()
        .expect("tool references are an array");
    assert!(
        references
            .iter()
            .all(|reference| reference.as_str() != Some("exec_shell")),
        "legacy shell alias must not surface via {match_kind}: {references:?}"
    );
    let unavailable = metadata["unavailable_tool_references"]
        .as_array()
        .expect("unavailable references are an array");
    assert!(
        unavailable
            .iter()
            .all(|reference| reference["tool_name"].as_str() != Some("exec_shell")),
        "legacy shell alias must not surface as an unavailable fallback via {match_kind}: {unavailable:?}"
    );
}

#[test]
fn regex_tool_search_does_not_discover_hidden_exec_shell_alias() {
    assert_exec_shell_is_not_discoverable("regex");
}

#[test]
fn bm25_tool_search_does_not_discover_hidden_exec_shell_alias() {
    assert_exec_shell_is_not_discoverable("bm25");
}

#[test]
fn tools_always_scenario() {
    // Scenario consolidation of: tools_always_load_overrides_mcp_deferral, tools_always_load_overrides_default_native_deferral
    // from tools_always_load_overrides_mcp_deferral
    {
        let always_load = HashSet::from(["mcp_server_write".to_string()]);
        let catalog = build_model_tool_catalog(
            vec![api_tool("read_file")],
            vec![api_tool("mcp_server_write")],
            AppMode::Agent,
            &always_load,
        );
        let mcp = catalog
            .iter()
            .find(|tool| tool.name == "mcp_server_write")
            .expect("mcp tool");
        assert_eq!(mcp.defer_loading, Some(false));
    }
    // from tools_always_load_overrides_default_native_deferral
    {
        let always_load = HashSet::from(["git_blame".to_string()]);
        assert!(!should_default_defer_tool("git_blame", &always_load));
    }
}

fn tool_catalog_surface_metrics(catalog: &[Tool]) -> serde_json::Value {
    let serialized = serde_json::to_vec(catalog).expect("serialize canonical tool catalog");
    let mut tool_names = catalog
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    tool_names.sort();
    let identity_sha256 = crate::hashing::sha256_hex(tool_names.join("\0").as_bytes());
    serde_json::json!({
        "tools": catalog.len(),
        "bytes": serialized.len(),
        "tokens_est": serialized.len().div_ceil(4),
        "tool_names": tool_names,
        "identity_sha256": identity_sha256,
    })
}

async fn measure_production_mode_tool_catalogs() -> serde_json::Value {
    let _env_lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).expect("create isolated home");
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale"));
    // Interpreter-backed advanced tools are intentionally excluded from this
    // cross-platform built-in profile. Pin host capabilities because Windows
    // system-directory lookup can find interpreters outside PATH. Keep the
    // nonexistent PATH root for the canonical shell spelling below.
    let _host_tools = crate::dependencies::pin_recorded_host_tools(Vec::new());
    let _path = EnvVarGuard::set("PATH", tmp.path().join("no-host-interpreters"));
    // Shell syntax is part of the tool schema. A bare name plus the empty
    // PATH resolves to the same `bash` spelling on Unix and Windows without
    // executing or requiring that interpreter. Do not inherit the host shell.
    let _shell = EnvVarGuard::set("SHELL", "bash");
    // Keep the explicit OCR-unavailable environment for framework probes that
    // ignore PATH; the host capability pin above also excludes local OCR from
    // this profile.
    let _ocr = EnvVarGuard::set("CODEWHALE_LOCAL_OCR_UNAVAILABLE", "1");

    let api_config = Config {
        default_text_model: Some(DEFAULT_TEXT_MODEL.to_string()),
        ..Config::default()
    }
    .with_legacy_root(Some("local-runtime-contract-fixture".to_string()), None);
    let mut mode_metrics = serde_json::Map::new();
    for (mode_name, mode) in [
        ("plan", AppMode::Plan),
        ("act", AppMode::Agent),
        ("operate", AppMode::Operate),
    ] {
        let workspace = tmp.path().join(mode_name);
        fs::create_dir_all(&workspace).expect("create isolated mode workspace");
        let engine_config = EngineConfig {
            workspace,
            allow_shell: true,
            ..EngineConfig::default()
        };
        let (mut engine, _handle) = Engine::new(engine_config, &api_config);
        // MCP catalogs depend on configured external servers. This receipt owns
        // the canonical provider-free built-in profile and exercises the same
        // production builder/planner with MCP explicitly disabled.
        engine.config.features.disable(Feature::Mcp);
        let route = TurnRouteContext {
            provider: ProviderKind::Deepseek,
            model: DEFAULT_TEXT_MODEL.to_string(),
            capabilities: codewhale_config::route::RouteCapabilities::default(),
            limits: None,
            client: engine.codewhale_client.clone(),
            api_config: Box::new(api_config.clone()),
            locale_tag: engine.config.locale_tag.clone(),
            role_models: engine.subagent_role_models(),
            auto_model: false,
            reasoning_effort: None,
            reasoning_effort_auto: false,
        };
        let policy = crate::core::authority::TurnAuthority::from_effective_fields(
            mode,
            true,
            false,
            false,
            ApprovalMode::Suggest,
        );
        let build = engine
            .build_turn_tool_registry_and_catalog(
                &policy,
                &[],
                None,
                SubAgentWiring::Inert,
                McpAccess::PassiveSnapshot,
                route,
                "",
                None,
            )
            .await;
        let active = build.surface.active.clone().unwrap_or_default();
        mode_metrics.insert(
            mode_name.to_string(),
            serde_json::json!({
                "full": tool_catalog_surface_metrics(&build.surface.catalog),
                "active": tool_catalog_surface_metrics(&active),
            }),
        );
    }

    serde_json::json!({
        "surface_profile": "production-default-builtins-no-mcp-no-host-interpreters-bash-v2",
        "execution_shell": crate::shell_dispatcher::global_dispatcher().kind().binary(),
        "modes": mode_metrics,
    })
}

fn metric_tool_names<'a>(
    payload: &'a serde_json::Value,
    mode: &str,
    surface: &str,
) -> HashSet<&'a str> {
    payload["modes"][mode][surface]["tool_names"]
        .as_array()
        .expect("tool names array")
        .iter()
        .map(|name| name.as_str().expect("tool name string"))
        .collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn runtime_contract_tool_metric_uses_canonical_mode_surfaces() {
    let payload = measure_production_mode_tool_catalogs().await;
    let expected_active = HashSet::from([
        "agent",
        "bash",
        "create_goal",
        "get_goal",
        "update_goal",
        "task_shell_wait",
        "edit",
        "file_search",
        "read",
        "todo_write",
        "tool_search",
        "workflow",
        "write",
        "load_skill",
    ]);

    for mode in ["plan", "act", "operate"] {
        let full = metric_tool_names(&payload, mode, "full");
        for required in [
            "read",
            "write",
            "edit",
            "bash",
            "agent",
            "workflow",
            "tool_search",
            "create_goal",
            "get_goal",
            "update_goal",
        ] {
            assert!(full.contains(required), "{mode} must include {required}");
        }
        for hidden in ["File", "Bash", "read_file", "write_file", "edit_file"] {
            assert!(!full.contains(hidden), "{mode} must hide {hidden}");
        }
        // #6562: `[features] code_mode` defaults on, so Act/Operate promote
        // `execute_tools` into the request head. Plan hides it entirely and
        // registers only the foreground shell surface, so Plan omits
        // `task_shell_wait`.
        let mut expected_mode_active = expected_active.clone();
        if mode != "plan" {
            expected_mode_active.insert("execute_tools");
        } else {
            expected_mode_active.remove("task_shell_wait");
        }
        assert_eq!(
            metric_tool_names(&payload, mode, "active"),
            expected_mode_active,
            "{mode} must expose the expected request head"
        );
    }

    let plan = metric_tool_names(&payload, "plan", "full");
    for forbidden in ["Run", "fim_edit", "verify", "execute_tools"] {
        assert!(!plan.contains(forbidden), "Plan must exclude {forbidden}");
    }

    for mode in ["act", "operate"] {
        let full = metric_tool_names(&payload, mode, "full");
        for required in ["Run", "verify"] {
            assert!(full.contains(required), "{mode} must include {required}");
        }
        assert!(
            !full.contains("fim_edit"),
            "{mode} must exclude FIM on its unsupported route"
        );
    }
}

#[tokio::test]
#[ignore = "one-shot metric for scripts/measure-tool-catalog.py"]
#[allow(clippy::await_holding_lock)]
#[allow(clippy::print_stdout)]
async fn print_mode_tool_catalog_metrics() {
    let metrics = measure_production_mode_tool_catalogs().await;
    assert_eq!(
        metrics["execution_shell"], "bash",
        "run this exact metric in a fresh process so its shell fixture owns dispatcher initialization"
    );
    println!("TOOL_CATALOG_METRICS {metrics}");
}

#[test]
fn runtime_contract_tool_metric_is_independent_of_inherited_shell() {
    let metric = "core::engine::tests::print_mode_tool_catalog_metrics";
    let samples: Vec<serde_json::Value> = ["/bin/zsh", "pwsh"]
        .into_iter()
        .map(|shell| {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    metric,
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("SHELL", shell)
                .output()
                .expect("run exact metric in a fresh process");
            assert!(
                output.status.success(),
                "metric failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("metric UTF-8")
                .lines()
                .find_map(|line| {
                    line.split_once("TOOL_CATALOG_METRICS ")
                        .map(|(_, payload)| serde_json::from_str(payload).expect("metric JSON"))
                })
                .expect("metric marker")
        })
        .collect();
    assert_eq!(samples[0], samples[1], "host shell changed the fixture");
    assert_eq!(samples[0]["execution_shell"], "bash");
}

#[test]
#[ignore = "one-shot metric for scripts/measure-runtime-contract.py"]
#[allow(clippy::print_stdout)]
fn print_mode_runtime_contract_metrics() {
    let _env_lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    fs::create_dir_all(&workspace).expect("create isolated workspace");
    fs::create_dir_all(&home).expect("create isolated home");
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let codewhale_home = home.join(".codewhale");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    // Keep the model-visible shell fact stable across developer and CI hosts
    // while exercising the exact-path contract used at runtime.
    let _shell = EnvVarGuard::set("SHELL", "/bin/bash");
    let mut mode_metrics = serde_json::Map::new();
    for (mode_name, mode) in [
        ("plan", AppMode::Plan),
        ("act", AppMode::Agent),
        ("operate", AppMode::Operate),
    ] {
        let prompt = system_prompt_for_mode_with_context_skills_and_session(
            &workspace,
            None,
            None,
            None,
            PromptSessionContext {
                mode,
                ..PromptSessionContext::default()
            },
        );
        let prompt_bytes = system_prompt_flat_text(&prompt).len();
        let prompt_blocks = match &prompt {
            codewhale_models::SystemPrompt::Blocks(blocks) => blocks.len(),
            _ => 1,
        };
        mode_metrics.insert(
            mode_name.to_string(),
            serde_json::json!({
                "system_prompt_bytes": prompt_bytes,
                "system_prompt_tokens_est": prompt_bytes.div_ceil(4),
                "system_prompt_blocks": prompt_blocks,
                "mode_instructions_bytes": 0,
                "mode_instructions_tokens_est": 0,
            }),
        );
    }

    println!(
        "RUNTIME_CONTRACT_METRICS {}",
        serde_json::json!({
            "modes": mode_metrics,
        })
    );
}

fn representative_prompt(
    workspace: &Path,
    skills_dir: &Path,
    instructions: Option<&[InstructionSource]>,
    user_memory_block: Option<&str>,
    goal_objective: Option<&str>,
) -> codewhale_models::SystemPrompt {
    system_prompt_for_mode_with_context_skills_and_session(
        workspace,
        None,
        Some(skills_dir),
        instructions,
        PromptSessionContext {
            user_memory_block,
            goal_objective,
            skills_discovery_mode: crate::skills::SkillDiscoveryMode::CodeWhaleOnly,
            mode: AppMode::Agent,
            ..PromptSessionContext::default()
        },
    )
}

fn prompt_block_count(prompt: &codewhale_models::SystemPrompt) -> usize {
    match prompt {
        codewhale_models::SystemPrompt::Blocks(blocks) => blocks.len(),
        codewhale_models::SystemPrompt::Text(_) => 1,
    }
}

#[derive(Debug)]
struct RepresentativePromptStage {
    name: &'static str,
    flat: String,
    normalized: String,
}

fn normalize_representative_prompt(text: &str, workspace: &Path, home: &Path) -> String {
    let mut replacements = Vec::new();
    for (path, replacement) in [(workspace, "<WORKSPACE>"), (home, "<HOME>")] {
        replacements.push((path.to_path_buf(), replacement));
        if let Ok(canonical) = path.canonicalize() {
            replacements.push((canonical, replacement));
        }
    }
    replacements.sort_by(|(left, _), (right, _)| {
        right
            .to_string_lossy()
            .len()
            .cmp(&left.to_string_lossy().len())
            .then_with(|| left.cmp(right))
    });
    replacements.dedup_by(|(left, _), (right, _)| left == right);

    let normalized =
        replacements
            .into_iter()
            .fold(text.to_string(), |normalized, (path, replacement)| {
                normalized.replace(path.to_string_lossy().as_ref(), replacement)
            });
    // Platform remains an actionable host fact in the stable environment
    // block. Pin it so this fixture measures prompt structure across hosts.
    normalized.replace(
        &format!("- platform: {}", std::env::consts::OS),
        "- platform: <PLATFORM>",
    )
}

fn representative_stage(
    name: &'static str,
    prompt: codewhale_models::SystemPrompt,
    workspace: &Path,
    home: &Path,
) -> RepresentativePromptStage {
    let flat = system_prompt_flat_text(&prompt);
    let normalized = normalize_representative_prompt(&flat, workspace, home);
    RepresentativePromptStage {
        name,
        flat,
        normalized,
    }
}

fn measure_representative_runtime_context()
-> (serde_json::Value, Vec<RepresentativePromptStage>, String) {
    let _env_lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    let skills_dir = workspace.join(".codewhale").join("skills");
    fs::create_dir_all(&workspace).expect("create isolated workspace");
    fs::create_dir_all(&home).expect("create isolated home");

    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let codewhale_home = home.join(".codewhale");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    // Keep the model-visible shell fact stable across developer and CI hosts
    // while exercising the exact-path contract used at runtime.
    let _shell = EnvVarGuard::set("SHELL", "/bin/bash");
    // The fixture measures a trusted repository: project skills load only in
    // a trusted workspace, and the skill stage exists to measure one.
    crate::test_support::trust_workspace(&workspace);

    let mut stages = vec![representative_stage(
        "base",
        representative_prompt(&workspace, &skills_dir, None, None, None),
        &workspace,
        &home,
    )];

    fs::write(
        workspace.join("AGENTS.md"),
        REPRESENTATIVE_PROJECT_AUTHORITY_BODY,
    )
    .expect("write representative project authority");
    stages.push(representative_stage(
        "project",
        representative_prompt(&workspace, &skills_dir, None, None, None),
        &workspace,
        &home,
    ));

    let instructions = [InstructionSource::Inline {
        name: "embedded:representative-v1".to_string(),
        content: REPRESENTATIVE_INLINE_INSTRUCTIONS.to_string(),
    }];
    stages.push(representative_stage(
        "instructions",
        representative_prompt(&workspace, &skills_dir, Some(&instructions), None, None),
        &workspace,
        &home,
    ));

    let skill_dir = skills_dir.join("representative-skill");
    fs::create_dir_all(&skill_dir).expect("create representative skill directory");
    fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: representative-skill\ndescription: {REPRESENTATIVE_SKILL_DESCRIPTION}\n---\nExercise the deterministic runtime-contract fixture.\n"
        ),
    )
    .expect("write representative skill");
    stages.push(representative_stage(
        "skill",
        representative_prompt(&workspace, &skills_dir, Some(&instructions), None, None),
        &workspace,
        &home,
    ));

    let memory_block = format!("## Memory\n\n- {REPRESENTATIVE_MEMORY_CHECKPOINT}");
    stages.push(representative_stage(
        "memory",
        representative_prompt(
            &workspace,
            &skills_dir,
            Some(&instructions),
            Some(&memory_block),
            None,
        ),
        &workspace,
        &home,
    ));
    stages.push(representative_stage(
        "goal",
        representative_prompt(
            &workspace,
            &skills_dir,
            Some(&instructions),
            Some(&memory_block),
            Some(REPRESENTATIVE_GOAL_OBJECTIVE),
        ),
        &workspace,
        &home,
    ));

    fs::write(
        workspace.join(crate::prompts::HANDOFF_RELATIVE_PATH),
        format!("# Representative Relay\n\n{REPRESENTATIVE_HANDOFF_RELAY}\n"),
    )
    .expect("write representative handoff");
    let final_prompt = representative_prompt(
        &workspace,
        &skills_dir,
        Some(&instructions),
        Some(&memory_block),
        Some(REPRESENTATIVE_GOAL_OBJECTIVE),
    );
    let final_blocks = prompt_block_count(&final_prompt);
    stages.push(representative_stage(
        "handoff",
        final_prompt,
        &workspace,
        &home,
    ));
    let repeated_flat = system_prompt_flat_text(&representative_prompt(
        &workspace,
        &skills_dir,
        Some(&instructions),
        Some(&memory_block),
        Some(REPRESENTATIVE_GOAL_OBJECTIVE),
    ));

    let mut stage_metrics = serde_json::Map::new();
    for (index, stage) in stages.iter().enumerate() {
        let mut metrics = serde_json::json!({
            // Keep byte ceilings host-independent just like the structural
            // identity: temporary workspace/home paths vary across runners.
            "bytes": stage.normalized.len(),
            "identity_sha256": crate::hashing::sha256_hex(stage.normalized.as_bytes()),
        });
        if let Some(previous) = index.checked_sub(1).and_then(|i| stages.get(i)) {
            metrics["delta_bytes"] = serde_json::json!(
                stage
                    .normalized
                    .len()
                    .checked_sub(previous.normalized.len())
                    .unwrap_or_else(|| panic!(
                        "representative {} stage unexpectedly shrank prompt",
                        stage.name
                    ))
            );
        }
        stage_metrics.insert(stage.name.to_string(), metrics);
    }
    let final_stage = stages.last().expect("handoff stage");
    let payload = serde_json::json!({
        "fixture_id": REPRESENTATIVE_FIXTURE_ID,
        "stages": stage_metrics,
        "total_bytes": final_stage.normalized.len(),
        "total_tokens_est": final_stage.normalized.len().div_ceil(4),
        "system_prompt_blocks": final_blocks,
        "prompts_byte_identical": final_stage.flat == repeated_flat,
    });

    (payload, stages, repeated_flat)
}

#[test]
fn representative_runtime_context_fixture_is_stable_and_contains_expected_markers() {
    let (payload, stages, repeated_prompt) = measure_representative_runtime_context();
    let (second_payload, second_stages, _) = measure_representative_runtime_context();
    let final_stage = stages.last().expect("handoff stage");
    assert_eq!(payload["fixture_id"], REPRESENTATIVE_FIXTURE_ID);
    assert_eq!(final_stage.flat, repeated_prompt);
    assert_eq!(payload["prompts_byte_identical"], true);
    for (first, second) in stages.iter().zip(&second_stages) {
        assert_eq!(first.name, second.name);
        assert_eq!(first.normalized, second.normalized);
        assert_eq!(
            payload["stages"][first.name]["identity_sha256"],
            second_payload["stages"][second.name]["identity_sha256"],
            "representative {} digest must be stable across temp roots",
            first.name
        );
    }
    for pair in stages.windows(2) {
        let [previous, current] = pair else {
            unreachable!("stage windows always contain two entries")
        };
        assert_eq!(
            payload["stages"][current.name]["delta_bytes"],
            current.normalized.len() - previous.normalized.len(),
            "representative {} delta must be computed from its adjacent stages",
            current.name
        );
    }
    let markers = [
        REPRESENTATIVE_PROJECT_AUTHORITY,
        REPRESENTATIVE_INLINE_INSTRUCTIONS,
        REPRESENTATIVE_SKILL_DESCRIPTION,
        REPRESENTATIVE_MEMORY_CHECKPOINT,
        REPRESENTATIVE_GOAL_OBJECTIVE,
        REPRESENTATIVE_HANDOFF_RELAY,
    ];
    for (stage_index, stage) in stages.iter().enumerate() {
        for (marker_index, marker) in markers.iter().enumerate() {
            let expected = usize::from(marker_index < stage_index);
            assert_eq!(
                stage.flat.matches(marker).count(),
                expected,
                "representative {} stage has the wrong count for {marker}",
                stage.name
            );
        }
    }
    let fixture_sources = [
        REPRESENTATIVE_PROJECT_AUTHORITY,
        REPRESENTATIVE_INLINE_INSTRUCTIONS,
        REPRESENTATIVE_SKILL_DESCRIPTION,
        REPRESENTATIVE_MEMORY_CHECKPOINT,
        REPRESENTATIVE_GOAL_OBJECTIVE,
        REPRESENTATIVE_HANDOFF_RELAY,
    ]
    .join("\n")
    .to_ascii_lowercase();
    for secret_shape in ["sk-", "api_key=", "password=", "bearer "] {
        assert!(
            !fixture_sources.contains(secret_shape),
            "representative fixture must not contain secret-shaped values"
        );
    }
}

#[test]
#[ignore = "one-shot metric for scripts/measure-runtime-contract.py"]
#[allow(clippy::print_stdout)]
fn print_representative_runtime_context_metrics() {
    let (payload, _, _) = measure_representative_runtime_context();
    println!("REPRESENTATIVE_CONTEXT_METRICS {payload}");
}

fn measure_unchanged_prompt_skill_discovery() -> (
    crate::skills::SkillDiscoveryMetrics,
    crate::skills::SkillDiscoveryMetrics,
    bool,
) {
    let _env_lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    let skills_dir = workspace.join(".codewhale").join("skills");
    let skill = skills_dir.join("receipt-demo");
    fs::create_dir_all(&skill).expect("create skill directory");
    fs::create_dir_all(&home).expect("create isolated home");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: receipt-demo\ndescription: Hermetic measurement skill\n---\nMeasure discovery.\n",
    )
    .expect("write skill");

    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let codewhale_home = home.join(".codewhale");
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    crate::test_support::trust_workspace(&workspace);

    crate::skills::clear_skill_discovery_cache();
    crate::skills::reset_discovery_metrics();
    let start = crate::skills::discovery_metrics_snapshot();
    let first_prompt = system_prompt_for_mode_with_context_skills_and_session(
        &workspace,
        None,
        Some(&skills_dir),
        None,
        PromptSessionContext::default(),
    );
    let after_first = crate::skills::discovery_metrics_snapshot();
    let second_prompt = system_prompt_for_mode_with_context_skills_and_session(
        &workspace,
        None,
        Some(&skills_dir),
        None,
        PromptSessionContext::default(),
    );
    let after_second = crate::skills::discovery_metrics_snapshot();

    let first = after_first.delta_since(start);
    let second = after_second.delta_since(after_first);
    let first_flat = system_prompt_flat_text(&first_prompt);
    let second_flat = system_prompt_flat_text(&second_prompt);
    (first, second, first_flat == second_flat)
}

#[test]
fn unchanged_prompt_skill_discovery_baseline_caches_the_second_turn() {
    let (first, second, prompts_byte_identical) = measure_unchanged_prompt_skill_discovery();
    let first_expected = crate::skills::SkillDiscoveryMetrics {
        root_discovery_calls: 1,
        directories_visited: 1,
        skill_md_read_attempts: 1,
    };
    assert_eq!(first, first_expected);
    assert_eq!(second, crate::skills::SkillDiscoveryMetrics::default());
    assert!(prompts_byte_identical);
}

fn skill_discovery_metric_payload(
    first: crate::skills::SkillDiscoveryMetrics,
    second: crate::skills::SkillDiscoveryMetrics,
    prompts_byte_identical: bool,
) -> serde_json::Value {
    serde_json::json!({
        "first_delta": {
            "root_discovery_calls": first.root_discovery_calls,
            "directories_visited": first.directories_visited,
            "skill_md_read_attempts": first.skill_md_read_attempts,
        },
        "second_delta": {
            "root_discovery_calls": second.root_discovery_calls,
            "directories_visited": second.directories_visited,
            "skill_md_read_attempts": second.skill_md_read_attempts,
        },
        "prompts_byte_identical": prompts_byte_identical,
    })
}

#[test]
fn skill_discovery_metric_payload_accepts_cached_second_turn() {
    let first = crate::skills::SkillDiscoveryMetrics {
        root_discovery_calls: 1,
        directories_visited: 1,
        skill_md_read_attempts: 1,
    };
    let payload = skill_discovery_metric_payload(
        first,
        crate::skills::SkillDiscoveryMetrics::default(),
        true,
    );
    assert_eq!(payload["first_delta"]["root_discovery_calls"], 1);
    assert_eq!(payload["second_delta"]["root_discovery_calls"], 0);
    assert_eq!(payload["second_delta"]["directories_visited"], 0);
    assert_eq!(payload["second_delta"]["skill_md_read_attempts"], 0);
}

#[test]
#[ignore = "one-shot metric for scripts/measure-runtime-contract.py"]
#[allow(clippy::print_stdout)]
fn print_skill_discovery_turn_metrics() {
    let (first, second, prompts_byte_identical) = measure_unchanged_prompt_skill_discovery();

    println!(
        "SKILL_DISCOVERY_METRICS {}",
        skill_discovery_metric_payload(first, second, prompts_byte_identical)
    );
}

#[test]
fn deferred_first_use_executes_well_formed_calls_and_hydrates_malformed_ones() {
    let mut apply_patch = api_tool("apply_patch");
    apply_patch.defer_loading = Some(true);
    apply_patch.input_schema = json!({
        "type": "object",
        "properties": {
            "patch": { "type": "string" }
        },
        "required": ["patch"]
    });

    let catalog = vec![apply_patch];
    let active_at_batch_start = HashSet::new();
    let mut hydrated_this_batch = HashSet::new();
    // A call already shaped like the unseen schema must not lose its turn.
    assert!(
        maybe_hydrate_requested_deferred_tool(
            "apply_patch",
            &json!({"patch": "*** Begin Patch\n*** End Patch"}),
            &catalog,
            &active_at_batch_start,
            &mut hydrated_this_batch,
        )
        .is_none(),
        "a well-formed first call executes"
    );
    assert!(
        hydrated_this_batch.contains("apply_patch"),
        "the executed tool still activates for later requests"
    );

    for malformed in [
        json!({}),
        json!({"diff": "*** Begin Patch\n*** End Patch"}),
        json!({"patch": "x", "path": "src/lib.rs"}),
        json!("*** Begin Patch"),
    ] {
        let mut hydrated = HashSet::new();
        let result = maybe_hydrate_requested_deferred_tool(
            "apply_patch",
            &malformed,
            &catalog,
            &active_at_batch_start,
            &mut hydrated,
        )
        .unwrap_or_else(|| panic!("{malformed} must return the schema instead of executing"));
        assert!(hydrated.contains("apply_patch"));
        assert!(result.success);
        assert!(result.content.contains("Tool `apply_patch` was deferred"));
        assert!(result.content.contains("patch: string"));
        assert!(result.content.contains("The tool was not executed"));
        let metadata = result.metadata.expect("metadata");
        assert_eq!(metadata["event"], "tool.schema_hydrated");
        assert_eq!(metadata["executed"], false);
        assert_eq!(metadata["retry_required"], true);
    }

    let mut active_next_batch = active_at_batch_start.clone();
    active_next_batch.extend(hydrated_this_batch);
    let mut hydrated_next_batch = HashSet::new();
    assert!(
        maybe_hydrate_requested_deferred_tool(
            "apply_patch",
            &json!({}),
            &catalog,
            &active_next_batch,
            &mut hydrated_next_batch,
        )
        .is_none(),
        "tools hydrated in a previous batch execute normally, even malformed"
    );
}

/// E3: the first call to a deferred tool hydrates its schema and tells the
/// model to retry. That hint is model-facing; it reaches the model in the
/// tool result and must not surface as a user status line.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn deferred_tool_first_use_does_not_emit_a_retry_status() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-e3\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_e3_map\",\"type\":\"function\",\"function\":{\"name\":\"project_map\",",
        // Malformed on purpose: a well-formed first call now executes, and
        // this test covers the schema hint returned for a malformed one.
        "\"arguments\":\"{\\\"not_a_project_map_field\\\":true}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-e3\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-e3-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-e3-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("call_e3_map"))
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

    let api_config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(
        EngineConfig {
            model: crate::config::DEFAULT_TEXT_MODEL.to_string(),
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
            content: "Map this project".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, crate::config::DEFAULT_TEXT_MODEL),
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

    let mut hydration_result = None;
    let mut statuses = Vec::new();
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for turn event")
    {
        match event {
            Event::Status { message, .. } => statuses.push(message),
            Event::ToolCallComplete { name, result, .. } if name == "project_map" => {
                hydration_result = Some(result);
            }
            Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    drop(rx);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");

    let hydration = hydration_result
        .expect("the deferred call completes")
        .expect("hydration result");
    assert!(
        hydration.content.contains("was deferred"),
        "the model still gets the retry hint: {}",
        hydration.content
    );
    assert!(
        statuses
            .iter()
            .all(|status| !status.contains("Loaded deferred tool")),
        "{statuses:?}"
    );
}

#[test]
fn model_tool_catalog_defers_non_core_native_tools_in_act_mode() {
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog(
        vec![api_tool("read"), api_tool("project_map")],
        vec![api_tool("mcp_server_write")],
        AppMode::Agent,
        &always_load,
    );

    let defer_loading = |name: &str| {
        catalog
            .iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.defer_loading)
    };

    assert_eq!(defer_loading("read"), Some(false));
    assert_eq!(defer_loading("project_map"), Some(true));
    assert_eq!(defer_loading("mcp_server_write"), Some(true));
}

#[test]
fn request_user_input_stays_deferred_but_can_be_dynamically_activated() {
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog(
        vec![api_tool("read_file"), api_tool(REQUEST_USER_INPUT_NAME)],
        Vec::new(),
        AppMode::Agent,
        &always_load,
    );

    assert_eq!(
        catalog
            .iter()
            .find(|tool| tool.name == REQUEST_USER_INPUT_NAME)
            .and_then(|tool| tool.defer_loading),
        Some(true)
    );

    let mut active = initial_active_tools(&catalog);
    assert!(!active.contains(REQUEST_USER_INPUT_NAME));
    active.insert(REQUEST_USER_INPUT_NAME.to_string());

    let active_tools = active_tools_for_step(&catalog, &active);
    assert!(
        active_tools
            .iter()
            .any(|tool| tool.name == REQUEST_USER_INPUT_NAME),
        "dynamic active tools should expose the question modal without making it eager by default"
    );
}

#[test]
fn question_tool_survives_the_tool_surface_in_every_posture() {
    // Questions are separate from approval posture: the surface takes no
    // posture input, so Auto-Review offers `request_user_input` exactly like
    // Suggest, Never and Full Access. Only an explicit deny (headless exec's
    // default) withholds it.
    let surface = policy_for_catalog(
        vec![api_tool("read_file"), api_tool(REQUEST_USER_INPUT_NAME)],
        None,
        None,
    );
    assert!(
        surface
            .catalog
            .iter()
            .any(|tool| tool.name == REQUEST_USER_INPUT_NAME)
    );

    // Headless exec's default deny list; `exec_agent` tests pin that
    // `exec_disallowed_tools(None)` produces it.
    let headless = policy_for_catalog(
        vec![api_tool("read_file"), api_tool(REQUEST_USER_INPUT_NAME)],
        None,
        Some(vec![REQUEST_USER_INPUT_NAME.to_string()]),
    );
    assert!(
        !headless
            .catalog
            .iter()
            .any(|tool| tool.name == REQUEST_USER_INPUT_NAME)
    );
    assert!(headless.catalog.iter().any(|tool| tool.name == "read_file"));
}

#[test]
fn model_tool_catalog_sorts_each_partition_for_prefix_cache_stability() {
    // Regression for #263: deterministic byte order of the tools array is a
    // hard requirement for DeepSeek's KV prefix cache. Built-ins stay as a
    // contiguous prefix; MCP tools follow. Within each partition: alphabetical.
    let always_load = HashSet::new();
    let catalog = build_model_tool_catalog(
        vec![
            api_tool("read_file"),
            api_tool("apply_patch"),
            api_tool("exec_shell"),
        ],
        vec![api_tool("mcp_zoo_b"), api_tool("mcp_aardvark_a")],
        AppMode::Agent,
        &always_load,
    );

    let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "apply_patch",
            "exec_shell",
            "read_file",
            "mcp_aardvark_a",
            "mcp_zoo_b",
        ],
        "built-ins must be alphabetical and contiguous; MCP tools follow, alphabetical",
    );
}

#[test]
fn active_tool_list_pushes_deferred_activations_to_the_tail() {
    // Regression for #263: when ToolSearch activates a deferred tool mid-
    // session, it must NOT be inserted at its catalog index — that would
    // shift every later tool's byte offset and bust the cached prefix.
    // Deferred-but-now-active tools belong at the tail.
    let mut a = api_tool("a_load_now");
    a.defer_loading = Some(false);
    let mut search = api_tool("search_via_toolsearch");
    search.defer_loading = Some(true);
    let mut b = api_tool("b_load_now");
    b.defer_loading = Some(false);

    let catalog = vec![a, search, b];
    let active: HashSet<String> = ["a_load_now", "search_via_toolsearch", "b_load_now"]
        .into_iter()
        .map(String::from)
        .collect();

    let listed = active_tools_for_step(&catalog, &active);
    let names: Vec<&str> = listed.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a_load_now", "b_load_now", "search_via_toolsearch"],
        "deferred-but-active tools must come after always-loaded tools",
    );
}

#[test]
fn legacy_rlm_actions_are_not_advertised_to_new_model_turns() {
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
    // The session-persistent kernel is now the normal RLM path. These tools
    // stay registered solely for saved-transcript replay and an explicitly
    // named compatibility call; letting a fresh model discover them would
    // recreate the old action-by-action workflow.
    for legacy_name in [
        "rlm",
        "rlm_session_objects",
        "rlm_open",
        "rlm_eval",
        "rlm_configure",
        "rlm_close",
    ] {
        assert!(
            !catalog.iter().any(|tool| tool.name == legacy_name),
            "{legacy_name} must remain outside the new-turn model catalog"
        );
    }
}

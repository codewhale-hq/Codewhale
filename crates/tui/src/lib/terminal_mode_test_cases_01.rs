fn parse_cli(args: &[&str]) -> Cli {
    parse_cli_args(args).expect("CLI args should parse")
}

#[test]
fn worker_command_policy_prompt_that_looks_like_a_flag_parses() {
    use codewhale_engine::fleet::executor::build_worker_exec_command;
    use codewhale_config::FleetExecConfig;
    use codewhale_protocol::fleet::FleetTaskSpec;

    let task: FleetTaskSpec = serde_json::from_value(serde_json::json!({
        "id": "t1",
        "name": "Smoke",
        "objective": "prove it runs",
        "instructions": "audit",
        "worker": { "role": "reviewer", "tool_profile": "read-only" }
    }))
    .unwrap();

    // A Markdown bullet list, and a policy that reads exactly like one of
    // exec's own flags; the latter makes clap reject a split
    // `--append-system-prompt <value>` pair.
    for policy in ["- Never push to main\n- Never touch .git/config", "--hooks"] {
        let exec = FleetExecConfig {
            append_system_prompt: policy.to_string(),
            ..FleetExecConfig::default()
        };
        let cmd = build_worker_exec_command("codewhale", &task, &exec, None);
        let cli = Cli::try_parse_from(std::iter::once("codewhale".to_string()).chain(cmd.args))
            .unwrap_or_else(|e| panic!("{policy:?}: {e}"));
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };
        assert_eq!(args.append_system_prompt.as_deref(), Some(policy));
        assert!(
            args.prompt.last().is_some_and(|p| p.contains("audit")),
            "{policy}"
        );
        assert!(!args.hooks, "{policy:?} must stay text, not a flag");
    }
}

#[test]
fn sessions_archive_cli_keeps_legacy_listing_and_export_options() {
    let legacy = parse_cli(&["codewhale", "sessions", "--limit", "7", "--search", "work"]);
    assert!(
        matches!(legacy.command, Some(Commands::Sessions { limit: 7, search: Some(ref s), command: None }) if s == "work")
    );
    let list = parse_cli(&["codewhale", "sessions", "list", "--limit", "4"]);
    assert!(matches!(
        list.command,
        Some(Commands::Sessions {
            command: Some(SessionsCommand::List { limit: 4, .. }),
            ..
        })
    ));
    let export = parse_cli(&[
        "codewhale",
        "sessions",
        "export",
        "abc123",
        "--output",
        "session.tar.xz",
        "--skip-artifacts",
        "--force",
        "--compression",
        "0",
    ]);
    assert!(
        matches!(export.command, Some(Commands::Sessions { command: Some(SessionsCommand::Export { ref id, output: Some(ref output), skip_artifacts: true, compression: 0, force: true }), .. }) if id == "abc123" && output == Path::new("session.tar.xz"))
    );
}

#[test]
fn headless_consultant_authority_overrides_network_allow_and_disables_web_search() {
    let config = {
        let __cw_config_init_0_0 = Some(codewhale_config::NetworkPolicyToml {
            default: "allow".to_string(),
            audit: false,
            ..codewhale_config::NetworkPolicyToml::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.network = __cw_config_init_0_0;
        __cw_config_init_0_value
    };
    let authority = codewhale_engine::tools::spec::ToolAuthorityEnvelope {
        schema_version: 1,
        owner: "consultant-1".to_string(),
        authority: codewhale_engine::tools::spec::ToolMutationAuthority::ReadOnly,
        network_access: Some(false),
        shell: codewhale_engine::tools::spec::ToolShellAuthority::None,
        verification: codewhale_engine::tools::spec::ToolVerificationAuthority::None,
        writable_roots: Vec::new(),
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
    }
    .normalized()
    .expect("Consultant authority");

    let policy = exec_network_policy(&config, authority.network_access)
        .expect("explicit network=false always installs a policy");
    assert_eq!(
        policy.evaluate("example.com", "web_search"),
        codewhale_net::network_policy::Decision::Deny,
        "the permissive user config must not widen Consultant network authority"
    );
    let mut features = codewhale_runtime::features::Features::default();
    features.enable(codewhale_runtime::features::Feature::ShellTool);
    features.enable(codewhale_runtime::features::Feature::WebSearch);
    apply_fleet_engine_feature_caps(
        &mut features,
        true,
        authority.network_access,
        authority.shell,
    );
    assert!(!features.enabled(codewhale_runtime::features::Feature::WebSearch));
    assert!(!features.enabled(codewhale_runtime::features::Feature::ShellTool));

    let worker_policy = exec_network_policy(&config, Some(true)).expect("configured policy");
    assert_eq!(
        worker_policy.evaluate("example.com", "web_search"),
        codewhale_net::network_policy::Decision::Allow,
        "a network-capable role keeps the configured policy"
    );
}
#[test]
fn hidden_remote_control_flag_starts_the_interactive_handoff() {
    let cli = parse_cli(&["codewhale-tui", "--remote-control"]);
    assert!(cli.remote_control);
}

#[test]
fn plugin_registry_discovery_is_route_independent_and_read_only() {
    let _env_lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let codewhale_home = temp.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let workspace_arg = workspace.to_string_lossy().into_owned();

    for route in [
        Vec::<&str>::new(),
        vec!["resume", "--last"],
        vec!["fork", "--last"],
        vec!["exec", "hello"],
        vec!["serve", "--mcp"],
    ] {
        let mut args = vec![
            "codewhale-tui".to_string(),
            "--workspace".to_string(),
            workspace_arg.clone(),
        ];
        args.extend(route.into_iter().map(str::to_string));
        let cli = Cli::try_parse_from(args).expect("route should parse");
        let discovery = codewhale_plugins::PluginDiscoveryContext::capture_pre_dotenv();
        let registry = discovery
            .registry_for_workspace(cli.workspace.as_deref().unwrap_or(workspace.as_path()));
        assert_eq!(registry.workspace(), workspace.as_path());
        assert!(
            !codewhale_home.join("plugins/state.json").exists(),
            "startup discovery must remain read-only"
        );
    }
}

fn custom_exec_config(active: &str) -> Config {
    let mut custom = std::collections::HashMap::new();
    for (name, base_url, model) in [
        (
            "custom-a",
            "http://127.0.0.1:18181/v1",
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        ),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                api_key: Some("local-test-key".to_string()),
                ..Default::default()
            },
        );
    }
    {
        let __cw_config_init_1_0 = Some(active.to_string());
        let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..Default::default()
        });
        let mut __cw_config_init_1_value: Config = Default::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    }
}

#[test]
fn doctor_json_surfaces_keep_exact_named_custom_provider() {
    let config = custom_exec_config("custom-a");
    let workspace = tempfile::tempdir().expect("doctor workspace");

    let operate = doctor_operate_fleet_report_json(&config, workspace.path());
    let provider_model = doctor_provider_model_report_json(&config);
    let capability = provider_capability_report(&config);
    let route = doctor_route_report(&config);

    assert_eq!(operate["provider"]["id"], "custom-a");
    assert_eq!(provider_model["provider"]["id"], "custom-a");
    assert_eq!(capability["resolved_provider"], "custom-a");
    assert_eq!(route["provider"], "custom-a");
    assert_eq!(route["provider_config_table"], "providers.custom-a");
    let serialized = serde_json::to_string(&serde_json::json!({
        "operate": operate,
        "provider_model": provider_model,
        "capability": capability,
        "route": route,
    }))
    .expect("doctor JSON");
    assert!(!serialized.contains("local-test-key"));
}

#[test]
fn doctor_operate_fleet_json_lists_multi_layer_profile_paths() {
    // #5098: doctor must name the winning layer and every losing path
    // when project and personal both define the same id.
    let _env_lock = crate::test_support::lock_test_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    let personal = home.join("agents");
    let project = workspace.join(".codewhale").join("agents");
    std::fs::create_dir_all(&personal).expect("personal agents");
    std::fs::create_dir_all(&project).expect("project agents");
    std::fs::write(
        personal.join("builder.toml"),
        "id = \"builder\"\nrole_hint = \"builder\"\nmodel = \"deepseek-v4-flash\"\n",
    )
    .expect("personal builder");
    std::fs::write(
        project.join("builder.toml"),
        "id = \"builder\"\nrole_hint = \"builder\"\nmodel = \"deepseek-v4-pro\"\n",
    )
    .expect("project builder");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);

    let operate = doctor_operate_fleet_report_json(&Config::default(), &workspace);
    let layers = operate["roster"]["multi_layer"]
        .as_array()
        .expect("multi_layer array");
    let builder = layers
        .iter()
        .find(|entry| entry["id"] == "builder")
        .expect("builder multi-layer entry");
    assert_eq!(builder["effective"], "project");
    let paths: Vec<&str> = builder["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .filter_map(|layer| layer["path"].as_str())
        .collect();
    assert!(
        paths.iter().any(|path| path.ends_with("builder.toml")),
        "layer paths include the profile files: {builder}"
    );
    assert!(
        builder["layers"]
            .as_array()
            .expect("layers")
            .iter()
            .any(|layer| layer["origin"] == "personal" && layer["wins"] == false),
        "personal layer is listed as ignored: {builder}"
    );
    assert!(
        builder["layers"]
            .as_array()
            .expect("layers")
            .iter()
            .any(|layer| layer["origin"] == "project" && layer["wins"] == true),
        "project layer wins: {builder}"
    );
}

#[test]
fn doctor_fleet_report_flags_pins_absent_from_fresh_live_roster() {
    // #6035: a pin that vanished from the provider's current live roster
    // is drift the report must name — warning only, never a rewrite.
    let _env_lock = crate::test_support::lock_test_env();
    let _live = codewhale_engine::provider_lake::lock_live_snapshot();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &home);
    let fleets = home.join("fleets");
    std::fs::create_dir_all(&fleets).expect("fleets dir");
    std::fs::write(
                            fleets.join("default.toml"),
                            "schema = \"fleet\"\nschema_revision = 2\nname = \"default\"\n\
                             [operator]\nprovider = \"deepseek\"\nmodel = \"deepseek-flash\"\n\
                             [[members]]\nid = \"builder\"\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\n",
                        )
                        .expect("fleet file");

    let config = {
        let __cw_config_init_2_0 = Some("deepseek".to_string());
        // A fresh one-row roster must not replace the normal DeepSeek
        // endpoint's process-wide catalog for unrelated route tests.
        let mut __cw_config_init_2_value: Config = Default::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value
    }
    .with_legacy_root(
        None,
        Some("https://api.deepseek.com/v1/doctor-roster-fixture".to_string()),
    );
    let base_url = config.base_url_for_route(
        &config
            .resolve_provider_selection_identity("deepseek")
            .unwrap(),
    );
    let fetched_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let fingerprint = codewhale_config::catalog::base_url_fingerprint(&base_url);
    assert_eq!(
        codewhale_engine::provider_catalog_live::record_success(
            codewhale_config::catalog::ProviderCatalogDelta {
                provider: "deepseek".to_string(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at,
                offerings: vec![codewhale_config::catalog::CatalogOffering {
                    provider: "deepseek".to_string(),
                    wire_model_id: "deepseek-flash".to_string(),
                    endpoint_key: "chat".to_string(),
                    source: codewhale_config::catalog::CatalogSource::Live {
                        base_url_fingerprint: fingerprint,
                        fetched_at,
                    },
                    ..Default::default()
                }],
            }
        ),
        codewhale_config::catalog::CatalogStatus::Fresh
    );

    let operate = doctor_operate_fleet_report_json(&config, &workspace);
    let drift = &operate["model_pin_drift"];
    let drifted = drift["drifted"].as_array().expect("drifted array");
    let row = drifted
        .iter()
        .find(|row| row["model"] == "deepseek-v4-flash")
        .expect("member pin flagged: {drift}");
    assert_eq!(row["provider"], "deepseek");
    assert!(
        row["owners"]
            .as_array()
            .expect("owners")
            .iter()
            .any(|owner| owner.as_str().is_some_and(|o| o.contains("member:builder"))),
        "the fleet member pin is named: {row}"
    );
    // The operator pin moved to the listed id — not drift.
    assert!(
        !drifted.iter().any(|row| row["model"] == "deepseek-flash"),
        "listed pin must not be flagged: {drifted:?}"
    );
}

fn saved_exec_session(provider: &str, model: &str) -> session_manager::SavedSession {
    let mut saved = session_manager::create_saved_session_with_mode(
        &[],
        model,
        Path::new("/tmp/exec-resume"),
        0,
        None,
        Some("exec"),
    );
    let kind = codewhale_config::ProviderKind::parse(provider)
        .unwrap_or(codewhale_config::ProviderKind::Custom)
        .as_str();
    let exact_id = (!provider
        .eq_ignore_ascii_case(codewhale_config::ProviderKind::Custom.as_str()))
    .then_some(provider);
    saved.metadata.set_model_provider_route(kind, exact_id);
    saved
}

#[test]
fn prompt_flag_accepts_split_prompt_words_for_windows_cmd_shims() {
    let cli = parse_cli(&["codewhale", "-p", "hello", "world"]);

    assert_eq!(cli.prompt, vec!["hello", "world"]);
}

#[test]
fn prompt_flag_starts_interactive_submit_input() {
    let cli = parse_cli(&["codewhale", "-p", "read", "the", "project"]);

    assert_eq!(
        top_level_prompt_initial_input(&cli.prompt),
        Some(tui::InitialInput::Submit("read the project".to_string()))
    );
}

#[test]
fn runtime_decoder_uses_the_canonical_command_name() {
    assert_eq!(Cli::command().get_name(), "codewhale");
}

#[test]
fn usage_errors_name_the_codewhale_command() {
    let error = Cli::try_parse_from(["codewhale-tui", "doctor", "--bogus"])
        .expect_err("an unknown doctor flag must not parse");
    let rendered = error.render().to_string();
    assert!(
        rendered.contains("codewhale doctor"),
        "usage should name `codewhale doctor`: {rendered}"
    );
    assert!(
        !rendered.contains("codewhale-tui"),
        "usage must not name the retired binary: {rendered}"
    );
}

#[test]
fn xai_device_auth_subcommand_parses() {
    let cli = parse_cli(&["codewhale-tui", "auth", "xai-device"]);
    assert!(matches!(
        cli.command,
        Some(Commands::Auth(TuiAuthArgs {
            command: TuiAuthCommand::XaiDevice
        }))
    ));
}

#[test]
fn chatgpt_auth_subcommand_parses() {
    let cli = parse_cli(&["codewhale-tui", "auth", "chatgpt"]);
    assert!(matches!(
        cli.command,
        Some(Commands::Auth(TuiAuthArgs {
            command: TuiAuthCommand::Chatgpt
        }))
    ));
    let cli = parse_cli(&["codewhale-tui", "auth", "chatgpt-revoke"]);
    assert!(matches!(
        cli.command,
        Some(Commands::Auth(TuiAuthArgs {
            command: TuiAuthCommand::ChatgptRevoke
        }))
    ));
}

#[test]
fn workflow_tool_internal_subcommand_parses_exact_json() {
    let cli = parse_cli(&[
        "codewhale-tui",
        "workflow-tool",
        "--approval-source",
        "explicit-workflow-command",
        "--input-json",
        r#"{"action":"run","source_path":"workflows/demo.js"}"#,
    ]);
    let Some(Commands::WorkflowTool(args)) = cli.command else {
        panic!("expected workflow-tool command");
    };
    assert!(args.input_json.contains("\"action\":\"run\""));
}

#[tokio::test]
async fn direct_workflow_tool_runs_without_an_operator_model_turn() {
    use codewhale_engine::tools::spec::ToolSpec;

    let workspace = tempfile::tempdir().expect("workspace");
    let config = {
        let __cw_config_init_3_0 = Some("vllm".to_string());
        let __cw_config_init_3_1 = Some(
            workspace
                .path()
                .join("missing-mcp.json")
                .display()
                .to_string(),
        );
        let __cw_config_init_3_2 = Some(codewhale_runtime::config::ProvidersConfig {
            vllm: codewhale_runtime::config::ProviderConfig {
                base_url: Some("http://127.0.0.1:9/v1".to_string()),
                model: Some("offline-test-model".to_string()),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_3_value: Config = Default::default();
        __cw_config_init_3_value.provider = __cw_config_init_3_0;
        __cw_config_init_3_value.mcp_config_path = __cw_config_init_3_1;
        __cw_config_init_3_value.providers = __cw_config_init_3_2;
        __cw_config_init_3_value
    };
    let route = CliAutoRoute {
        provider: config.test_identity_for_kind(codewhale_config::ProviderKind::Vllm),
        model: "offline-test-model".to_string(),
        reasoning_effort: None,
        auto_controls_reasoning: false,
        auto_model: false,
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(workspace.path()));
    let (tool, context) =
        build_direct_workflow_tool(&config, &route, workspace.path(), event_tx, plugins)
            .await
            .expect("build direct workflow runtime");

    let result = tool
        .execute(
            serde_json::json!({
                "action": "run",
                "script": "phase('offline'); return { ok: true };",
                "token_budget": 1_000_000
            }),
            &context,
        )
        .await
        .expect("model-free workflow run");
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("workflow JSON");

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["result"]["ok"], true);
    assert_eq!(payload["child_ids"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        payload["plan_approval"]["decision"],
        "approved_explicit_cli_command"
    );
    assert!(!context.auto_approve);
    assert!(!context.trust_mode);
    assert_eq!(
        context.shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
    assert!(matches!(
        context.elevated_sandbox_policy,
        Some(codewhale_sandbox::SandboxPolicy::WorkspaceWrite { .. })
    ));
    let mut event_types = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let codewhale_engine::core::events::Event::WorkflowUi { event, .. } = event
            && let Some(kind) = event["type"].as_str()
        {
            event_types.push(kind.to_string());
        }
    }
    assert!(event_types.iter().any(|kind| kind == "run_started"));
    assert!(event_types.iter().any(|kind| kind == "run_completed"));
}

#[tokio::test]
async fn direct_workflow_mcp_pool_applies_network_policy_before_connect() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mcp_path = workspace.path().join("mcp.json");
    std::fs::write(
        &mcp_path,
        r#"{
                                "mcpServers": {
                                    "blocked": { "url": "https://blocked.invalid/mcp" }
                                }
                            }"#,
    )
    .expect("write MCP config");
    let config = {
        let __cw_config_init_4_0 = Some(mcp_path.display().to_string());
        let mut __cw_config_init_4_value: Config = Default::default();
        __cw_config_init_4_value.mcp_config_path = __cw_config_init_4_0;
        __cw_config_init_4_value
    };
    let policy = codewhale_net::network_policy::NetworkPolicyDecider::new(
        codewhale_net::network_policy::NetworkPolicy {
            default: codewhale_net::network_policy::DecisionToml::Deny,
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        },
        None,
    );

    let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(workspace.path()));
    let (_pool, failures) =
        initialize_direct_workflow_mcp_pool(&config, workspace.path(), Some(policy), plugins)
            .await
            .expect("MCP feature enabled");
    assert_eq!(failures.len(), 1, "failures={failures:?}");
    assert_eq!(failures[0].0, "blocked");
    assert!(failures[0].1.contains("blocked by network policy"));
}

#[test]
fn exec_model_resolution_uses_provider_scoped_default() {
    let _env_lock = crate::test_support::lock_test_env();
    let _codewhale_model = crate::test_support::EnvVarGuard::remove("CODEWHALE_MODEL");
    let _deepseek_model = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MODEL");
    let config = {
        let __cw_config_init_5_0 = Some("openrouter".to_string());
        let __cw_config_init_5_1 = Some("deepseek/deepseek-v4-pro".to_string());
        let __cw_config_init_5_2 = Some(codewhale_runtime::config::ProvidersConfig {
            openrouter: codewhale_runtime::config::ProviderConfig {
                model: Some("arcee-ai/trinity-large-thinking".to_string()),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_5_value: Config = Default::default();
        __cw_config_init_5_value.provider = __cw_config_init_5_0;
        __cw_config_init_5_value.default_text_model = __cw_config_init_5_1;
        __cw_config_init_5_value.providers = __cw_config_init_5_2;
        __cw_config_init_5_value
    };

    assert_eq!(
        resolve_exec_model(&config, None),
        "arcee-ai/trinity-large-thinking"
    );
    assert_eq!(
        resolve_exec_model(&config, Some("arcee-ai/trinity-large-thinking")),
        "arcee-ai/trinity-large-thinking"
    );
}

#[test]
fn exec_model_resolution_prefers_codewhale_model_env_override() {
    let _env_lock = crate::test_support::lock_test_env();
    let _codewhale_model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", " auto ");
    let _deepseek_model =
        crate::test_support::EnvVarGuard::set("DEEPSEEK_MODEL", "stale-deepseek-model");
    let config = {
        let __cw_config_init_6_0 = Some("deepseek/deepseek-v4-pro".to_string());
        let mut __cw_config_init_6_value: Config = Default::default();
        __cw_config_init_6_value.default_text_model = __cw_config_init_6_0;
        __cw_config_init_6_value
    };

    assert_eq!(resolve_exec_model(&config, None), "auto");
}

#[test]
fn exec_model_resolution_uses_legacy_deepseek_model_env_override() {
    let _env_lock = crate::test_support::lock_test_env();
    let _codewhale_model = crate::test_support::EnvVarGuard::remove("CODEWHALE_MODEL");
    let _deepseek_model = crate::test_support::EnvVarGuard::set("DEEPSEEK_MODEL", " auto ");
    let config = {
        let __cw_config_init_7_0 = Some("deepseek/deepseek-v4-pro".to_string());
        let mut __cw_config_init_7_value: Config = Default::default();
        __cw_config_init_7_value.default_text_model = __cw_config_init_7_0;
        __cw_config_init_7_value
    };

    assert_eq!(resolve_exec_model(&config, None), "auto");
}

#[test]
fn exec_model_resolution_uses_provider_safe_default_for_zai() {
    let _env_lock = crate::test_support::lock_test_env();
    let _codewhale_model = crate::test_support::EnvVarGuard::remove("CODEWHALE_MODEL");
    let _deepseek_model = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MODEL");
    let config = {
        let __cw_config_init_8_0 = Some("zai".to_string());
        let __cw_config_init_8_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_8_value: Config = Default::default();
        __cw_config_init_8_value.provider = __cw_config_init_8_0;
        __cw_config_init_8_value.default_text_model = __cw_config_init_8_1;
        __cw_config_init_8_value
    };

    assert_eq!(
        resolve_exec_model(&config, None),
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
    );
}

#[test]
fn fresh_launch_uses_selected_fleet_operator_unless_route_is_explicit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fleets = workspace.path().join(".codewhale").join("fleets");
    std::fs::create_dir_all(&fleets).expect("fleet directory");
    std::fs::write(fleets.join("selected"), "Launch\n").expect("selection");
    std::fs::write(
        fleets.join("launch.toml"),
        r#"schema = "fleet"
                schema_revision = 2
                name = "Launch"

                [operator]
                provider = "deepseek"
                model = "deepseek-v4-flash-vision-exp"
                reasoning = "high"
                "#,
    )
    .expect("fleet file");

    let base = {
        let __cw_config_init_9_0 = Some("openrouter".to_string());
        let __cw_config_init_9_1 = Some("off".to_string());
        let mut __cw_config_init_9_value: Config = Default::default();
        __cw_config_init_9_value.provider = __cw_config_init_9_0;
        __cw_config_init_9_value.reasoning_effort = __cw_config_init_9_1;
        __cw_config_init_9_value
    }
    .with_legacy_root(Some("test-key".to_string()), None);
    let mut explicit = base.clone();
    assert!(
        !apply_selected_fleet_operator_for_launch(&mut explicit, workspace.path(), true, false,)
            .expect("explicit route bypasses Fleet operator")
    );
    assert_eq!(
        explicit.active_provider_identity().unwrap().provider,
        codewhale_config::ProviderKind::Openrouter
    );

    let mut selected = base;
    assert!(
        apply_selected_fleet_operator_for_launch(&mut selected, workspace.path(), false, false,)
            .expect("selected operator applies")
    );
    assert_eq!(
        selected.active_provider_identity().unwrap().provider,
        codewhale_config::ProviderKind::Deepseek
    );
    assert_eq!(selected.default_model(), "deepseek-v4-flash-vision-exp");
    assert_eq!(selected.reasoning_effort(), Some("high"));
    assert!(selected.fleet_operator_route_applied);
    assert!(selected.fleet_operator_reasoning_applied);

    let mut reasoning_override = {
        let __cw_config_init_10_0 = Some("off".to_string());
        let mut __cw_config_init_10_value: Config = Default::default();
        __cw_config_init_10_value.reasoning_effort = __cw_config_init_10_0;
        __cw_config_init_10_value
    }
    .with_legacy_root(Some("test-key".to_string()), None);
    apply_selected_fleet_operator_for_launch(
        &mut reasoning_override,
        workspace.path(),
        false,
        true,
    )
    .expect("explicit reasoning coexists with Fleet route");
    assert_eq!(
        reasoning_override.default_model(),
        "deepseek-v4-flash-vision-exp"
    );
    assert_eq!(reasoning_override.reasoning_effort(), Some("off"));
    assert!(reasoning_override.fleet_operator_route_applied);
    assert!(!reasoning_override.fleet_operator_reasoning_applied);
}

#[test]
fn selected_fleet_operator_load_error_redacts_paths_excerpts_and_opaque_name() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fleets = workspace.path().join(".codewhale").join("fleets");
    std::fs::create_dir_all(&fleets).expect("fleet directory");
    let secret_marker = "sk-live-abcdef0123456789abcdef";
    std::fs::write(fleets.join("selected"), format!("{secret_marker}\n")).expect("selection");
    std::fs::write(
        fleets.join(format!("{secret_marker}.toml")),
        format!("invalid TOML /Users/operator/private {secret_marker}\n"),
    )
    .expect("invalid Fleet");

    let mut config = Config::default();
    let message =
        apply_selected_fleet_operator_for_launch(&mut config, workspace.path(), false, false)
            .expect_err("invalid selected Fleet must fail")
            .to_string();

    assert!(!message.contains(&workspace.path().display().to_string()));
    assert!(!message.contains("/Users/operator"));
    assert!(!message.contains(secret_marker));
    assert!(!message.contains("invalid TOML"));
    assert!(message.chars().count() <= 700, "{message}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn explicit_exec_model_routes_to_unique_authenticated_provider_candidate() {
    let _env_lock = crate::test_support::lock_test_env();
    let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
    let _openrouter = crate::test_support::EnvVarGuard::remove("OPENROUTER_API_KEY");
    let config = {
        let __cw_config_init_11_0 = Some("deepseek".to_string());
        let __cw_config_init_11_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_11_value: Config = Default::default();
        __cw_config_init_11_value.provider = __cw_config_init_11_0;
        __cw_config_init_11_value.default_text_model = __cw_config_init_11_1;
        __cw_config_init_11_value
    };

    let route = resolve_cli_auto_route(
        &config,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        "pong",
    )
    .await
    .expect("explicit GLM should route to the configured Z.ai provider");

    assert_eq!(route.provider.provider, codewhale_config::ProviderKind::Zai);
    assert_eq!(
        route.model,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
    );
    assert!(!route.auto_model);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn explicit_exec_model_reports_ambiguous_authenticated_provider_candidates() {
    let _env_lock = crate::test_support::lock_test_env();
    let _zai = crate::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
    let _openrouter = crate::test_support::EnvVarGuard::set("OPENROUTER_API_KEY", "or-key");
    let config = {
        let __cw_config_init_12_0 = Some("deepseek".to_string());
        let __cw_config_init_12_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_12_value: Config = Default::default();
        __cw_config_init_12_value.provider = __cw_config_init_12_0;
        __cw_config_init_12_value.default_text_model = __cw_config_init_12_1;
        __cw_config_init_12_value
    };

    let err = resolve_cli_auto_route(
        &config,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        "pong",
    )
    .await
    .expect_err("ambiguous GLM route should ask for an explicit provider");
    let message = err.to_string();

    assert!(message.contains("model `GLM-5.2` is available"));
    assert!(message.contains("openrouter"));
    assert!(message.contains("zai"));
    assert!(message.contains("--provider"));
    assert!(message.contains("/provider"));
    assert!(message.contains("/model"));
    assert!(message.contains("/setup"));
}

#[tokio::test]
async fn cli_auto_model_honors_a_fixed_reasoning_preference() {
    let config = {
        let __cw_config_init_13_0 = Some("vllm".to_string());
        let __cw_config_init_13_1 = Some("low".to_string());
        let __cw_config_init_13_2 = Some(codewhale_runtime::config::ProvidersConfig {
            vllm: codewhale_runtime::config::ProviderConfig {
                base_url: Some("http://127.0.0.1:18190/v1".to_string()),
                model: Some("local-auto-model".to_string()),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_13_value: Config = Default::default();
        __cw_config_init_13_value.provider = __cw_config_init_13_0;
        __cw_config_init_13_value.reasoning_effort = __cw_config_init_13_1;
        __cw_config_init_13_value.providers = __cw_config_init_13_2;
        __cw_config_init_13_value
    };

    let route = resolve_cli_auto_route(&config, "auto", "debug a failing test")
        .await
        .expect("Auto model route");

    assert!(route.auto_model);
    assert_eq!(
        route.reasoning_effort,
        Some(codewhale_engine::reasoning_preference::ReasoningEffort::Low)
    );
    assert!(
        !route.auto_controls_reasoning,
        "a fixed saved tier must not be replaced per prompt"
    );
}

#[test]
fn cli_route_execution_config_stamps_routed_model_into_provider_slot() {
    let mut providers = codewhale_runtime::config::ProvidersConfig::default();
    providers.deepseek.model = Some("deepseek-v4-pro".to_string());
    let config = {
        let __cw_config_init_14_0 = Some("deepseek".to_string());
        let __cw_config_init_14_1 = Some(providers);
        let mut __cw_config_init_14_value: Config = Default::default();
        __cw_config_init_14_value.provider = __cw_config_init_14_0;
        __cw_config_init_14_value.providers = __cw_config_init_14_1;
        __cw_config_init_14_value
    };
    let route = CliAutoRoute {
        provider: Config::default()
            .test_identity_for_kind(codewhale_config::ProviderKind::Deepseek),
        model: "deepseek-v4-flash".to_string(),
        reasoning_effort: None,
        auto_controls_reasoning: true,
        auto_model: true,
    };

    let execution_config = config_for_cli_route(&config, &route).expect("admitted execution route");

    assert_eq!(execution_config.default_model(), "deepseek-v4-flash");
    assert_eq!(
        execution_config
            .provider_config_for(
                &execution_config.test_identity_for_kind(codewhale_config::ProviderKind::Deepseek)
            )
            .and_then(|entry| entry.model.as_deref()),
        Some("deepseek-v4-flash")
    );
}

#[test]
fn cli_route_execution_config_preserves_the_legacy_literal_custom_route() {
    let _lock = crate::test_support::lock_test_env();
    let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let config = {
        let __cw_config_init_15_0 = Some("custom".to_string());
        let __cw_config_init_15_1 = Some("legacy-model".to_string());
        let mut __cw_config_init_15_value: Config = Default::default();
        __cw_config_init_15_value.provider = __cw_config_init_15_0;
        __cw_config_init_15_value.default_text_model = __cw_config_init_15_1;
        __cw_config_init_15_value
    }
    .with_legacy_root(
        Some("legacy-root-key".to_string()),
        Some("http://127.0.0.1:18183/v1".to_string()),
    );
    let route = CliAutoRoute {
        provider: config
            .active_provider_identity()
            .expect("captured literal root identity"),
        model: "routed-legacy-model".to_string(),
        reasoning_effort: None,
        auto_controls_reasoning: false,
        auto_model: false,
    };

    let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

    // The literal route's top-level fields live in `[providers.custom]`
    // (#6394), and the routed model lands there too.
    assert!(execution.selects_literal_custom_provider());
    assert_eq!(execution.provider.as_deref(), Some("custom"));
    assert_eq!(execution.default_model(), "routed-legacy-model");
    assert_eq!(
        execution.active_route_base_url(),
        "http://127.0.0.1:18183/v1"
    );
    assert_eq!(execution.active_route_api_key().unwrap(), "legacy-root-key");
    for _ in 0..2 {
        let identity = execution
            .resolve_provider_identity("custom")
            .expect("legacy identity remains repeatedly resolvable");
        assert_eq!(identity.key.as_str(), "custom");
    }
    let client = codewhale_engine::client::CodewhaleClient::new(&execution).expect("legacy execution client");
    assert_eq!(client.base_url(), "http://127.0.0.1:18183/v1");
}

/// #6510: only a flag that grants tool authority opens a tool surface.
/// Limits, prompt/hook opt-ins and the output format keep plain exec a
/// zero-tool one-shot; `--max-turns 1` used to make it a tool agent.
#[test]
fn exec_tool_surface_needs_an_explicit_grant() {
    let grants = |argv: &[&str], yolo: bool, resuming: bool, env: bool| {
        let mut full = vec!["codewhale", "exec"];
        full.extend_from_slice(argv);
        full.push("hi");
        let cli = parse_cli(&full);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };
        exec_grants_tool_surface(&args, yolo, resuming, env)
    };

    for zero_tool in [
        &[][..],
        &["--max-turns", "1"],
        &["--max-tool-calls", "3"],
        &["--disallowed-tools", "exec_shell"],
        &["--append-system-prompt", "be brief"],
        &["--hooks"],
        &["--sandbox", "read-only"],
        &["--allow-sandbox-elevation"],
        &["--output-format", "stream-json"],
        &["--json"],
    ] {
        assert!(
            !grants(zero_tool, false, false, false),
            "{zero_tool:?} must not grant tools"
        );
    }

    assert!(grants(&["--auto"], false, false, false));
    assert!(grants(
        &["--allowed-tools", "read_file"],
        false,
        false,
        false
    ));
    assert!(grants(
        &["--tool-authority-json", "{}"],
        false,
        false,
        false
    ));
    assert!(grants(&["--max-turns", "1"], true, false, false), "yolo");
    assert!(grants(&[], false, true, false), "resumed session");
    assert!(grants(&[], false, false, true), "launcher tool surface");
}

#[test]
fn exec_accepts_split_prompt_words_for_windows_cmd_shims() {
    let cli = parse_cli(&["codewhale", "exec", "hello", "world"]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };

    assert_eq!(args.prompt, vec!["hello", "world"]);
}

#[test]
fn exec_keeps_model_flag_before_split_prompt_words() {
    let cli = parse_cli(&["codewhale", "exec", "--model", "auto", "hello", "world"]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };

    assert_eq!(args.model.as_deref(), Some("auto"));
    assert_eq!(args.prompt, vec!["hello", "world"]);
}

#[test]
fn exec_keeps_flags_before_split_prompt_words() {
    let cli = parse_cli(&["codewhale", "exec", "--json", "hello", "--world"]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };

    assert!(args.json);
    assert_eq!(args.prompt, vec!["hello", "--world"]);
    for flag in [
        "--sandbox",
        "--sandbox=workspace-write",
        "--auto",
        "--help",
        "--enable=web_search",
        "--operation-key=example",
    ] {
        let error = parse_cli_args(&["codewhale", "exec", "--auto", "fix tests", flag])
            .expect_err("exec flags after the prompt must not silently become text");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(error.to_string().contains(flag.split('=').next().unwrap()));
    }
    for prompt in [
        vec!["--", "--sandbox", "workspace-write"],
        vec!["hello", "--", "--sandbox=workspace-write"],
    ] {
        let argv: Vec<&str> = ["codewhale", "exec", "--auto"]
            .into_iter()
            .chain(prompt)
            .collect();
        let cli = parse_cli(&argv);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };
        assert!(args.sandbox.is_none());
        assert!(args.prompt.iter().any(|word| word.starts_with("--sandbox")));
    }
    let error = Cli::try_parse_from([
        "codewhale",
        "exec",
        "--auto",
        "--sandox",
        "workspace-write",
        "--max-turns",
        "1",
        "--json",
        "Reply with the single word OK and use no tools.",
    ])
    .expect_err("unknown flags before the prompt must fail parsing");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn exec_prompt_file_carries_prompts_past_the_argv_ceiling() {
    // #6688: argv caps one argument at 128 KiB; the file transport does not.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big-prompt.txt");
    let body = "стих ".repeat(60_000);
    assert!(body.len() > 512 * 1024);
    std::fs::write(&path, &body).expect("write prompt");
    let path_arg = path.to_str().expect("utf-8 path");

    let cli = parse_cli(&["codewhale", "exec", "--auto", "--prompt-file", path_arg]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };
    assert!(args.auto);
    assert!(args.prompt.is_empty());
    assert_eq!(resolve_exec_prompt(&args).expect("prompt"), body);

    let conflict =
        Cli::try_parse_from(["codewhale", "exec", "--prompt-file", path_arg, "also argv"])
            .expect_err("positional prompt and --prompt-file conflict");
    assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);

    let missing = dir.path().join("missing.txt");
    let cli = parse_cli(&[
        "codewhale",
        "exec",
        "--prompt-file",
        missing.to_str().expect("utf-8 path"),
    ]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };
    let err = resolve_exec_prompt(&args).expect_err("missing file fails");
    assert!(err.to_string().contains("--prompt-file"), "{err:#}");

    let cli = parse_cli(&[
        "codewhale",
        "exec",
        "--parent-death-watch",
        "--prompt-file",
        "-",
    ]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };
    let err = resolve_exec_prompt(&args).expect_err("stdin is owned by the watcher");
    assert!(err.to_string().contains("--parent-death-watch"), "{err:#}");

    let cli = parse_cli(&["codewhale", "exec", "explain", "this"]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };
    assert_eq!(
        resolve_exec_prompt(&args).expect("argv prompt"),
        "explain this"
    );

    // A positional `-` is prompt text, never a stdin read: cloud dispatch
    // passes job prompts verbatim as argv.
    let cli = parse_cli(&["codewhale", "exec", "--auto", "-"]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };
    assert_eq!(resolve_exec_prompt(&args).expect("literal dash"), "-");
}

#[test]
fn read_capped_text_names_the_limit_in_bytes() {
    let err =
        read_capped_text(&b"abcdef"[..], 4, "exec prompt on stdin").expect_err("over the cap");
    assert_eq!(
        err.to_string(),
        "exec prompt on stdin exceeds the 4-byte limit"
    );
    assert_eq!(
        read_capped_text(&b"abcd"[..], 4, "x").expect("at the cap"),
        "abcd"
    );
}

#[test]
fn exec_parses_provider_flag_alongside_model() {
    // #4093: Fleet threads `--provider <id>` so a worker launches on its
    // profile-pinned provider even when the parent session is elsewhere.
    let cli = parse_cli(&[
        "codewhale",
        "exec",
        "--provider",
        "openrouter",
        "--model",
        "glm-5.2",
        "audit",
    ]);
    let Some(Commands::Exec(args)) = cli.command else {
        panic!("expected exec command");
    };

    assert_eq!(args.provider.as_deref(), Some("openrouter"));
    assert_eq!(args.model.as_deref(), Some("glm-5.2"));
    assert_eq!(args.prompt, vec!["audit"]);
    // The threaded id round-trips through the provider vocabulary the exec
    // handler validates against — never a model-id sniff (EPIC #2608).
    assert_eq!(
        codewhale_config::ProviderKind::parse(args.provider.as_deref().unwrap()),
        Some(codewhale_config::ProviderKind::Openrouter)
    );
}

#[test]
fn exec_provider_override_accepts_configured_custom_provider() {
    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "lm-studio".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:1234/v1".to_string()),
            model: Some("qwen-2.5-7b".to_string()),
            api_key: Some("lm-studio".to_string()),
            ..Default::default()
        },
    );
    let mut config = {
        let __cw_config_init_16_0 = Some("deepseek".to_string());
        let __cw_config_init_16_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..Default::default()
        });
        let mut __cw_config_init_16_value: Config = Default::default();
        __cw_config_init_16_value.provider = __cw_config_init_16_0;
        __cw_config_init_16_value.providers = __cw_config_init_16_1;
        __cw_config_init_16_value
    };

    apply_exec_provider_override(&mut config, "lm-studio")
        .expect("configured custom provider should be accepted");

    assert_eq!(config.provider.as_deref(), Some("lm-studio"));
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        codewhale_config::ProviderKind::Custom
    );
}

#[test]
fn exec_provider_override_prefers_exact_case_colliding_custom_key() {
    let mut config = {
        let __cw_config_init_17_0 = Some("deepseek".to_string());
        let __cw_config_init_17_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom: std::collections::HashMap::from([(
                "CUSTOM".to_string(),
                codewhale_runtime::config::ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    base_url: Some("http://127.0.0.1:5678/v1".to_string()),
                    model: Some("case-model".to_string()),
                    api_key: Some("case-key".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let mut __cw_config_init_17_value: Config = Default::default();
        __cw_config_init_17_value.provider = __cw_config_init_17_0;
        __cw_config_init_17_value.providers = __cw_config_init_17_1;
        __cw_config_init_17_value
    };

    apply_exec_provider_override(&mut config, "CUSTOM")
        .expect("exact case-colliding custom provider");
    assert_eq!(config.provider.as_deref(), Some("CUSTOM"));
    assert_eq!(
        config.active_provider_identity().unwrap().provider,
        codewhale_config::ProviderKind::Custom
    );
    assert_eq!(
        config.active_provider_identity().unwrap().key.as_str(),
        "CUSTOM"
    );
    let route = codewhale_engine::route_runtime::resolve_runtime_route(
        &config,
        codewhale_config::ProviderKind::Custom,
        Some("case-model"),
    )
    .expect("resolve exact case-colliding route")
    .validate()
    .expect("preflight exact case-colliding route");
    assert_eq!(route.identity.key.as_str(), "CUSTOM");
    assert_eq!(route.client.base_url(), "http://127.0.0.1:5678/v1");
}

#[test]
fn exec_provider_override_rejects_unknown_provider() {
    let mut config = {
        let __cw_config_init_18_0 = Some("deepseek".to_string());
        let mut __cw_config_init_18_value: Config = Default::default();
        __cw_config_init_18_value.provider = __cw_config_init_18_0;
        __cw_config_init_18_value
    };

    let err = apply_exec_provider_override(&mut config, "lm-studio")
        .expect_err("unconfigured custom provider should fail closed");
    let message = err.to_string();

    assert!(message.contains("Unrecognized --provider"));
    assert!(message.contains("[providers.<name>] custom provider"));
    assert_eq!(config.provider.as_deref(), Some("deepseek"));
}

#[test]
fn exec_resume_route_matrix_preserves_or_overrides_exact_provider_deliberately() {
    let saved = saved_exec_session(
        "custom-a",
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
    );

    let mut restored = custom_exec_config("custom-b");
    let model = resolve_exec_resume_route(&mut restored, &saved, false, None)
        .expect("plain resume restores saved route");
    assert_eq!(restored.provider.as_deref(), Some("custom-a"));
    assert_eq!(
        model,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
    );

    let mut explicit_provider = custom_exec_config("custom-a");
    apply_exec_provider_override(&mut explicit_provider, "custom-b").expect("custom B");
    let model = resolve_exec_resume_route(&mut explicit_provider, &saved, true, None)
        .expect("explicit provider wins");
    assert_eq!(explicit_provider.provider.as_deref(), Some("custom-b"));
    assert_eq!(model, "model-b");

    let mut explicit_model = custom_exec_config("custom-b");
    let model =
        resolve_exec_resume_route(&mut explicit_model, &saved, false, Some("override-model"))
            .expect("explicit model keeps saved provider");
    assert_eq!(explicit_model.provider.as_deref(), Some("custom-a"));
    assert_eq!(model, "override-model");

    let mut missing = custom_exec_config("custom-b");
    missing
        .providers
        .as_mut()
        .expect("providers")
        .custom
        .remove("custom-a");
    let before = missing.provider.clone();
    let err = resolve_exec_resume_route(&mut missing, &saved, false, None)
        .expect_err("removed saved provider must fail closed");
    assert!(err.to_string().contains("will not fall back"), "{err}");
    assert_eq!(missing.provider, before);
}

#[test]
fn exec_resume_honours_dispatcher_forwarded_launch_overrides() {
    // `codewhale --provider X --model Y exec --resume ID ...` reaches this
    // binary with X/Y only in CODEWHALE_PROVIDER / CODEWHALE_MODEL; a
    // resume must treat them as explicit instead of restoring the saved
    // route.
    assert_eq!(
        exec_resume_route_overrides(None, None, None, None),
        (false, None)
    );
    assert_eq!(
        exec_resume_route_overrides(None, None, Some("modelstudio-token-plan"), None),
        (true, None)
    );
    assert_eq!(
        exec_resume_route_overrides(None, None, None, Some("qwen3.8-flash")),
        (false, Some("qwen3.8-flash".to_string()))
    );
    assert_eq!(
        exec_resume_route_overrides(
            None,
            None,
            Some("modelstudio-token-plan"),
            Some(" qwen3.8-flash ")
        ),
        (true, Some("qwen3.8-flash".to_string()))
    );
    // Exec-level flags still win over the forwarded launch env.
    assert_eq!(
        exec_resume_route_overrides(
            Some("deepseek"),
            Some("deepseek-v4-pro"),
            Some("x"),
            Some("y")
        ),
        (true, Some("deepseek-v4-pro".to_string()))
    );
    // Blank values are not overrides.
    assert_eq!(
        exec_resume_route_overrides(None, None, Some("  "), Some("")),
        (false, None)
    );

    let saved = saved_exec_session(
        "custom-a",
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
    );
    let mut launch_model_only = custom_exec_config("custom-b");
    let (explicit_provider, explicit_model) =
        exec_resume_route_overrides(None, None, None, Some("override-model"));
    let model = resolve_exec_resume_route(
        &mut launch_model_only,
        &saved,
        explicit_provider,
        explicit_model.as_deref(),
    )
    .expect("launch model override keeps saved provider");
    assert_eq!(launch_model_only.provider.as_deref(), Some("custom-a"));
    assert_eq!(model, "override-model");

    let mut launch_provider = custom_exec_config("custom-b");
    let (explicit_provider, explicit_model) =
        exec_resume_route_overrides(None, None, Some("custom-b"), None);
    let model = resolve_exec_resume_route(
        &mut launch_provider,
        &saved,
        explicit_provider,
        explicit_model.as_deref(),
    )
    .expect("launch provider override keeps the launched route");
    assert_eq!(launch_provider.provider.as_deref(), Some("custom-b"));
    assert_eq!(model, "model-b");
}

#[test]
fn exec_resume_uses_dispatcher_env_route_loaded_by_config() {
    // Exercise the production sequence without starting an Engine turn:
    // `Config::load` applies the dispatcher-forwarded environment, then
    // the resume seam must treat the same launch env as explicit and skip
    // the unavailable persisted route rather than restoring it.
    let _env_lock = crate::test_support::lock_test_env();
    let _legacy_provider = crate::test_support::EnvVarGuard::remove("DEEPSEEK_PROVIDER");
    let _legacy_model = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MODEL");
    let _provider = crate::test_support::EnvVarGuard::set("CODEWHALE_PROVIDER", "launch-route");
    let _model = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "dispatcher-model");
    let tmp = tempfile::tempdir().expect("config tempdir");
    let codewhale_home = tmp.path().join("codewhale-home");
    std::fs::create_dir_all(&codewhale_home).expect("isolated Codewhale home");
    let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"provider = "stored-route"

                [providers.launch-route]
                kind = "openai-compatible"
                base_url = "https://launch.example.test/v1"
                model = "configured-launch-model"
                api_key = "test-only-key"
                "#,
    )
    .expect("write config");

    let mut config = codewhale_plugins::providers::load_config(Some(config_path), None)
        .expect("load launch config");
    assert_eq!(config.provider.as_deref(), Some("launch-route"));
    assert_eq!(config.default_model(), "dispatcher-model");

    // The saved route deliberately has no live config table. This control
    // proves that a non-explicit resume would fail closed instead of
    // silently falling back; the dispatcher overrides must prevent that
    // restore attempt.
    let saved = saved_exec_session("stored-route", "stored-model");
    let mut restore_attempt = config.clone();
    let restore_error = resolve_exec_resume_route(&mut restore_attempt, &saved, false, None)
        .expect_err("unavailable saved route must not be restored");
    assert!(
        restore_error.to_string().contains("stored-route"),
        "{restore_error}"
    );

    let (explicit_provider, explicit_model) = exec_resume_route_overrides(
        None,
        None,
        codewhale_runtime::config::explicit_launch_provider_override().as_deref(),
        codewhale_runtime::config::explicit_launch_model_override().as_deref(),
    );
    assert!(explicit_provider);
    assert_eq!(explicit_model.as_deref(), Some("dispatcher-model"));

    let model = resolve_exec_resume_route(
        &mut config,
        &saved,
        explicit_provider,
        explicit_model.as_deref(),
    )
    .expect("dispatcher environment must keep the launch route on resume");
    assert_eq!(config.provider.as_deref(), Some("launch-route"));
    assert_eq!(model, "dispatcher-model");
}

#[test]
fn exec_model_reads_wait_for_foreign_test_env_overrides_to_restore() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::channel();

    let (reader, expected_after_restore) = {
        let lock = crate::test_support::lock_test_env();
        let expected_after_restore = exec_model_env_override();
        let temporary = crate::test_support::EnvVarGuard::set("CODEWHALE_MODEL", "temporary-model");
        let reader = std::thread::spawn(move || {
            started_tx.send(()).expect("signal model read start");
            tx.send(exec_model_env_override())
                .expect("send resolved model override");
        });

        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("reader reached model read");
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "a foreign reader observed another test's temporary model override"
        );
        drop(temporary);
        drop(lock);
        (reader, expected_after_restore)
    };

    let observed = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("reader resumed after model override was restored");
    reader.join().expect("reader thread");
    assert_eq!(observed, expected_after_restore);
}

#[tokio::test]
async fn forced_exec_route_keeps_custom_provider_when_model_matches_builtin_catalog() {
    let config = custom_exec_config("custom-a");

    let route = resolve_cli_exec_route(
        &config,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        "audit",
        true,
    )
    .await
    .expect("forced route");
    let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

    assert_eq!(
        route.provider.provider,
        codewhale_config::ProviderKind::Custom
    );
    assert_eq!(
        route.model,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
    );
    assert_eq!(execution.provider.as_deref(), Some("custom-a"));
}

#[tokio::test]
async fn no_flag_exec_keeps_configured_named_custom_route_for_matching_builtin_model() {
    let mut config = custom_exec_config("custom-a");
    config
        .providers
        .as_mut()
        .expect("providers")
        .custom
        .get_mut("custom-a")
        .expect("custom A")
        .model =
        Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string());
    let model = resolve_exec_model(&config, None);
    let force = should_force_configured_exec_route(false, None, None);

    assert!(force, "configured/default exec route must be authoritative");
    assert!(!should_force_configured_exec_route(
        false,
        None,
        Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL)
    ));
    assert!(should_force_configured_exec_route(
        false,
        Some("custom-a"),
        Some(codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL)
    ));
    assert!(should_force_configured_exec_route(
        true,
        None,
        Some("override-model")
    ));

    let route = resolve_cli_exec_route(&config, &model, "audit", force)
        .await
        .expect("no-flag configured route");
    let execution = config_for_cli_route(&config, &route).expect("admitted execution route");
    assert_eq!(
        route.provider.provider,
        codewhale_config::ProviderKind::Custom
    );
    assert_eq!(
        route.model,
        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
    );
    assert_eq!(execution.provider.as_deref(), Some("custom-a"));
}

#[tokio::test]
async fn configured_review_default_keeps_named_custom_route_and_exact_receipt() {
    let mut config = custom_exec_config("custom-a");
    config
        .providers
        .as_mut()
        .expect("providers")
        .custom
        .get_mut("custom-a")
        .expect("custom A")
        .model = Some("model-a".to_string());
    config.default_text_model = Some("stale-root-deepseek-model".to_string());
    let model = resolve_review_model(&config, None);
    assert_eq!(model, "model-a");
    assert_eq!(
        resolve_review_model(&config, Some("explicit-review-model")),
        "explicit-review-model"
    );

    let route = resolve_cli_exec_route(&config, &model, "review diff", true)
        .await
        .expect("configured review route");
    let execution = config_for_cli_route(&config, &route).expect("admitted execution route");
    let identity = execution.active_provider_identity().unwrap();
    let provider = identity.key.as_str();

    assert_eq!(
        route.provider.provider,
        codewhale_config::ProviderKind::Custom
    );
    assert_eq!(provider, "custom-a");
    assert_eq!(
        execution.active_route_base_url(),
        "http://127.0.0.1:18181/v1"
    );
    let output = codewhale_engine::tools::review::ReviewOutput::parse("{}");
    let receipt = codewhale_engine::tools::review::build_review_receipt(
        "working tree",
        "diff --git a/a b/a",
        provider,
        &route.model,
        &output,
        "{}",
        Vec::new(),
    );
    assert_eq!(receipt.provider, "custom-a");
    let serialized = serde_json::to_string(&receipt).expect("review receipt");
    assert!(!serialized.contains("127.0.0.1"));
    assert!(!serialized.contains("local-test-key"));
}

fn review_args(argv: &[&str]) -> ReviewArgs {
    let cli = parse_cli(argv);
    let Some(Commands::Review(args)) = cli.command else {
        panic!("expected review command");
    };
    args
}

#[test]
fn review_parses_provider_flag_alongside_model() {
    let args = review_args(&[
        "codewhale",
        "review",
        "--pr",
        "5709",
        "--provider",
        "zai",
        "--model",
        "GLM-5.3",
    ]);

    assert_eq!(args.provider.as_deref(), Some("zai"));
    assert_eq!(args.model.as_deref(), Some("GLM-5.3"));
    assert_eq!(args.pr, Some(5709));
    assert_eq!(args.max_passes, 1);
    // The threaded id round-trips through the provider vocabulary the
    // override validates against — never a model-id sniff.
    assert_eq!(
        codewhale_config::ProviderKind::parse(args.provider.as_deref().unwrap()),
        Some(codewhale_config::ProviderKind::Zai)
    );
}

#[test]
fn review_batch_passes_require_explicit_bounded_opt_in() {
    let args = review_args(&["codewhale", "review", "--pr", "6002", "--max-passes", "31"]);
    assert_eq!(args.max_passes, 31);
    assert!(validate_review_receipt_args(&args).is_ok());
    let too_many = review_args(&["codewhale", "review", "--pr", "6002", "--max-passes", "65"]);
    assert!(validate_review_receipt_args(&too_many).is_err());
    let local = review_args(&["codewhale", "review", "--max-passes", "2"]);
    assert!(validate_review_receipt_args(&local).is_err());
}

#[test]
fn review_failure_payload_preserves_usage_without_complete_claim() {
    let usage = codewhale_models::Usage {
        input_tokens: 24,
        output_tokens: 4,
        reasoning_tokens: Some(3),
        ..Default::default()
    };
    let payload = review_failure_payload(
        "fixture-provider",
        "fixture-model",
        &usage,
        1,
        2,
        ReviewPublication::NotAttempted,
        "second pass malformed",
    );
    assert_eq!(payload["success"], false);
    assert_eq!(payload["complete"], false);
    assert_eq!(payload["completed_review_passes"], 1);
    assert_eq!(payload["planned_review_passes"], 2);
    assert_eq!(payload["publication"], "not_attempted");
    assert_eq!(payload["usage"]["input_tokens"], 24);
    assert_eq!(payload["usage"]["reasoning_tokens"], 3);
    assert!(payload.get("review").is_none());
    assert!(payload.get("receipt").is_none());
}

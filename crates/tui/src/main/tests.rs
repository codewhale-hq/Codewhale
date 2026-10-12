use super::*;
use clap::Parser;
use std::fs;
use tempfile::TempDir;

#[test]
fn doctor_api_key_rows_never_name_the_retired_antigravity_slot() {
    let rows: Vec<crate::config::ProviderKind> = doctor_api_key_providers().collect();
    assert!(!rows.contains(&crate::config::ProviderKind::Antigravity));
    assert!(rows.contains(&crate::config::ProviderKind::Deepseek));
    assert!(rows.contains(&crate::config::ProviderKind::Google));
    assert!(
        rows.iter()
            .all(|provider| provider.as_str() != "antigravity")
    );
}

#[test]
fn offline_doctor_loads_never_materialize_secret_environment_overrides() {
    let _guard = crate::test_support::lock_test_env();
    let temp = tempfile::TempDir::new().expect("tempdir");
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "").expect("empty config");
    let _home = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_HOME",
        temp.path().join("home").as_os_str(),
    );
    let _profile = crate::test_support::EnvVarGuard::remove("CODEWHALE_PROFILE");
    let _legacy_profile = crate::test_support::EnvVarGuard::remove("DEEPSEEK_PROFILE");
    let _managed = crate::test_support::EnvVarGuard::remove("CODEWHALE_MANAGED_CONFIG_PATH");
    let _legacy_managed = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MANAGED_CONFIG_PATH");
    let _requirements = crate::test_support::EnvVarGuard::remove("CODEWHALE_REQUIREMENTS_PATH");
    let _legacy_requirements =
        crate::test_support::EnvVarGuard::remove("DEEPSEEK_REQUIREMENTS_PATH");
    let _headers = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_HTTP_HEADERS",
        "Authorization=doctor-offline-header-sentinel",
    );
    let _legacy_headers = crate::test_support::EnvVarGuard::set(
        "DEEPSEEK_HTTP_HEADERS",
        "Authorization=doctor-offline-legacy-header-sentinel",
    );
    let _sandbox = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_SANDBOX_API_KEY",
        "doctor-offline-sandbox-sentinel",
    );
    let _legacy_sandbox = crate::test_support::EnvVarGuard::set(
        "DEEPSEEK_SANDBOX_API_KEY",
        "doctor-offline-legacy-sandbox-sentinel",
    );
    let _search = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_SEARCH_API_KEY",
        "doctor-offline-search-sentinel",
    );
    let _legacy_search = crate::test_support::EnvVarGuard::set(
        "DEEPSEEK_SEARCH_API_KEY",
        "doctor-offline-legacy-search-sentinel",
    );
    let _base_url = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_BASE_URL",
        "https://safe-doctor.example:9443/v1",
    );
    let _allow_shell = crate::test_support::EnvVarGuard::set("CODEWHALE_ALLOW_SHELL", "false");
    let config_arg = config_path.to_string_lossy().into_owned();

    for suffix in [
        Vec::<&str>::new(),
        vec!["--json"],
        vec!["--context-json"],
        vec!["--check-updates"],
        vec!["--probe-mcp"],
    ] {
        let mut argv = vec![
            "codewhale-tui".to_string(),
            "--config".to_string(),
            config_arg.clone(),
            "doctor".to_string(),
        ];
        argv.extend(suffix.iter().copied().map(str::to_string));
        let cli = Cli::try_parse_from(argv).expect("offline doctor CLI");
        let Some(Commands::Doctor(args)) = cli.command.as_ref() else {
            panic!("expected doctor command");
        };
        let config = load_doctor_config_from_cli(&cli, args).expect("offline doctor config");
        assert!(config.http_headers.is_none());
        assert!(config.sandbox_api_key.is_none());
        assert!(
            config
                .search
                .as_ref()
                .and_then(|search| search.api_key.as_deref())
                .is_none()
        );
        assert_eq!(
            config.deepseek_table_base_url(),
            Some("https://safe-doctor.example:9443/v1")
        );
        assert_eq!(config.allow_shell, Some(false));
        let rendered = format!("{config:?}");
        for sentinel in [
            "doctor-offline-header-sentinel",
            "doctor-offline-legacy-header-sentinel",
            "doctor-offline-sandbox-sentinel",
            "doctor-offline-legacy-sandbox-sentinel",
            "doctor-offline-search-sentinel",
            "doctor-offline-legacy-search-sentinel",
        ] {
            assert!(
                !rendered.contains(sentinel),
                "{suffix:?} retained {sentinel}"
            );
        }
    }

    for probe in ["--probe-api", "--probe-local"] {
        let cli = Cli::try_parse_from(["codewhale-tui", "--config", &config_arg, "doctor", probe])
            .expect("live doctor CLI");
        let Some(Commands::Doctor(args)) = cli.command.as_ref() else {
            panic!("expected doctor command");
        };
        let config = load_doctor_config_from_cli(&cli, args).expect("live doctor config");
        assert!(config.http_headers.is_some(), "{probe} keeps live headers");
        assert_eq!(
            config.sandbox_api_key.as_deref(),
            Some("doctor-offline-sandbox-sentinel")
        );
        assert_eq!(
            config
                .search
                .as_ref()
                .and_then(|search| search.api_key.as_deref()),
            Some("doctor-offline-search-sentinel")
        );
    }
}

fn parse_cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("CLI args should parse")
}

fn make_server(command: Option<&str>, args: &[&str], url: Option<&str>) -> McpServerConfig {
    McpServerConfig {
        command: command.map(String::from),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: std::collections::HashMap::new(),
        cwd: None,
        url: url.map(String::from),
        transport: None,
        connect_timeout: None,
        execute_timeout: None,
        read_timeout: None,
        disabled: false,
        enabled: true,
        required: false,
        lazy: false,
        enabled_tools: Vec::new(),
        disabled_tools: Vec::new(),
        headers: std::collections::HashMap::new(),
        env_headers: std::collections::HashMap::new(),
        bearer_token_env_var: None,
        scopes: Vec::new(),
        oauth: None,
        oauth_resource: None,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    }
}

#[test]
fn doctor_does_not_expand_mcp_environment_placeholders() {
    let _lock = crate::test_support::lock_test_env();
    let _missing = crate::test_support::EnvVarGuard::remove("CODEWHALE_DOCTOR_MCP_MISSING_PATH");
    let mut server = make_server(Some("codewhale-mcp-command"), &[], None);
    server.env.insert(
        "PATH".to_string(),
        "do-not-leak-${CODEWHALE_DOCTOR_MCP_MISSING_PATH}-also-secret".to_string(),
    );

    let status = doctor_check_mcp_server(&server);
    assert!(matches!(status, McpServerDoctorStatus::Ok(_)));
    let report = doctor_mcp_server_json("invalid-env", &server);
    assert_eq!(report["checks"]["command"]["status"], "not_checked");
    let serialized = report.to_string();
    assert!(!serialized.contains("do-not-leak"));
    assert!(!serialized.contains("also-secret"));
    assert!(!serialized.contains("CODEWHALE_DOCTOR_MCP_MISSING_PATH"));
}
#[test]
fn doctor_does_not_resolve_or_echo_command_environment() {
    let sentinel = "MCP-PATH-VALUE-SENTINEL";
    let mut server = make_server(Some("mcp-command"), &[], None);
    server.env.insert("PATH".to_string(), sentinel.to_string());

    assert!(matches!(
        doctor_check_mcp_server(&server),
        McpServerDoctorStatus::Ok(_)
    ));
    let report = doctor_mcp_server_json("path-only", &server);
    assert_eq!(report["checks"]["command"]["status"], "not_checked");
    assert!(!report.to_string().contains(sentinel));
}
#[test]
fn doctor_mcp_reports_omit_missing_command_value() {
    let sentinel = "MCP-MISSING-COMMAND-SENTINEL";
    let server = make_server(Some(sentinel), &[], None);
    let status = doctor_check_mcp_server(&server);
    let human = status.detail().to_string();
    let json = doctor_mcp_server_json("missing-command", &server).to_string();

    assert!(!human.contains(sentinel));
    assert!(!json.contains(sentinel));
}
#[test]
fn doctor_mcp_reports_redact_url_argv_and_env_values() {
    let url_sentinels = [
        "MCP-URL-USER-SENTINEL",
        "MCP-URL-PASSWORD-SENTINEL",
        "MCP-URL-PATH-SENTINEL",
        "MCP-URL-QUERY-KEY-SENTINEL",
        "MCP-URL-QUERY-VALUE-SENTINEL",
        "MCP-URL-FRAGMENT-SENTINEL",
    ];
    let url = format!(
        "https://{}:{}@example.invalid:8443/{}/mcp?{}={}#{}",
        url_sentinels[0],
        url_sentinels[1],
        url_sentinels[2],
        url_sentinels[3],
        url_sentinels[4],
        url_sentinels[5]
    );
    let url_server = make_server(None, &[], Some(&url));
    let url_status = doctor_check_mcp_server(&url_server);
    let url_human = format!("configuration: {}", url_status.detail());
    let url_json = doctor_mcp_server_json("url-server", &url_server).to_string();

    for sentinel in url_sentinels {
        assert!(!url_human.contains(sentinel));
        assert!(!url_json.contains(sentinel));
    }
    assert!(url_human.contains("https://example.invalid:8443"));
    assert!(!url_human.contains("/mcp"));
    assert!(!url_human.contains('?'));
    assert!(!url_human.contains('#'));

    let executable = std::env::current_exe().expect("current test executable");
    let executable = executable.to_string_lossy();
    let argv_sentinels = [
        "MCP-BEARER-ARG-SENTINEL",
        "MCP-TOKEN-VALUE-SENTINEL",
        "MCP-OTHER-ARG-SENTINEL",
        "MCP-ENV-VALUE-SENTINEL",
    ];
    let mut stdio_server = make_server(
        Some(&executable),
        &[
            "--bearer=MCP-BEARER-ARG-SENTINEL",
            "--token",
            "MCP-TOKEN-VALUE-SENTINEL",
            "MCP-OTHER-ARG-SENTINEL",
        ],
        None,
    );
    stdio_server.env.insert(
        "MCP_TOKEN".to_string(),
        "MCP-ENV-VALUE-SENTINEL".to_string(),
    );
    let stdio_status = doctor_check_mcp_server(&stdio_server);
    let stdio_human = format!("configuration: {}", stdio_status.detail());
    let stdio_json = doctor_mcp_server_json("stdio-server", &stdio_server).to_string();

    for sentinel in argv_sentinels {
        assert!(!stdio_human.contains(sentinel));
        assert!(!stdio_json.contains(sentinel));
    }
    assert_eq!(
        doctor_mcp_server_json("stdio-server", &stdio_server)["args_count"],
        4
    );
    assert_eq!(
        doctor_mcp_server_json("stdio-server", &stdio_server)["env_count"],
        1
    );
}
#[test]
fn doctor_provider_json_redacts_every_credential_url_to_its_authority() {
    for row in codewhale_config::descriptors::provider_compatibility() {
        if row.kind == crate::config::ProviderKind::Antigravity {
            continue;
        }
        let provider = row.id;
        let config = Config {
            provider: Some(provider.to_string()),
            ..Config::default()
        };
        let report = doctor_provider_model_report_json(&config);
        for field in ["credential_url", "credential_docs_url"] {
            let Some(value) = report["auth"][field].as_str() else {
                continue;
            };
            assert_eq!(
                value,
                crate::doctor::structural_url_authority(value),
                "{provider:?} {field} must be authority-only"
            );
            assert!(
                value
                    .chars()
                    .all(|character| !matches!(character, '?' | '#' | '@')),
                "{provider:?} {field} must omit credential-bearing URL components"
            );
        }
    }
}
#[test]
fn doctor_provider_url_reports_omit_secret_capable_components() {
    let sentinels = [
        "PROVIDER-URL-USER-SENTINEL",
        "PROVIDER-URL-PASSWORD-SENTINEL",
        "PROVIDER-URL-PATH-SENTINEL",
        "PROVIDER-URL-QUERY-KEY-SENTINEL",
        "PROVIDER-URL-QUERY-VALUE-SENTINEL",
        "PROVIDER-URL-FRAGMENT-SENTINEL",
    ];
    let base_url = format!(
        "https://{}:{}@provider.example.invalid:9443/{}/v1?{}={}#{}",
        sentinels[0], sentinels[1], sentinels[2], sentinels[3], sentinels[4], sentinels[5]
    );
    let config = Config {
        ..Config::default()
    }
    .with_legacy_root(None, Some(base_url));
    let target = doctor_api_target(&config);
    let human = format!(
        "base_url: {}",
        crate::doctor::structural_url_authority(&target.base_url)
    );
    let json = serde_json::json!({
        "base_url": crate::doctor::structural_url_authority(&target.base_url),
        "route": doctor_route_report(&config),
    })
    .to_string();

    assert_eq!(human, "base_url: https://provider.example.invalid:9443");
    for sentinel in sentinels {
        assert!(!human.contains(sentinel));
        assert!(!json.contains(sentinel));
    }
}
#[test]
fn doctor_startup_skips_workspace_dotenv_loading() {
    use std::cell::Cell;

    for args in [
        vec!["codewhale", "doctor"],
        vec!["codewhale", "doctor", "--json"],
        vec!["codewhale", "doctor", "--context-json"],
        vec!["codewhale", "doctor", "--probe-mcp"],
        vec!["codewhale", "doctor", "--check-updates"],
    ] {
        let phase = Cell::new(0);
        let (_cli, command) = prepare_cli_startup(
            parse_cli(&args),
            || {
                assert_eq!(phase.get(), 0);
                phase.set(1);
            },
            || phase.set(2),
        );

        assert_eq!(phase.get(), 1, "doctor must not load workspace dotenv");
        assert!(matches!(command, Some(Commands::Doctor(_))));
    }
}
#[test]
fn explicit_doctor_api_probes_may_load_workspace_dotenv_credentials() {
    use std::cell::Cell;

    for args in [
        ["codewhale", "doctor", "--probe-api"],
        ["codewhale", "doctor", "--probe-local"],
    ] {
        let phase = Cell::new(0);
        let (_cli, command) = prepare_cli_startup(
            parse_cli(&args),
            || phase.set(1),
            || {
                assert_eq!(phase.get(), 1);
                phase.set(2);
            },
        );

        assert_eq!(
            phase.get(),
            2,
            "explicit API probes may resolve dotenv credentials"
        );
        assert!(matches!(command, Some(Commands::Doctor(_))));
    }
}
#[test]
fn hosted_provider_does_not_probe_without_explicit_opt_in() {
    assert!(!doctor_should_probe_api(
        crate::config::ProviderKind::Deepseek,
        "https://api.deepseek.com/beta",
        crate::doctor::DoctorProbeRequest::default(),
    ));
    assert!(doctor_should_probe_api(
        crate::config::ProviderKind::Deepseek,
        "https://api.deepseek.com/beta",
        crate::doctor::DoctorProbeRequest {
            probe_api: true,
            ..crate::doctor::DoctorProbeRequest::default()
        },
    ));
}
#[test]
fn resolve_api_key_source_does_not_clone_openai_codex_env_tokens() {
    let _guard = crate::test_support::lock_test_env();
    let sentinel = "OPENAI-CODEX-ACCESS-TOKEN-SENTINEL";
    let _token = crate::test_support::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", sentinel);
    let _legacy_token = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let config = Config {
        provider: Some("openai-codex".to_string()),
        ..Config::default()
    };

    let source = resolve_api_key_source(&config);
    let report = doctor_provider_model_report_json(&config).to_string();

    assert_eq!(source, ApiKeySource::OAuth);
    assert_eq!(doctor_api_key_source_label(source), "oauth_unprobed");
    assert!(!report.contains(sentinel));
}
#[test]
fn resolve_api_key_source_does_not_inspect_dispatcher_source_or_value() {
    let _guard = crate::test_support::lock_test_env();
    let prev = std::env::var("DEEPSEEK_API_KEY").ok();
    let prev_source = std::env::var("DEEPSEEK_API_KEY_SOURCE").ok();
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "test-helper-value");
        std::env::set_var("DEEPSEEK_API_KEY_SOURCE", "keyring");
    }
    let cfg = Config::default();
    let source = resolve_api_key_source(&cfg);
    match prev {
        Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY", value) },
        None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY") },
    }
    match prev_source {
        Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY_SOURCE", value) },
        None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY_SOURCE") },
    }
    assert_eq!(source, ApiKeySource::SecretStoreUnprobed);
}
#[test]
fn resolve_api_key_source_does_not_inspect_provider_env_values() {
    let _guard = crate::test_support::lock_test_env();
    let prev = std::env::var("DEEPSEEK_API_KEY").ok();
    let prev_source = std::env::var("DEEPSEEK_API_KEY_SOURCE").ok();
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "test-helper-value");
        std::env::remove_var("DEEPSEEK_API_KEY_SOURCE");
    }
    let cfg = Config::default();
    let source = resolve_api_key_source(&cfg);
    match prev {
        Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY", value) },
        None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY") },
    }
    match prev_source {
        Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY_SOURCE", value) },
        None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY_SOURCE") },
    }
    assert_eq!(source, ApiKeySource::SecretStoreUnprobed);
}
#[test]
fn resolve_api_key_source_does_not_open_standalone_secret_store() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("temp home");
    let codewhale_home = temp.path().join("codewhale-home");
    std::fs::create_dir_all(&codewhale_home).expect("create codewhale home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let secret_path = codewhale_home.join("secrets").join("secrets.json");
    std::fs::create_dir_all(secret_path.parent().unwrap()).expect("secret fixture dir");
    let sentinel = "not-json:doctor-must-not-read-this-secret";
    std::fs::write(&secret_path, sentinel).expect("secret fixture");

    assert_eq!(
        resolve_credential_diagnostic(&Config::default()),
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    );
    assert_eq!(std::fs::read_to_string(secret_path).unwrap(), sentinel);
}
#[test]
fn resolve_api_key_source_does_not_probe_system_keyring() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("temp home");
    let _home = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_HOME",
        temp.path().join("codewhale-home").as_os_str(),
    );
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "system");
    let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");

    assert_eq!(
        resolve_credential_diagnostic(&Config::default()),
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    );
}

#[test]
fn credential_diagnostic_distinguishes_literal_from_empty_config_keys() {
    let literal = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("TEST-LITERAL-CONFIG-KEY".to_string()), None);
    assert_eq!(
        resolve_credential_diagnostic(&literal),
        CredentialDiagnostic::new(
            ApiKeySource::ConfigDeclared,
            CredentialAvailability::Present,
        )
    );
    assert!(doctor_has_credentials_or_local_runtime(&literal));

    let mut providers = crate::config::ProvidersConfig::default();
    providers.openai.api_key = Some("  ".to_string());
    let empty = Config {
        provider: Some("openai".to_string()),
        providers: Some(providers),
        ..Config::default()
    };
    assert_eq!(
        resolve_credential_diagnostic(&empty),
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    );
    assert!(!doctor_has_credentials_or_local_runtime(&empty));

    let empty_root = Config {
        ..Config::default()
    }
    .with_legacy_root(Some(String::new()), None);
    assert_eq!(
        resolve_credential_diagnostic(&empty_root).source,
        ApiKeySource::SecretStoreUnprobed
    );
    assert!(!doctor_has_credentials_or_local_runtime(&empty_root));
}

#[test]
fn credential_diagnostic_treats_sentinel_as_unprobed_store_not_config() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("temp home");
    let codewhale_home = temp.path().join("codewhale-home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some(crate::config::API_KEYRING_SENTINEL.to_string()), None);

    assert_eq!(
        resolve_credential_diagnostic(&config),
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    );
    assert!(!doctor_has_credentials_or_local_runtime(&config));
    assert!(!codewhale_home.join("secrets/secrets.json").exists());

    for sentinel in [crate::config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.api_key = Some(sentinel.to_string());
        let official = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Config::default()
        };
        assert_eq!(
            resolve_credential_diagnostic(&official),
            CredentialDiagnostic::new(
                ApiKeySource::SecretStoreUnprobed,
                CredentialAvailability::NotProbed,
            )
        );
    }
}

#[test]
fn unavailable_sentinel_routes_stay_distinct_from_unknown_and_unprobed() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("temp home");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let _home =
        crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("codewhale-home"));

    for sentinel in [crate::config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "sentinel-route".to_string(),
            crate::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://gateway.example.test/v1".to_string()),
                model: Some("test-model".to_string()),
                api_key: Some(sentinel.to_string()),
                ..Default::default()
            },
        );
        let named_custom = Config {
            provider: Some("sentinel-route".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Config::default()
        };
        assert_eq!(
            resolve_credential_diagnostic(&named_custom),
            CredentialDiagnostic::new(
                ApiKeySource::SecretStoreUnavailable,
                CredentialAvailability::Unavailable,
            )
        );
        assert!(!doctor_has_credentials_or_local_runtime(&named_custom));
        let setup = doctor_setup_report_json(&named_custom, &workspace);
        assert_eq!(setup["credential"]["source"], "secret_store_unavailable");
        assert_eq!(setup["credential"]["availability"], "unavailable");
        assert_eq!(setup["credential"]["ready"], false);
        assert_eq!(
            setup["provider_model"]["auth"]["availability"],
            "unavailable"
        );
        assert_eq!(
            setup["operate_fleet"]["provider"]["auth"]["availability"],
            "unavailable"
        );
        assert_eq!(setup["operate_fleet"]["ready"], false);
        assert_eq!(
            doctor_route_report(&named_custom)["auth"]["availability"],
            "unavailable"
        );
    }

    let mut providers = crate::config::ProvidersConfig::default();
    providers.openrouter.base_url = Some("https://gateway.example.test/v1".to_string());
    providers.openrouter.api_key = Some(crate::config::API_KEYRING_SENTINEL.to_string());
    let custom_endpoint = Config {
        provider: Some("openrouter".to_string()),
        providers: Some(providers),
        ..Config::default()
    };
    assert_eq!(
        resolve_credential_diagnostic(&custom_endpoint),
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnavailable,
            CredentialAvailability::Unavailable,
        )
    );

    let mut custom = std::collections::HashMap::new();
    custom.insert(
        "empty-route".to_string(),
        crate::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("https://empty.example.test/v1".to_string()),
            model: Some("test-model".to_string()),
            api_key: Some("  ".to_string()),
            ..Default::default()
        },
    );
    let empty_custom = Config {
        provider: Some("empty-route".to_string()),
        providers: Some(crate::config::ProvidersConfig {
            custom,
            ..Default::default()
        }),
        ..Config::default()
    };
    assert_eq!(
        resolve_credential_diagnostic(&empty_custom),
        CredentialDiagnostic::new(ApiKeySource::Unknown, CredentialAvailability::Unknown)
    );
}

#[test]
fn sentinel_placeholders_never_become_attemptable_routes_or_metered_evidence() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("isolated credential home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _xai = crate::test_support::EnvVarGuard::remove("XAI_API_KEY");
    let _cli_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
    let _cli_key = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
    let _mimo_mode = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_MODE");
    let _mimo_base = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_BASE_URL");
    let _mimo_plan = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
    let _mimo_plan_alias = crate::test_support::EnvVarGuard::remove("MIMO_TOKEN_PLAN_API_KEY");
    let _mimo_key = crate::test_support::EnvVarGuard::remove("XIAOMI_MIMO_API_KEY");
    let _xiaomi_key = crate::test_support::EnvVarGuard::remove("XIAOMI_API_KEY");
    let _mimo_key_alias = crate::test_support::EnvVarGuard::remove("MIMO_API_KEY");

    for sentinel in [crate::config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        let named_custom = Config {
            provider: Some("sentinel-route".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "sentinel-route".to_string(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("https://gateway.example.test/v1".to_string()),
                        model: Some("sentinel-model".to_string()),
                        api_key: Some(sentinel.to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            crate::provider_readiness::credential_state_for_provider(
                &named_custom,
                &(named_custom).test_identity_for_kind(crate::config::ProviderKind::Custom),
            ),
            crate::provider_readiness::CredentialState::MissingKey
        );
        let readiness = crate::provider_readiness::resolve_for_model(
            &named_custom,
            &(named_custom).test_identity_for_kind(crate::config::ProviderKind::Custom),
            "sentinel-model",
            &crate::provider_readiness::ProviderReadinessSnapshot::default(),
        );
        assert_eq!(
            readiness,
            crate::provider_readiness::ResolvedProviderReadiness::MissingKey
        );
        assert!(!readiness.can_attempt());
        assert!(
            crate::model_inventory::ModelInventory::from_config(&named_custom)
                .unwrap()
                .candidates
                .iter()
                .all(|candidate| candidate.provider != crate::config::ProviderKind::Custom)
        );

        let xai = Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    api_key: Some(sentinel.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            crate::provider_readiness::credential_state_for_provider(
                &xai,
                &(xai).test_identity_for_kind(crate::config::ProviderKind::Xai),
            ),
            crate::provider_readiness::CredentialState::MissingKey
        );

        let mut xiaomi_providers = crate::config::ProvidersConfig::default();
        xiaomi_providers.xiaomi_mimo.api_key = Some(sentinel.to_string());
        let xiaomi = Config {
            providers: Some(xiaomi_providers),
            ..Default::default()
        };
        assert_eq!(
            crate::route_billing::for_route(
                &xiaomi,
                &(xiaomi).test_identity_for_kind(crate::config::ProviderKind::XiaomiMimo)
            ),
            crate::route_billing::BillingPresentation::Subscription("MiMo token plan")
        );
    }
}

#[test]
fn sentinel_diagnostic_yields_to_route_bound_env_or_auth_declaration() {
    for sentinel in [crate::config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.api_key = Some(sentinel.to_string());
        providers.openai.api_key_env = Some("OFFICIAL_SENTINEL_ROUTE_KEY".to_string());
        let official = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Config::default()
        };
        assert_eq!(
            resolve_credential_diagnostic(&official),
            CredentialDiagnostic::new(ApiKeySource::EnvDeclared, CredentialAvailability::NotProbed,)
        );

        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "sentinel-route".to_string(),
            crate::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://gateway.example.test/v1".to_string()),
                model: Some("test-model".to_string()),
                api_key: Some(sentinel.to_string()),
                api_key_env: Some("CUSTOM_SENTINEL_ROUTE_KEY".to_string()),
                ..Default::default()
            },
        );
        let named_custom = Config {
            provider: Some("sentinel-route".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Config::default()
        };
        assert_eq!(
            resolve_credential_diagnostic(&named_custom),
            CredentialDiagnostic::new(ApiKeySource::EnvDeclared, CredentialAvailability::NotProbed,)
        );

        let mut external = std::collections::HashMap::new();
        external.insert(
            "external-sentinel-route".to_string(),
            crate::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://external.example.test/v1".to_string()),
                model: Some("test-model".to_string()),
                api_key: Some(sentinel.to_string()),
                auth: Some(codewhale_config::ProviderAuthSourceToml {
                    source: codewhale_config::AuthSourceKind::Command,
                    command: vec!["MUST-NOT-RUN".to_string()],
                    timeout_ms: None,
                    secret_id: None,
                }),
                ..Default::default()
            },
        );
        let named_external = Config {
            provider: Some("external-sentinel-route".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom: external,
                ..Default::default()
            }),
            ..Config::default()
        };
        assert_eq!(
            resolve_credential_diagnostic(&named_external),
            CredentialDiagnostic::new(
                ApiKeySource::ExternalAuthDeclared,
                CredentialAvailability::NotProbed,
            )
        );
    }
}

#[test]
fn credential_declarations_do_not_certify_setup_or_fleet_readiness() {
    let _lock = crate::test_support::lock_test_env();
    let temp = TempDir::new().expect("temp home");
    let codewhale_home = temp.path().join("codewhale-home");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &codewhale_home);
    let _declared_value =
        crate::test_support::EnvVarGuard::set("TEST_DECLARED_API_KEY", "MUST-NOT-BE-READ");

    let mut providers = crate::config::ProvidersConfig::default();
    providers.openai.api_key_env = Some("TEST_DECLARED_API_KEY".to_string());
    let env_config = Config {
        provider: Some("openai".to_string()),
        providers: Some(providers),
        ..Config::default()
    };
    let env_diagnostic = resolve_credential_diagnostic(&env_config);
    assert_eq!(env_diagnostic.source, ApiKeySource::EnvDeclared);
    assert_eq!(
        env_diagnostic.availability,
        CredentialAvailability::NotProbed
    );
    let env_setup = doctor_setup_report_json(&env_config, &workspace);
    assert_eq!(env_setup["credential"]["ready"], false);
    assert_eq!(
        env_setup["operate_fleet"]["provider"]["auth"]["present_or_local"],
        false
    );
    assert_eq!(env_setup["operate_fleet"]["ready"], false);
    assert!(!env_setup.to_string().contains("MUST-NOT-BE-READ"));

    let mut providers = crate::config::ProvidersConfig::default();
    providers.openai.auth = Some(codewhale_config::ProviderAuthSourceToml {
        source: codewhale_config::AuthSourceKind::Command,
        command: vec!["MUST-NOT-RUN".to_string()],
        timeout_ms: None,
        secret_id: None,
    });
    let external_config = Config {
        provider: Some("openai".to_string()),
        providers: Some(providers),
        ..Config::default()
    };
    let external_diagnostic = resolve_credential_diagnostic(&external_config);
    assert_eq!(
        external_diagnostic,
        CredentialDiagnostic::new(
            ApiKeySource::ExternalAuthDeclared,
            CredentialAvailability::NotProbed,
        )
    );
    assert!(!doctor_has_credentials_or_local_runtime(&external_config));
}

#[test]
fn credential_diagnostic_certifies_only_no_auth_and_local_runtime_without_keys() {
    let mut providers = crate::config::ProvidersConfig::default();
    providers.openrouter.auth_mode = Some("none".to_string());
    let no_auth = Config {
        provider: Some("openrouter".to_string()),
        providers: Some(providers),
        ..Config::default()
    };
    assert_eq!(
        resolve_credential_diagnostic(&no_auth),
        CredentialDiagnostic::new(ApiKeySource::NoAuth, CredentialAvailability::NotRequired,)
    );
    assert!(doctor_has_credentials_or_local_runtime(&no_auth));

    let local = Config {
        provider: Some("ollama".to_string()),
        ..Config::default()
    };
    assert_eq!(
        resolve_credential_diagnostic(&local),
        CredentialDiagnostic::new(
            ApiKeySource::LocalRuntime,
            CredentialAvailability::NotRequired,
        )
    );
    assert!(doctor_has_credentials_or_local_runtime(&local));
}
#[test]
fn test_bare_stdio_command_is_structurally_valid_without_resolution() {
    #[cfg(unix)]
    let command = "sh";
    #[cfg(windows)]
    let command = "cmd";
    let server = make_server(Some(command), &["serve", "--mcp"], None);
    match doctor_check_mcp_server(&server) {
        McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("command omitted")),
        other => panic!("Expected structural Ok, got {other:?}"),
    }
}

#[test]
fn receipts_cli_parses_id_last_turn_and_format() {
    use clap::Parser as _;
    let cli = crate::Cli::try_parse_from(["codewhale", "receipts", "--last", "--format", "json"])
        .expect("parse --last");
    assert!(matches!(
        cli.command,
        Some(crate::Commands::Receipts {
            last: true,
            id: None,
            format: crate::receipts::ReceiptFormat::Json,
            ..
        })
    ));
    let cli = crate::Cli::try_parse_from(["codewhale", "receipt", "thr_1", "--turn", "turn_2"])
        .expect("parse alias");
    assert!(matches!(
        cli.command,
        Some(crate::Commands::Receipts { ref id, ref turn, format: crate::receipts::ReceiptFormat::Md, .. })
            if id.as_deref() == Some("thr_1") && turn.as_deref() == Some("turn_2")
    ));
    assert!(
        crate::Cli::try_parse_from(["codewhale", "receipts", "abc", "--last"]).is_err(),
        "an id and --last conflict"
    );
}

use super::*;

#[test]
fn default_probe_request_is_fully_offline() {
    let request = DoctorProbeRequest::default();
    assert!(!request.should_check_updates());
    assert!(!request.should_probe_api(false));
    assert!(!request.should_probe_api(true));
    assert!(!request.should_probe_mcp());
    assert!(!request.should_probe_search());
}

fn ds4_config() -> codewhale_runtime::config::Config {
    let mut providers = codewhale_runtime::config::ProvidersConfig::default();
    providers.custom.insert(
        "ds4".to_string(),
        codewhale_runtime::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:8000/v1".to_string()),
            model: Some("deepseek-v4-flash".to_string()),
            auth_mode: Some("none".to_string()),
            ..Default::default()
        },
    );
    {
        let __cw_config_init_0_0 = Some("ds4".to_string());
        let __cw_config_init_0_1 = Some(providers);
        let mut __cw_config_init_0_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
}

#[test]
fn recognizes_only_the_explicit_keyless_local_ds4_route() {
    let ds4 = ds4_config();
    assert!(is_keyless_ds4_route(&ds4));

    let mut hosted = ds4.clone();
    hosted
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("ds4"))
        .expect("DS4 config")
        .base_url = Some("https://ds4.example/v1".to_string());
    assert!(!is_keyless_ds4_route(&hosted));
}

#[test]
fn ds4_probe_error_reports_status_without_response_body() {
    let mut config = ds4_config();
    config
        .providers
        .as_mut()
        .and_then(|providers| providers.custom.get_mut("ds4"))
        .expect("DS4 config")
        .base_url = Some("http://user:secret@127.0.0.1:8000/v1?token=secret".to_string());
    let message = ds4_probe_error(
        &config,
        "Failed to list models: HTTP 404: secret backend response",
    );

    assert!(message.contains("HTTP 404"));
    assert!(!message.contains("secret backend response"));
    assert!(!message.contains("secret"));
}

#[test]
fn update_renderer_omits_untrusted_release_tags_and_errors() {
    let release_sentinel = "v9.9.9?token=doctor-update-sentinel";
    let error_sentinel = "https://user:doctor-update-error-sentinel@example.test/path";

    let metadata = doctor_update_report("0.9.3", Ok::<String, ()>(release_sentinel.to_string()));
    let transport = doctor_update_report("0.9.3", Err(error_sentinel.to_string()));
    let rendered = [
        doctor_update_report_lines(&metadata, "codewhale update").join("\n"),
        doctor_update_report_lines(&transport, "codewhale update").join("\n"),
    ]
    .join("\n");

    assert_eq!(metadata, DoctorUpdateReport::ReleaseMetadataInvalid);
    assert_eq!(transport, DoctorUpdateReport::ReleaseCheckFailed);
    assert!(!rendered.contains(release_sentinel));
    assert!(!rendered.contains(error_sentinel));
    assert!(rendered.contains("details omitted"));
}

#[test]
fn update_renderer_canonicalizes_safe_release_tags() {
    let report = doctor_update_report("0.9.3", Ok::<String, ()>(" v0.9.4 ".to_string()));
    assert_eq!(
        doctor_update_report_lines(&report, "codewhale update"),
        vec![
            "latest: v0.9.4".to_string(),
            "Update available. Run `codewhale update` to install.".to_string(),
        ]
    );
}

#[test]
fn update_renderer_names_the_package_manager_for_managed_installs() {
    let report = doctor_update_report("0.9.3", Ok::<String, ()>("v0.9.4".to_string()));
    let npm = codewhale_release::InstallMethod::Npm.update_command();
    let lines = doctor_update_report_lines(&report, npm);
    assert_eq!(
        lines[1],
        "Update available. Run `npm install -g codewhale@latest` to install."
    );
    assert!(
        !lines.join("\n").contains("`codewhale update`"),
        "an npm-owned binary must not be told to self-update: {lines:?}"
    );
}

#[test]
fn live_probe_flags_open_only_their_owned_boundary() {
    let update = DoctorProbeRequest {
        check_updates: true,
        ..DoctorProbeRequest::default()
    };
    assert!(update.should_check_updates());
    assert!(!update.should_probe_api(false));
    assert!(!update.should_probe_api(true));

    let hosted = DoctorProbeRequest {
        probe_api: true,
        ..DoctorProbeRequest::default()
    };
    assert!(hosted.should_probe_api(false));
    assert!(!hosted.should_probe_api(true));

    let local = DoctorProbeRequest {
        probe_local: true,
        ..DoctorProbeRequest::default()
    };
    assert!(!local.should_probe_api(false));
    assert!(local.should_probe_api(true));

    let search = DoctorProbeRequest {
        probe_search: true,
        ..DoctorProbeRequest::default()
    };
    assert!(search.should_probe_search());
    assert!(!search.should_check_updates());
    assert!(!search.should_probe_api(false));
    assert!(!search.should_probe_mcp());
}

#[tokio::test]
async fn search_probe_counts_any_http_response_as_transport_only() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let config = {
        let __cw_config_init_1_0 = Some(codewhale_runtime::config::SearchConfig {
            provider: Some(codewhale_runtime::config::SearchProvider::DuckDuckGo),
            base_url: Some(server.uri()),
            api_key: None,
            native: None,
        });
        let mut __cw_config_init_1_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_1_value.search = __cw_config_init_1_0;
        __cw_config_init_1_value
    };

    let report = doctor_search_probe(
        &config,
        DoctorProbeRequest {
            probe_search: true,
            ..DoctorProbeRequest::default()
        },
    )
    .await;
    let lines = doctor_search_probe_lines(&report).join("\n");

    assert!(matches!(
        report,
        DoctorSearchProbeReport::Reachable { status: 401, .. }
    ));
    assert!(lines.contains("responded (HTTP 401)"), "{lines}");
    assert!(lines.contains("Transport only"), "{lines}");
    assert!(lines.contains("authentication and search results were not tested"));
    assert!(!lines.contains("search successful"));
    let requests = server.received_requests().await.expect("recorded probe");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.query().is_none());
    assert!(requests[0].body.is_empty());
    assert!(requests[0].headers.get("authorization").is_none());
    assert!(requests[0].headers.get("x-api-key").is_none());
    assert!(requests[0].headers.get("cookie").is_none());
}

#[tokio::test]
async fn search_probe_does_not_follow_redirects() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/redirected"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/redirected"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let config = {
        let __cw_config_init_2_0 = Some(codewhale_runtime::config::SearchConfig {
            provider: Some(codewhale_runtime::config::SearchProvider::Searxng),
            base_url: Some(server.uri()),
            api_key: None,
            native: None,
        });
        let mut __cw_config_init_2_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_2_value.search = __cw_config_init_2_0;
        __cw_config_init_2_value
    };

    let report = doctor_search_probe(
        &config,
        DoctorProbeRequest {
            probe_search: true,
            ..DoctorProbeRequest::default()
        },
    )
    .await;

    assert!(matches!(
        report,
        DoctorSearchProbeReport::Reachable { status: 302, .. }
    ));
}

#[tokio::test]
async fn search_probe_respects_network_policy_without_contacting_the_authority() {
    let config = {
        let __cw_config_init_3_0 = Some(codewhale_runtime::config::SearchConfig {
            provider: Some(codewhale_runtime::config::SearchProvider::Searxng),
            base_url: Some("https://search.example/private?token=secret".to_string()),
            api_key: None,
            native: None,
        });
        let __cw_config_init_3_1 = Some(codewhale_config::NetworkPolicyToml {
            default: "allow".to_string(),
            deny: vec!["search.example".to_string()],
            ..Default::default()
        });
        let mut __cw_config_init_3_value: codewhale_runtime::config::Config = Default::default();
        __cw_config_init_3_value.search = __cw_config_init_3_0;
        __cw_config_init_3_value.network = __cw_config_init_3_1;
        __cw_config_init_3_value
    };

    let report = doctor_search_probe(
        &config,
        DoctorProbeRequest {
            probe_search: true,
            ..DoctorProbeRequest::default()
        },
    )
    .await;
    let lines = doctor_search_probe_lines(&report).join("\n");

    assert_eq!(
        report,
        DoctorSearchProbeReport::PolicyDenied {
            authority: "https://search.example".to_string()
        }
    );
    assert!(!lines.contains("private"));
    assert!(!lines.contains("token"));
    assert!(!lines.contains("secret"));
}

#[tokio::test]
async fn search_probe_skips_the_default_provider_when_web_search_is_disabled() {
    let mut config = codewhale_runtime::config::Config::default();
    config
        .set_feature("web_search", false)
        .expect("known feature");

    let report = doctor_search_probe(
        &config,
        DoctorProbeRequest {
            probe_search: true,
            ..DoctorProbeRequest::default()
        },
    )
    .await;

    assert_eq!(report, DoctorSearchProbeReport::FeatureDisabled);
    assert!(
        doctor_search_probe_lines(&report)
            .join("\n")
            .contains("web_search feature is disabled")
    );
}

#[test]
fn explicit_codewhale_home_owns_every_default_user_path() {
    let _lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().join("isolated-codewhale-home");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
    let _config = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
    let _legacy_config = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
    let _automations = crate::test_support::EnvVarGuard::remove("CODEWHALE_AUTOMATIONS_DIR");
    let _legacy_automations = crate::test_support::EnvVarGuard::remove("DEEPSEEK_AUTOMATIONS_DIR");
    let _tasks = crate::test_support::EnvVarGuard::remove("CODEWHALE_TASKS_DIR");
    let _legacy_tasks = crate::test_support::EnvVarGuard::remove("DEEPSEEK_TASKS_DIR");
    let _runtime = crate::test_support::EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
    let _legacy_runtime = crate::test_support::EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

    let report = DoctorPathReport::resolve(None).expect("resolve doctor paths");
    let task_manager_root = crate::task_manager::default_tasks_dir();
    let runtime_config = crate::runtime_threads::RuntimeThreadManagerConfig::from_task_data_dir(
        task_manager_root.clone(),
    );
    let (secrets, legacy_secrets) =
        codewhale_secrets::FileKeyringStore::default_paths_read_only().expect("secret paths");

    assert_eq!(report.home, home);
    assert_eq!(report.config, home.join("config.toml"));
    assert_eq!(report.settings, home.join("settings.toml"));
    assert_eq!(report.sessions, home.join("sessions"));
    assert_eq!(report.logs, home.join("logs"));
    assert_eq!(report.automations, home.join("automations"));
    assert_eq!(report.task_manager_root, task_manager_root);
    assert_eq!(report.task_manager_tasks, task_manager_root.join("tasks"));
    assert_eq!(
        report.task_manager_artifacts,
        task_manager_root.join("artifacts")
    );
    assert_eq!(report.runtime_store, runtime_config.data_dir);
    assert_eq!(
        report.runtime_events,
        runtime_config.data_dir.join("events")
    );
    assert_eq!(
        report.personal_fleet_definitions,
        crate::fleet::exact::personal_fleet_definitions_dir().expect("personal fleets")
    );
    assert_eq!(
        report.personal_fleet_agents,
        crate::fleet::profile::personal_agent_profile_dir().expect("personal agents")
    );
    assert_eq!(report.secrets, secrets);
    assert_eq!(legacy_secrets, None);
    assert_eq!(report.entries().len(), 14);
    let json = serde_json::to_value(&report).expect("serialize path snapshot");
    for (label, path) in report.entries() {
        assert_eq!(
            json[label].as_str(),
            Some(path.to_string_lossy().as_ref()),
            "human and JSON path snapshots diverged for {label}"
        );
    }
    assert!(
        !home.exists(),
        "path reporting must not create the configured home"
    );
}

#[test]
fn explicit_relative_config_matches_the_canonical_loader_path() {
    let relative = Path::new("fixtures/relative-doctor-config.toml");
    let expected = codewhale_config::resolve_config_path(Some(relative.to_path_buf()))
        .expect("canonical config path");

    let report = DoctorPathReport::resolve(Some(relative)).expect("resolve doctor paths");

    assert_eq!(report.config, expected);
    assert!(report.config.is_absolute());
}

#[test]
fn path_report_json_contains_no_secret_file_contents() {
    let _lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().join("isolated-codewhale-home");
    let secret_path = home.join("secrets").join("secrets.json");
    std::fs::create_dir_all(secret_path.parent().unwrap()).expect("secret dir fixture");
    let sentinel = "doctor-path-report-secret-sentinel";
    std::fs::write(&secret_path, sentinel).expect("secret fixture");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());

    let report = DoctorPathReport::resolve(None).expect("resolve doctor paths");
    let json = serde_json::to_string(&report).expect("serialize path report");

    assert!(!json.contains(sentinel));
    assert_eq!(std::fs::read_to_string(secret_path).unwrap(), sentinel);
}

#[test]
fn human_and_json_backend_reports_never_include_secret_file_contents() {
    let _lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path().join("isolated-codewhale-home");
    let secret_path = home.join("secrets").join("secrets.json");
    std::fs::create_dir_all(secret_path.parent().unwrap()).expect("secret dir fixture");
    let sentinel = "doctor-render-secret-sentinel";
    std::fs::write(&secret_path, format!("not-json:{sentinel}")).expect("secret fixture");
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.as_os_str());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");

    let diagnostic = codewhale_secrets::diagnose_secret_backend();
    let human = secret_backend_human_lines(&diagnostic).join("\n");
    let json = serde_json::to_string(&diagnostic).expect("serialize backend diagnostic");

    assert!(!human.contains(sentinel));
    assert!(!json.contains(sentinel));
    assert_eq!(
        std::fs::read_to_string(secret_path).unwrap(),
        format!("not-json:{sentinel}")
    );
}

#[test]
fn structural_url_authority_omits_every_secret_capable_component() {
    let sentinels = [
        "URL-USER-SENTINEL",
        "URL-PASSWORD-SENTINEL",
        "URL-PATH-SENTINEL",
        "URL-QUERY-KEY-SENTINEL",
        "URL-QUERY-VALUE-SENTINEL",
        "URL-FRAGMENT-SENTINEL",
    ];
    let raw = format!(
        "https://{}:{}@example.invalid:8443/{}/child?{}={}#{}",
        sentinels[0], sentinels[1], sentinels[2], sentinels[3], sentinels[4], sentinels[5]
    );

    let authority = codewhale_config::structural_url_authority(&raw);

    assert_eq!(authority, "https://example.invalid:8443");
    for sentinel in sentinels {
        assert!(!authority.contains(sentinel));
    }
    assert_eq!(
        codewhale_config::structural_url_authority("http://[::1]:9000/private?token=review-secret"),
        "http://[::1]:9000"
    );
}

#[test]
fn credential_shaped_config_values_are_flagged_by_key_name_only() {
    // Fixture tokens stay low-entropy on purpose: realistic random strings
    // trip secret scanners (GitGuardian flagged the originals as live credentials).
    let raw = r#"
# comment with sk-not-a-real-line
model = "deepseek-v4-flash"
base_url = "https://api.moonshot.ai/kimi-code/v1"
chatgpt_access_token = "eyJ0000000000000000000000000"
moonshot_api_key = "[redacted]"
provider_api_key = "sk-test0000000000000000"
workspace_token_note = "short"
random_id = "0123456789abcdef0123456789abcdef"
"#;
    let flagged = super::config_credential_shaped_keys(raw);
    assert_eq!(flagged, vec!["chatgpt_access_token", "provider_api_key"]);
}

#[test]
fn credential_scan_ignores_urls_models_and_redacted_entries() {
    let raw = r#"
model = "kimi-k3-instruct-preview-2026"
endpoint = "https://example.com/v1?key=nope"
api_key = "[redacted]"
"#;
    assert!(super::config_credential_shaped_keys(raw).is_empty());
}

use super::headers::{MCP_HTTP_ACCEPT, is_safe_custom_header, with_default_mcp_http_headers};
use super::http::HttpTransport;
use super::http_client::McpHttpAuth;
use super::streamable_http::StreamableHttpTransport;
use super::wire::{
    find_sse_event_separator, find_sse_event_separator_bytes, is_mcp_stale_session_error,
    parse_sse_message_data,
};
use super::*;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};

fn test_http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    crate::tls::reqwest_client()
}

async fn lock_mcp_loopback_tests() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

struct WorkspaceTrustConfigGuard {
    config_path: PathBuf,
    _codewhale_config_path: crate::test_support::EnvVarGuard,
    _deepseek_config_path: crate::test_support::EnvVarGuard,
    _env_lock: crate::test_support::TestEnvLock,
}

fn workspace_trust_config_guard(workspace: &Path) -> WorkspaceTrustConfigGuard {
    let env_lock = crate::test_support::lock_test_env();
    let config_path = workspace
        .parent()
        .unwrap_or(workspace)
        .join("user-config")
        .join("config.toml");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let codewhale_config_path =
        crate::test_support::EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
    let deepseek_config_path = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

    WorkspaceTrustConfigGuard {
        config_path,
        _codewhale_config_path: codewhale_config_path,
        _deepseek_config_path: deepseek_config_path,
        _env_lock: env_lock,
    }
}

fn write_workspace_trust_config(config_path: &Path, workspace: &Path) {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let key = workspace
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        config_path,
        format!("[projects.\"{key}\"]\ntrust_level = \"trusted\"\n"),
    )
    .unwrap();
}

fn mark_workspace_trusted(workspace: &Path) -> WorkspaceTrustConfigGuard {
    let guard = workspace_trust_config_guard(workspace);
    write_workspace_trust_config(&guard.config_path, workspace);
    guard
}

#[test]
fn test_mcp_config_defaults() {
    let config = McpConfig::default();
    assert_eq!(config.timeouts.connect_timeout, 30);
    assert_eq!(config.timeouts.execute_timeout, 1800);
    assert_eq!(config.timeouts.read_timeout, 120);
    assert!(config.servers.is_empty());
}

#[test]
fn reviewed_remote_endpoint_identity_normalizes_case_idna_and_default_ports() {
    let canonical = reviewed_remote_endpoint_identity("https://example.com/mcp").unwrap();
    assert_eq!(
        reviewed_remote_endpoint_identity("https://EXAMPLE.COM:443/mcp").unwrap(),
        canonical
    );
    assert_eq!(
        reviewed_remote_endpoint_identity("https://BÜCHER.example:443/mcp").unwrap(),
        reviewed_remote_endpoint_identity("https://xn--bcher-kva.example/mcp").unwrap()
    );
    assert_ne!(
        reviewed_remote_endpoint_identity("https://example.com:444/mcp").unwrap(),
        canonical
    );
    assert!(reviewed_remote_endpoint_identity("http://localhost:8080/mcp").is_ok());
    assert!(reviewed_remote_endpoint_identity("http://127.0.0.1/mcp").is_ok());
    assert!(reviewed_remote_endpoint_identity("http://[::1]/mcp").is_ok());
}

#[test]
fn reviewed_remote_endpoint_identity_rejects_ambiguous_or_secret_bearing_urls() {
    for endpoint in [
        "http://example.com/mcp",
        "ftp://example.com/mcp",
        "https://user@example.com/mcp",
        "https://user:secret@example.com/mcp",
        "https://example.com/mcp?token=secret",
        "https://example.com/mcp#fragment",
    ] {
        let error = reviewed_remote_endpoint_identity(endpoint)
            .expect_err("unsafe reviewed endpoint must fail closed")
            .to_string();
        assert!(
            !error.contains("secret"),
            "endpoint error leaked URL material"
        );
    }
}

#[test]
fn reviewed_plugin_redirects_are_exact_normalized_origin_only() {
    let approved = reviewed_remote_endpoint_identity("https://BÜCHER.example:443/mcp")
        .unwrap()
        .1;
    let accepted = [
        "https://xn--bcher-kva.example/next",
        "https://BÜCHER.example:443/next?cursor=opaque",
    ];
    for endpoint in accepted {
        assert!(reviewed_redirect_matches_origin(
            &reqwest::Url::parse(endpoint).unwrap(),
            &approved
        ));
    }

    let rejected = [
        "http://xn--bcher-kva.example/next",
        "https://user@xn--bcher-kva.example/next",
        "https://xn--bcher-kva.example:444/next",
        "https://other.example/next",
    ];
    for endpoint in rejected {
        assert!(!reviewed_redirect_matches_origin(
            &reqwest::Url::parse(endpoint).unwrap(),
            &approved
        ));
    }
}

#[test]
fn reviewed_plugin_remote_proxy_policy_never_reads_ambient_environment() {
    let reads = std::cell::Cell::new(0_u32);
    let proxy = configured_mcp_proxy(
        &reqwest::Url::parse("https://example.com/mcp").unwrap(),
        true,
        |_| {
            reads.set(reads.get() + 1);
            Ok("http://127.0.0.1:9999".to_string())
        },
    );

    assert_eq!(
        reads.get(),
        0,
        "reviewed remotes must not read proxy values"
    );
    assert!(proxy.unwrap().is_none());
}

#[test]
fn user_authored_mcp_proxy_policy_keeps_environment_support() {
    let requested = std::cell::RefCell::new(Vec::new());
    let proxy = configured_mcp_proxy(
        &reqwest::Url::parse("https://example.com/mcp").unwrap(),
        false,
        |name| {
            requested.borrow_mut().push(name.to_string());
            match name {
                "HTTPS_PROXY" => Ok("http://127.0.0.1:8080".to_string()),
                _ => Err(std::env::VarError::NotPresent),
            }
        },
    );

    assert_eq!(
        requested.into_inner(),
        vec![
            "HTTPS_PROXY".to_string(),
            "NO_PROXY".to_string(),
            "no_proxy".to_string(),
        ]
    );
    assert!(proxy.unwrap().is_some());
}

#[test]
fn test_mcp_config_parse() {
    let json = r#"{
        "timeouts": {
            "connect_timeout": 15,
            "execute_timeout": 90
        },
        "servers": {
            "test": {
                "command": "node",
                "args": ["server.js"],
                "env": {"FOO": "bar"}
            }
        }
    }"#;

    let config: McpConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.timeouts.connect_timeout, 15);
    assert_eq!(config.timeouts.execute_timeout, 90);
    assert_eq!(config.timeouts.read_timeout, 120); // default
    assert!(config.servers.contains_key("test"));

    let server = config.servers.get("test").unwrap();
    assert_eq!(server.command, Some("node".to_string()));
    assert_eq!(server.args, vec!["server.js"]);
    assert_eq!(server.env.get("FOO"), Some(&"bar".to_string()));
}

#[test]
fn mcp_pool_parse_prefixed_name_rejects_ambiguous_configured_server_prefixes() {
    let config: McpConfig = serde_json::from_str(
        r#"{
            "servers": {
                "my": {"command": "node"},
                "my_db": {"command": "node"}
            }
        }"#,
    )
    .unwrap();
    let pool = McpPool::new(config);

    let error = pool
        .parse_prefixed_name("mcp_my_db_execute_sql")
        .expect_err("configured server-prefix collisions must fail closed");
    assert!(error.to_string().contains("Unknown MCP tool name"));
}

#[test]
fn mcp_server_config_parses_custom_headers() {
    let json = r#"{
        "servers": {
            "hf": {
                "url": "https://example.invalid/mcp",
                "headers": {
                    "Authorization": "Bearer tok",
                    "X-Org": "anthropic"
                }
            }
        }
    }"#;
    let cfg: McpConfig = serde_json::from_str(json).unwrap();
    let hf = cfg.servers.get("hf").expect("server present");
    assert_eq!(
        hf.headers.get("Authorization"),
        Some(&"Bearer tok".to_string())
    );
    assert_eq!(hf.headers.get("X-Org"), Some(&"anthropic".to_string()));
}

#[test]
fn mcp_server_config_parses_remote_auth_fields() {
    let json = r#"{
        "servers": {
            "remote": {
                "url": "https://example.invalid/mcp",
                "env_http_headers": {
                    "X-Api-Key": "REMOTE_MCP_KEY"
                },
                "bearer_token_env_var": "REMOTE_MCP_TOKEN",
                "scopes": ["tools/read", "tools/write"],
                "oauth": {
                    "client_id": "client-123"
                },
                "oauth_resource": "https://example.invalid"
            }
        }
    }"#;
    let cfg: McpConfig = serde_json::from_str(json).unwrap();
    let remote = cfg.servers.get("remote").expect("server present");
    assert_eq!(
        remote.env_headers.get("X-Api-Key"),
        Some(&"REMOTE_MCP_KEY".to_string())
    );
    assert_eq!(
        remote.bearer_token_env_var.as_deref(),
        Some("REMOTE_MCP_TOKEN")
    );
    assert_eq!(remote.scopes, vec!["tools/read", "tools/write"]);
    assert_eq!(remote.oauth_client_id(), Some("client-123"));
    assert_eq!(
        remote.oauth_resource.as_deref(),
        Some("https://example.invalid")
    );
}

#[test]
fn mcp_server_config_omits_headers_when_empty() {
    // Empty headers map should not appear in the serialized output —
    // older mcp.json files written before v0.8.31 must round-trip
    // unchanged so a `mcp save` from a fresh install doesn't add
    // dead keys.
    let cfg = McpServerConfig {
        command: Some("node".into()),
        args: vec!["server.js".into()],
        env: HashMap::new(),
        cwd: None,
        url: None,
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
        headers: HashMap::new(),
        env_headers: HashMap::new(),
        bearer_token_env_var: None,
        scopes: Vec::new(),
        oauth: None,
        oauth_resource: None,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    };
    let serialized = serde_json::to_string(&cfg).unwrap();
    assert!(
        !serialized.contains("\"headers\""),
        "empty headers must be omitted: {serialized}"
    );
    assert!(
        !serialized.contains("\"env_headers\""),
        "empty env_headers must be omitted: {serialized}"
    );
    assert!(
        !serialized.contains("\"scopes\""),
        "empty scopes must be omitted: {serialized}"
    );
    assert!(
        !serialized.contains("\"oauth\""),
        "empty oauth config must be omitted: {serialized}"
    );
}

#[test]
fn expand_env_placeholders_expands_value_from_environment() {
    let _lock = crate::test_support::lock_test_env();
    let _secret =
        crate::test_support::EnvVarGuard::set("MCP_TEST_SECRET_TOKEN", "test-secret-123456");
    let mut env = HashMap::new();
    env.insert(
        "API_TOKEN".to_string(),
        "${MCP_TEST_SECRET_TOKEN}".to_string(),
    );

    let expanded = expand_env_placeholders_map(&env, "env").unwrap();

    assert_eq!(
        expanded.get("API_TOKEN").map(String::as_str),
        Some("test-secret-123456")
    );
}

#[test]
fn expand_env_placeholders_reports_missing_variable_without_secret_value() {
    let _lock = crate::test_support::lock_test_env();
    let _missing = crate::test_support::EnvVarGuard::remove("MCP_TEST_MISSING_SECRET");

    let err = expand_env_placeholders("Bearer ${MCP_TEST_MISSING_SECRET}")
        .expect_err("missing env should fail")
        .to_string();

    // The error must name the variable but must not leak the surrounding
    // value (which in practice carries the secret).
    assert!(err.contains("MCP_TEST_MISSING_SECRET"));
    assert!(!err.contains("Bearer "));
}

#[test]
fn reviewed_plugin_environment_uses_only_the_pre_dotenv_snapshot() {
    let _lock = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugins/env-snapshot");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"env-snapshot\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let (_, authority) = active_plugin_fixture(&plugin_base);
    let snapshot = crate::plugins::HostEnvironment::from_entries([(
        OsString::from("PLUGIN_SNAPSHOT_TOKEN"),
        OsString::from("captured-before-dotenv"),
    )]);
    let mut server = test_server_config();
    server
        .env
        .insert("TOKEN".to_string(), "${PLUGIN_SNAPSHOT_TOKEN}".to_string());
    server.reviewed_plugin =
        Some(ReviewedPluginMcpSource::from_authority(authority, None, Arc::new(snapshot)).unwrap());
    let _late_dotenv = crate::test_support::EnvVarGuard::set(
        "PLUGIN_SNAPSHOT_TOKEN",
        "workspace-dotenv-must-not-win",
    );

    let expanded = expanded_mcp_stdio_env(&server).unwrap();
    assert_eq!(expanded["TOKEN"], "captured-before-dotenv");

    server.reviewed_plugin.as_mut().unwrap().host_environment =
        Arc::new(crate::plugins::HostEnvironment::from_entries([]));
    let error = expanded_mcp_stdio_env(&server)
        .expect_err("a value present only after dotenv must fail closed");
    assert!(
        format!("{error:#}").contains("PLUGIN_SNAPSHOT_TOKEN"),
        "unexpected missing-snapshot error: {error:#}"
    );
    assert!(!format!("{error:#}").contains("workspace-dotenv-must-not-win"));
}

fn write_path_only_test_command(dir: &Path) -> String {
    let command = "codewhale-mcp-path-only-test";
    #[cfg(windows)]
    let file_name = format!("{command}.exe");
    #[cfg(not(windows))]
    let file_name = command.to_string();
    let path = dir.join(file_name);
    fs::write(&path, b"test executable").expect("write path-only test command");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path)
            .expect("path-only command metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make path-only test command executable");
    }
    command.to_string()
}

#[test]
fn static_mcp_command_uses_expanded_sanitized_stdio_path() {
    let _lock = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let command = write_path_only_test_command(temp.path());
    let _path = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_MCP_PATH_ONLY_DIR",
        temp.path().as_os_str(),
    );
    let _secret = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_MCP_STATIC_TEST_SECRET",
        "must-not-reach-child",
    );
    let mut server = test_server_config();
    server.command = Some(command);
    server.env.insert(
        "PATH".to_string(),
        "${CODEWHALE_MCP_PATH_ONLY_DIR}".to_string(),
    );

    assert_eq!(
        static_mcp_command_availability(&server).expect("static command check"),
        McpCommandAvailability::Available
    );

    let child_env = mcp_stdio_child_env(&server).expect("stdio child env");
    assert_eq!(
        env_value(&child_env, "PATH"),
        Some(temp.path().as_os_str()),
        "expanded server PATH must override the inherited PATH"
    );
    assert!(
        child_env
            .iter()
            .all(|(key, _)| key != "CODEWHALE_MCP_STATIC_TEST_SECRET"),
        "static lookup must use the same sanitized parent environment as spawn"
    );

    let expanded_env = expand_env_placeholders_map(&server.env, "env").expect("expanded env");
    let mut old_spawn_command = tokio::process::Command::new("unused-test-command");
    crate::child_env::apply_to_tokio_command_mcp(
        &mut old_spawn_command,
        crate::child_env::string_map_env(&expanded_env),
    );
    let old_spawn_env = old_spawn_command
        .as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_os_string(),
                value.expect("spawn env value").to_os_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    let static_env = child_env.into_iter().collect::<HashMap<_, _>>();
    assert_eq!(
        static_env, old_spawn_env,
        "static lookup and the pre-fix spawn helper must receive identical environments"
    );
}

#[cfg(not(windows))]
#[test]
fn static_mcp_command_reports_missing_with_server_path_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut server = test_server_config();
    server.command = Some("codewhale-mcp-command-that-does-not-exist".to_string());
    server.env.insert(
        "PATH".to_string(),
        temp.path().to_string_lossy().into_owned(),
    );

    assert_eq!(
        static_mcp_command_availability(&server).expect("static command check"),
        McpCommandAvailability::Missing
    );
}

#[test]
fn static_mcp_command_reports_invalid_path_expansion() {
    let _lock = crate::test_support::lock_test_env();
    let _missing = crate::test_support::EnvVarGuard::remove("CODEWHALE_MCP_MISSING_PATH_DIR");
    let mut server = test_server_config();
    server.command = Some("codewhale-mcp-command".to_string());
    server.env.insert(
        "PATH".to_string(),
        "do-not-leak-${CODEWHALE_MCP_MISSING_PATH_DIR}-also-secret".to_string(),
    );

    let error = static_mcp_command_availability(&server)
        .expect_err("missing PATH placeholder must fail static validation");
    let error = format!("{error:#}");
    assert!(error.contains("CODEWHALE_MCP_MISSING_PATH_DIR"));
    assert!(!error.contains("codewhale-mcp-command"));
    assert!(!error.contains("do-not-leak"));
    assert!(!error.contains("also-secret"));
}

#[cfg(unix)]
fn write_unix_test_command(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write Unix test command");
    let mut permissions = fs::metadata(path)
        .expect("Unix test command metadata")
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).expect("set Unix test command mode");
}

#[cfg(unix)]
#[test]
fn static_mcp_command_anchors_relative_and_empty_path_to_server_cwd() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().join("server-cwd");
    let bin = cwd.join("relative-bin");
    fs::create_dir_all(&bin).expect("relative bin dir");
    let relative_command = "codewhale-mcp-relative-path-test";
    write_unix_test_command(&bin.join(relative_command), 0o755);

    let mut server = test_server_config();
    server.command = Some(relative_command.to_string());
    server.cwd = Some(cwd.clone());
    server
        .env
        .insert("PATH".to_string(), "relative-bin".to_string());
    assert_eq!(
        static_mcp_command_availability(&server).expect("relative PATH check"),
        McpCommandAvailability::Available
    );

    let empty_path_command = "codewhale-mcp-empty-path-test";
    write_unix_test_command(&cwd.join(empty_path_command), 0o755);
    server.command = Some(empty_path_command.to_string());
    server.env.insert("PATH".to_string(), String::new());
    assert_eq!(
        static_mcp_command_availability(&server).expect("empty PATH check"),
        McpCommandAvailability::Available,
        "an empty Unix PATH entry resolves from the child's cwd"
    );
}

#[cfg(unix)]
#[test]
fn static_mcp_command_preserves_literal_name_and_requires_execute_bits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let literal_command = " codewhale-mcp-literal-command-test ";
    write_unix_test_command(&temp.path().join(literal_command), 0o755);

    let mut server = test_server_config();
    server.command = Some(literal_command.to_string());
    server.env.insert(
        "PATH".to_string(),
        temp.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        static_mcp_command_availability(&server).expect("literal command check"),
        McpCommandAvailability::Available,
        "static validation must not trim the command passed to Command::new"
    );

    let non_executable = temp.path().join("codewhale-mcp-non-executable-test");
    write_unix_test_command(&non_executable, 0o644);
    server.command = Some("codewhale-mcp-non-executable-test".to_string());
    assert_eq!(
        static_mcp_command_availability(&server).expect("PATH execute-bit check"),
        McpCommandAvailability::Missing
    );
    server.command = Some(non_executable.to_string_lossy().into_owned());
    assert_eq!(
        static_mcp_command_availability(&server).expect("absolute execute-bit check"),
        McpCommandAvailability::Missing
    );
}

#[cfg(windows)]
#[test]
fn static_mcp_command_matches_windows_path_and_extension_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    let command = write_path_only_test_command(temp.path());
    let mut server = test_server_config();
    server.command = Some(command);
    server.env.insert(
        "Path".to_string(),
        temp.path().to_string_lossy().into_owned(),
    );

    assert_eq!(
        static_mcp_command_availability(&server).expect("case-insensitive PATH check"),
        McpCommandAvailability::Available
    );

    server.command = Some(
        temp.path()
            .join("codewhale-mcp-path-only-test")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        static_mcp_command_availability(&server).expect("absolute omitted .exe check"),
        McpCommandAvailability::Available
    );

    let pathext_command = "codewhale-mcp-pathext-only-test";
    fs::write(
        temp.path().join(format!("{pathext_command}.cmd")),
        b"@exit /b 0\r\n",
    )
    .expect("write PATHEXT-only command");
    server.command = Some(pathext_command.to_string());
    server.env.insert("PATHEXT".to_string(), ".CMD".to_string());
    assert_eq!(
        static_mcp_command_availability(&server).expect("PATHEXT command check"),
        McpCommandAvailability::NotChecked,
        "a child-PATH miss is conservative because Windows still searches implicit fallbacks"
    );
    server.command = Some(format!("{pathext_command}.cmd"));
    assert_eq!(
        static_mcp_command_availability(&server).expect("explicit .cmd command check"),
        McpCommandAvailability::Available,
        "Rust requires non-.exe extensions to be explicit"
    );
}

#[tokio::test]
async fn mcp_http_auth_prefers_static_authorization_over_bearer_env() {
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), "Bearer static".to_string());
    let auth = McpHttpAuth {
        headers,
        bearer_token_env_var: Some("PATH".to_string()),
        ..Default::default()
    };

    let resolved = auth.resolved_headers().await.unwrap();
    assert_eq!(
        resolved.get("Authorization"),
        Some(&"Bearer static".to_string())
    );
}

#[tokio::test]
async fn mcp_http_auth_uses_bearer_env_when_no_authorization_header() {
    let auth = McpHttpAuth {
        bearer_token_env_var: Some("PATH".to_string()),
        ..Default::default()
    };

    let resolved = auth.resolved_headers().await.unwrap();
    assert!(
        resolved
            .get("Authorization")
            .is_some_and(|value| value.starts_with("Bearer ") && value.len() > "Bearer ".len()),
        "expected PATH-backed bearer header, got {resolved:?}"
    );
}

#[test]
fn is_safe_custom_header_accepts_normal_auth_pairs() {
    assert!(is_safe_custom_header("Authorization", "Bearer tok"));
    assert!(is_safe_custom_header("X-Api-Key", "deadbeef"));
    assert!(is_safe_custom_header("x-org", "anthropic"));
}

#[test]
fn is_safe_custom_header_rejects_empty_or_whitespace_key() {
    assert!(!is_safe_custom_header("", "value"));
    assert!(!is_safe_custom_header("   ", "value"));
}

#[test]
fn is_safe_custom_header_rejects_response_splitting_values() {
    assert!(
        !is_safe_custom_header("X-Foo", "abc\r\nSet-Cookie: evil=1"),
        "CRLF in value must reject — response-splitting defense"
    );
    assert!(
        !is_safe_custom_header("X-Foo", "abc\nbar"),
        "bare LF in value must reject"
    );
    assert!(
        !is_safe_custom_header("X-Foo", "abc\rbar"),
        "bare CR in value must reject"
    );
}

#[test]
fn is_safe_custom_header_rejects_protocol_framing_overrides() {
    // The MCP Streamable HTTP transport relies on its own
    // Accept / Content-Type values for protocol negotiation;
    // a stray user override would silently break tool discovery.
    assert!(!is_safe_custom_header("Accept", "text/plain"));
    assert!(!is_safe_custom_header("accept", "text/plain"));
    assert!(!is_safe_custom_header("Content-Type", "text/plain"));
    assert!(!is_safe_custom_header("CONTENT-TYPE", "x/y"));
}

#[test]
fn default_mcp_http_get_accepts_json_and_event_stream() {
    let client = test_http_client();
    let request = with_default_mcp_http_headers(client.get("https://example.invalid/mcp"), false)
        .build()
        .unwrap();
    assert_eq!(
        request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()),
        Some(MCP_HTTP_ACCEPT)
    );
    assert!(
        request.headers().get(CONTENT_TYPE).is_none(),
        "SSE GET requests should not advertise a JSON request body"
    );
}

#[test]
fn default_mcp_http_post_accepts_json_and_event_stream() {
    let client = test_http_client();
    let request = with_default_mcp_http_headers(client.post("https://example.invalid/mcp"), true)
        .build()
        .unwrap();
    assert_eq!(
        request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()),
        Some(MCP_HTTP_ACCEPT)
    );
    assert_eq!(
        request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

#[tokio::test]
async fn streamable_http_transport_prepares_configured_headers() {
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), "Bearer xyz".to_string());
    let url = "https://example.invalid/mcp";
    let transport = StreamableHttpTransport::new(
        test_mcp_http_client(url).with_mcp_auth(McpHttpAuth {
            headers,
            ..Default::default()
        }),
        url.to_string(),
    );
    let request = transport
        .client
        .prepare_mcp_request(transport.client.post(&transport.url), true)
        .await
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        request.headers().get("Authorization").unwrap(),
        "Bearer xyz"
    );
    assert_eq!(request.headers().get(ACCEPT).unwrap(), MCP_HTTP_ACCEPT);
    assert_eq!(
        request.headers().get(CONTENT_TYPE).unwrap(),
        "application/json"
    );
}

#[test]
fn mcp_auth_required_error_item_is_model_visible() {
    let pool = McpPool::new(McpConfig::default());
    let item = pool.mcp_auth_required_error_item("nordic-mcp");
    assert_eq!(item["error"], "authentication_required");
    assert_eq!(item["server"], "nordic-mcp");
    assert!(
        item.get("authenticate_tool").is_none(),
        "an OAuth-servable synthetic tool is only named for a configured needs-auth server: {item}"
    );
    assert!(
        item["message"]
            .as_str()
            .expect("message")
            .contains("codewhale mcp login nordic-mcp")
    );
}

#[test]
fn test_mcp_config_parse_mcp_servers_alias_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
          "mcpServers": {
            "disabled": {
              "command": "node",
              "args": ["server.js"],
              "disabled": true
            }
          }
        }"#,
    )
    .unwrap();

    let cfg = load_config(&path).unwrap();
    assert!(cfg.servers.contains_key("disabled"));
    let snapshot = manager_snapshot_from_config(&path, true).unwrap();
    assert!(snapshot.reload_required);
    assert_eq!(snapshot.servers[0].name, "disabled");
    assert!(!snapshot.servers[0].enabled);
    assert_eq!(snapshot.servers[0].error.as_deref(), Some("disabled"));
    assert_eq!(
        snapshot.servers[0].capability_metadata,
        McpServerCapabilityMetadata::NotObserved
    );
}

#[test]
fn malformed_mcp_config_error_omits_secret_contents_and_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    let secret = "cw-secret-mcp-config-4507";
    fs::write(
        &path,
        format!(
            r#"{{"servers":{{"private":{{"headers":{{"Authorization":"{secret}"}} trailing-junk}}}}}}"#
        ),
    )
    .unwrap();

    let error = load_config(&path).expect_err("malformed MCP config must fail");
    let diagnostic = format!("{error:#}");
    assert!(!diagnostic.contains(secret), "{diagnostic}");
    assert!(!diagnostic.contains("Authorization"), "{diagnostic}");
    assert!(
        diagnostic.contains("file contents were omitted"),
        "{diagnostic}"
    );
}

#[test]
fn workspace_mcp_config_merges_with_project_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(
        &global_path,
        r#"{
          "servers": {
            "global": {"command": "node", "args": ["global.js"]},
            "shared": {"command": "node", "args": ["global-shared.js"]}
          }
        }"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{
          "servers": {
            "project": {"command": "php", "args": ["artisan", "boost:mcp"]},
            "shared": {"command": "php", "args": ["artisan", "shared:mcp"]}
          }
        }"#,
    )
    .unwrap();

    let cfg = load_config_with_workspace(&global_path, &workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();

    assert!(cfg.servers.contains_key("global"));
    let project = cfg.servers.get("project").unwrap();
    assert_eq!(project.command.as_deref(), Some("php"));
    assert_eq!(project.cwd.as_deref(), Some(workspace.as_path()));
    let shared = cfg.servers.get("shared").unwrap();
    assert_eq!(shared.args, vec!["artisan", "shared:mcp"]);
    assert_eq!(shared.cwd.as_deref(), Some(workspace.as_path()));
}

#[test]
fn workspace_manager_snapshot_counts_global_and_project_servers() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(
        &global_path,
        r#"{
          "servers": {
            "chrome-devtools": {"command": "npx", "args": ["-y", "chrome-devtools-mcp@latest"]},
            "context7": {"command": "npx", "args": ["-y", "@upstash/context7-mcp@latest"]}
          }
        }"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{
          "servers": {
            "laravel-boost": {"command": "php", "args": ["artisan", "boost:mcp"]}
          }
        }"#,
    )
    .unwrap();

    let plain = manager_snapshot_from_config(&global_path, false).unwrap();
    let merged =
        manager_snapshot_from_config_with_workspace(&global_path, &workspace, false).unwrap();

    assert_eq!(plain.servers.len(), 2);
    assert_eq!(merged.servers.len(), 3);
    assert!(
        merged
            .servers
            .iter()
            .any(|server| server.name == "laravel-boost"),
        "workspace-aware snapshots must include trusted project MCP servers"
    );
}

#[test]
fn plugin_mcp_servers_are_qualified_and_resolve_relative_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugins").join("fleet");
    fs::create_dir_all(plugin_base.join("servers/local")).unwrap();
    fs::write(plugin_base.join("servers/local/server.js"), "// server\n").unwrap();

    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "fleet"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["server.js"]
cwd = "servers/local"

[mcp_servers.remote]
url = "https://example.invalid/mcp"

[capabilities]
network_hosts = ["example.invalid"]
"#,
    )
    .unwrap();
    let (plugin, authority) = active_plugin_fixture(&plugin_base);
    let plugin_for_collision = plugin.clone();
    let authority_for_collision = authority.clone();
    let mut config = McpConfig::default();
    config.servers.insert(
        "global".to_string(),
        serde_json::from_str(r#"{"command":"node","args":["global.js"]}"#).unwrap(),
    );

    let cfg = merge_plugin_mcp_servers_from_plugins(
        config,
        vec![("fleet".to_string(), plugin, authority)],
    )
    .unwrap();

    assert!(cfg.servers.contains_key("global"));

    let local = cfg.servers.get("plugin-5-fleet-local").unwrap();
    assert_eq!(local.command.as_deref(), Some("node"));
    let staged_root = plugin_for_collision.staged_root.as_deref().unwrap();
    assert_eq!(
        local.args,
        vec![
            staged_root
                .join("servers/local/server.js")
                .display()
                .to_string()
        ]
    );
    assert_eq!(
        local.cwd.as_deref(),
        Some(staged_root.join("servers/local").as_path())
    );

    let remote = cfg.servers.get("plugin-5-fleet-remote").unwrap();
    assert_eq!(remote.url.as_deref(), Some("https://example.invalid/mcp"));
    assert!(remote.cwd.is_none());

    let mut explicit = McpConfig::default();
    explicit.servers.insert(
        "plugin-5-fleet-local".to_string(),
        serde_json::from_str(r#"{"command":"node","args":["explicit.js"]}"#).unwrap(),
    );
    let collision_safe = merge_plugin_mcp_servers_from_plugins(
        explicit,
        vec![(
            "fleet".to_string(),
            plugin_for_collision,
            authority_for_collision,
        )],
    )
    .unwrap();
    assert_eq!(
        collision_safe.servers["plugin-5-fleet-local"].args,
        vec!["explicit.js"],
        "explicit MCP config must outrank a colliding plugin server"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn reviewed_node_mjs_plugin_connects_through_inherited_descriptor() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping reviewed Node ESM launch test because node is unavailable");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("node-esm");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("server.mjs"),
        r#"import readline from 'node:readline';
const lines = readline.createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result;
  if (request.method === 'initialize') {
    result = {
      protocolVersion: '2025-06-18',
      capabilities: { tools: {} },
      serverInfo: { name: 'node-esm', version: '1.0.0' }
    };
  } else if (request.method === 'tools/list') {
    result = {
      tools: [{ name: 'ready', description: 'ready', inputSchema: { type: 'object' } }]
    };
  } else {
    result = {};
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#,
    )
    .unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "node-esm"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["server.mjs"]
connect_timeout = 2
"#,
    )
    .unwrap();

    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("node-esm").unwrap();
    registry.enable("node-esm").unwrap();
    let active = registry.active_plugins()[0].clone();
    let authority = registry.authority_for("node-esm").unwrap();
    let merged = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("node-esm".to_string(), active, authority)],
    )
    .unwrap();
    let mut pool = McpPool::new(merged);

    let connection = pool
        .get_or_connect("plugin-8-node-esm-local")
        .await
        .unwrap();
    assert_eq!(connection.tools().len(), 1);
    assert_eq!(connection.tools()[0].name, "ready");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn reviewed_node_plugins_preserve_module_path_context() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping reviewed multi-file Node ESM launch test because node is unavailable");
        return;
    }

    for (extension, package_type) in [
        ("mjs", "module"),
        ("js", "module"),
        ("js", "commonjs"),
        ("cjs", "module"),
    ] {
        let esm = extension != "cjs" && package_type == "module";
        // The entry imports a sibling module, exactly like the computer-use
        // bundle (#5916). Launched by descriptor, Node would resolve `./lib/...`
        // against `/dev/` and the child would die before the handshake.
        let dir = tempfile::tempdir().unwrap();
        let plugins_root = dir.path().join("plugins");
        let plugin_base = plugins_root.join("node-esm-multi");
        fs::create_dir_all(plugin_base.join("mcp")).unwrap();
        fs::create_dir_all(plugin_base.join("lib")).unwrap();
        fs::write(
            plugin_base.join("package.json"),
            format!(r#"{{"type":"{package_type}"}}"#),
        )
        .unwrap();
        fs::write(plugin_base.join("mcp/reply.json"), r#"{"answer":42}"#).unwrap();
        fs::write(
            plugin_base.join("lib").join(format!("reply.{extension}")),
            if esm {
                r#"import path from 'node:path';
import url from 'node:url';
export const TOOL = 'ready-from-sibling';
export const ENTRY_DIR = path.basename(path.dirname(url.fileURLToPath(import.meta.url)));
"#
            } else {
                r#"const path = require('node:path');
exports.TOOL = 'ready-from-sibling';
exports.ENTRY_DIR = path.basename(__dirname);
"#
            },
        )
        .unwrap();
        let imports = if esm {
            format!(
                "import readline from 'node:readline';\nimport fs from 'node:fs';\nimport {{ TOOL, ENTRY_DIR }} from '../lib/reply.{extension}';\n"
            )
        } else {
            format!(
                "const readline = require('node:readline');\nconst fs = require('node:fs');\nconst {{ TOOL, ENTRY_DIR }} = require('../lib/reply.{extension}');\n"
            )
        };
        fs::write(
            plugin_base.join("mcp").join(format!("server.{extension}")),
            imports
                + r#"const lines = readline.createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result;
  if (request.method === 'initialize') {
    result = {
      protocolVersion: '2025-06-18',
      capabilities: { tools: {} },
      serverInfo: { name: 'node-esm-multi', version: '1.0.0' }
    };
  } else if (request.method === 'tools/list') {
    result = {
      tools: [{ name: TOOL, description: ENTRY_DIR, inputSchema: { type: 'object' } }]
    };
  } else if (request.method === 'tools/call') {
    result = { content: [{ type: 'text', text: JSON.stringify({
      answer: JSON.parse(fs.readFileSync('reply.json', 'utf8')).answer,
      args: process.argv.slice(2)
    }) }] };
  } else {
    result = {};
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#,
        )
        .unwrap();
        fs::write(
            plugin_base.join("plugin.toml"),
            format!(
                r#"
schema_version = 1
[plugin]
name = "node-esm-multi"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["--no-warnings", "server.{extension}", "fixture-argument"]
cwd = "mcp"
connect_timeout = 2
"#
            ),
        )
        .unwrap();

        let discovery = crate::plugins::discovery::DiscoveryConfig {
            workspace: dir.path().join("project"),
            user_plugins_dir: plugins_root,
            workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
            builtin_plugin_dirs: Vec::new(),
            state_path: dir.path().join("plugin-state/state.json"),
        };
        let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
        registry.trust("node-esm-multi").unwrap();
        registry.enable("node-esm-multi").unwrap();
        let active = registry.active_plugins()[0].clone();
        let authority = registry.authority_for("node-esm-multi").unwrap();
        let merged = merge_plugin_mcp_servers_from_plugins(
            McpConfig::default(),
            vec![("node-esm-multi".to_string(), active, authority)],
        )
        .unwrap();
        let mut pool = McpPool::new(merged);

        let connection = pool
            .get_or_connect("plugin-14-node-esm-multi-local")
            .await
            .unwrap();
        assert_eq!(connection.tools().len(), 1);
        assert_eq!(connection.tools()[0].name, "ready-from-sibling");
        // The sibling resolved from the staged tree, not from `/dev/`.
        assert_eq!(connection.tools()[0].description.as_deref(), Some("lib"));
        let result = pool
            .call_tool(
                "mcp_plugin-14-node-esm-multi-local_ready-from-sibling",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert_eq!(
            result["content"][0]["text"], r#"{"answer":42,"args":["fixture-argument"]}"#,
            "{extension}/{package_type} must preserve staged cwd resources and script arguments"
        );
        registry.disable("node-esm-multi").unwrap();
        assert!(pool.all_tools().is_empty());
        assert!(
            pool.call_tool(
                "mcp_plugin-14-node-esm-multi-local_ready-from-sibling",
                serde_json::json!({})
            )
            .await
            .is_err(),
            "disabled Node tools must not remain callable"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn node_entry_preserves_package_type_and_sibling_context() {
    use std::collections::BTreeMap;
    let staged_root = Path::new("/stage/plugin");
    let entry = staged_root.join("mcp/server.mjs");
    let hash = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| (PathBuf::from(path), "h".to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    // Even a single .js/.cjs depends on filename/package-type semantics.
    for extension in ["js", "cjs"] {
        let relative = format!("mcp/server.{extension}");
        assert!(node_entry_needs_staged_path(
            staged_root,
            &staged_root.join(&relative),
            &hash(&[&relative, "package.json"]),
        ));
    }
    // Manifests, docs, and data files are not modules.
    assert!(!node_entry_needs_staged_path(
        staged_root,
        &entry,
        &hash(&[
            "mcp/server.mjs",
            "plugin.json",
            "mcp.json",
            "README.md",
            "skills/a/SKILL.md"
        ]),
    ));
    for sibling in [
        "src/tools.mjs",
        "lib/x.js",
        "lib/x.cjs",
        "native/x.node",
        "wasm/x.wasm",
    ] {
        assert!(
            node_entry_needs_staged_path(
                staged_root,
                &entry,
                &hash(&["mcp/server.mjs", "plugin.json", sibling]),
            ),
            "{sibling} must force a path launch"
        );
    }
    // An entry outside the stage never qualifies.
    assert!(!node_entry_needs_staged_path(
        Path::new("/elsewhere"),
        &entry,
        &hash(&["mcp/server.mjs", "src/tools.mjs"]),
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn node_esm_descriptor_launch_keeps_options_argv_shape_and_script_arguments() {
    use std::ffi::OsString;
    let os = |value: &str| OsString::from(value);

    // Options before the entrypoint stay in front; the entry is imported by
    // descriptor once, an empty main is supplied, and the descriptor path is
    // echoed after `--` so `process.argv[1]` keeps the file-mode shape.
    let args = vec![
        os("--max-old-space-size=256"),
        os("/dev/fd/7"),
        os("--port"),
        os("0"),
    ];
    assert_eq!(
        super::node_esm_descriptor_args(&args, 1),
        vec![
            os("--max-old-space-size=256"),
            os("--import"),
            os("/dev/fd/7"),
            os("-e"),
            os(""),
            os("--"),
            os("/dev/fd/7"),
            os("--port"),
            os("0"),
        ]
    );

    // A `.mjs` that is not the first positional argument is not the
    // entrypoint; the launch is left untouched.
    let args = vec![os("other.js"), os("/dev/fd/7")];
    assert_eq!(super::node_esm_descriptor_args(&args, 1), args);

    // The original option terminator cannot precede the injected --import.
    let args = vec![
        os("--no-warnings"),
        os("--"),
        os("/dev/fd/7"),
        os("argument"),
    ];
    assert_eq!(
        super::node_esm_descriptor_args(&args, 2),
        vec![
            os("--no-warnings"),
            os("--import"),
            os("/dev/fd/7"),
            os("-e"),
            os(""),
            os("--"),
            os("/dev/fd/7"),
            os("argument")
        ]
    );
    let args = vec![os("--conditions"), os("fixture"), os("/dev/fd/7")];
    let rewritten = super::node_esm_descriptor_args(&args, 2);
    assert_eq!(
        &rewritten[..4],
        &[
            os("--conditions"),
            os("fixture"),
            os("--import"),
            os("/dev/fd/7")
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn reviewed_node_launch_identifies_only_the_script_operand() {
    for (args, expected) in [
        (vec!["/stage/server.js", "/stage/later.mjs"], Some(0)),
        (vec!["other.js", "/stage/later.mjs"], Some(0)),
        (
            vec!["--require", "/stage/preload.cjs", "/stage/server.js"],
            Some(2),
        ),
        (
            vec!["--import", "/stage/preload.mjs", "/stage/server.mjs"],
            Some(2),
        ),
        (vec!["--require"], None),
        (vec!["--", "/stage/server.cjs"], Some(1)),
        (vec!["--"], None),
        (
            vec!["--max-old-space-size=256", "/stage/server.mjs"],
            Some(1),
        ),
        (
            vec!["--max-old-space-size", "256", "/stage/server.mjs"],
            Some(2),
        ),
        (
            vec![
                "--abort-on-uncaught-exception",
                "--expose-gc",
                "--jitless",
                "/stage/server.mjs",
            ],
            Some(3),
        ),
        (vec!["-e", "console.log('x')", "/stage/later.mjs"], None),
        (vec!["--eval=console.log('x')", "/stage/later.mjs"], None),
        (vec!["--run=task", "--", "/stage/later.js"], None),
        (vec!["--input-type=module", "/stage/server.mjs"], None),
        (vec!["--input-type", "module", "/stage/server.mjs"], None),
        (vec!["--unknown-option", "/stage/value.js"], None),
        (vec!["-", "/stage/later.mjs"], None),
    ] {
        assert_eq!(node_script_entry_index(&args), expected, "{args:?}");
    }
}

#[test]
fn plugin_server_ids_are_unambiguous_across_hyphenated_plugin_and_server_names() {
    let left = qualified_plugin_server_name("foo-bar", "baz");
    let right = qualified_plugin_server_name("foo", "bar-baz");

    assert_eq!(left, "plugin-7-foo-bar-baz");
    assert_eq!(right, "plugin-3-foo-bar-baz");
    assert_ne!(left, right);
}

#[test]
fn mixed_components_contribute_only_mcp_servers_to_the_mcp_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugin");
    fs::create_dir_all(plugin_base.join("commands")).unwrap();
    fs::create_dir_all(plugin_base.join("hooks")).unwrap();
    fs::write(plugin_base.join("server.js"), "// reviewed entrypoint\n").unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "fleet"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["server.js"]

[mcp_servers.remote]
url = "https://example.invalid/mcp"

[capabilities]
network_hosts = ["example.invalid"]

[commands]
path = "commands"

[hooks]
path = "hooks"
"#,
    )
    .unwrap();
    let (plugin, authority) = active_plugin_fixture(&plugin_base);
    assert!(plugin.active());

    let cfg = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("fleet".to_string(), plugin.clone(), authority)],
    )
    .unwrap();
    assert!(cfg.servers.contains_key("plugin-5-fleet-local"));
    assert!(cfg.servers.contains_key("plugin-5-fleet-remote"));
    assert_eq!(
        cfg.servers.len(),
        2,
        "declarative components must not become MCP servers: {:?}",
        cfg.servers.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        cfg.servers["plugin-5-fleet-local"].command.as_deref(),
        Some("node")
    );
    assert_eq!(
        cfg.servers["plugin-5-fleet-remote"].url.as_deref(),
        Some("https://example.invalid/mcp")
    );
}

#[test]
fn plugin_mcp_adapter_denies_disabled_and_untrusted_bundles() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugin");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "denied"
version = "1.0.0"

[mcp_servers.local]
command = "node"
"#,
    )
    .unwrap();
    let (mut disabled, authority) = active_plugin_fixture(&plugin_base);
    disabled.enabled = false;
    let mut untrusted = disabled.clone();
    untrusted.enabled = true;
    untrusted.trust_status = crate::plugins::types::PluginTrustStatus::NeverReviewed;

    for plugin in [disabled, untrusted] {
        let config = merge_plugin_mcp_servers_from_plugins(
            McpConfig::default(),
            vec![("denied".to_string(), plugin, authority.clone())],
        )
        .unwrap();
        assert!(
            config.servers.is_empty(),
            "headless MCP adapter admitted an inactive bundle"
        );
    }
}

#[test]
fn plugin_mcp_adapter_denies_content_changed_after_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugin");
    fs::create_dir_all(&plugin_base).unwrap();
    let manifest_path = plugin_base.join("plugin.toml");
    fs::write(
        &manifest_path,
        r#"
schema_version = 1
[plugin]
name = "changed"
version = "1.0.0"

[mcp_servers.local]
command = "node"
"#,
    )
    .unwrap();
    let (plugin, authority) = active_plugin_fixture(&plugin_base);
    fs::write(plugin_base.join("late-change.txt"), "changed after review").unwrap();

    let config = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("changed".to_string(), plugin, authority)],
    )
    .unwrap();
    assert!(config.servers.is_empty());
}

fn registry_with_local_mcp(
    name: &str,
    base_path: PathBuf,
    workspace: &Path,
) -> crate::plugins::PluginRegistry {
    fs::write(
        base_path.join("plugin.toml"),
        format!(
            r#"
schema_version = 1
[plugin]
name = "{name}"
version = "1.0.0"

[mcp_servers.local]
command = "node"
args = ["server.js"]
"#,
        ),
    )
    .unwrap();
    let plugins_root = base_path.parent().expect("plugin parent").to_path_buf();
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: workspace.to_path_buf(),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: workspace.join(".codewhale/plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: workspace
            .join("plugin-state")
            .join(format!("plugin-state-{name}.json")),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust(name).unwrap();
    registry.enable(name).unwrap();
    registry
}

#[test]
fn plugin_mcp_servers_merge_without_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let plugin_base = dir.path().join("plugins").join("fixture");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();

    let plugins = registry_with_local_mcp("fixture", plugin_base.clone(), &workspace);
    let staged_root = plugins
        .get("fixture")
        .and_then(|plugin| plugin.staged_root.clone())
        .expect("trusted plugin should have an immutable runtime snapshot");
    let cfg = load_config_with_workspace_and_plugins(&global_path, &workspace, &plugins).unwrap();

    assert!(cfg.servers.contains_key("global"));
    let qualified_name = qualified_plugin_server_name("fixture", "local");
    let local = cfg
        .servers
        .get(&qualified_name)
        .expect("plugin MCP should merge without a project MCP config");
    assert_eq!(local.command.as_deref(), Some("node"));
    assert_eq!(local.cwd.as_deref(), Some(staged_root.as_path()));
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_mcp_lazy_spawn_denies_component_changed_after_pool_construction() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("guarded");
    fs::create_dir_all(&plugin_base).unwrap();
    let server_path = plugin_base.join("server.sh");
    fs::write(&server_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&server_path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&server_path, permissions).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "guarded"
version = "1.0.0"

[mcp_servers.local]
command = "sh"
args = ["server.sh"]
connect_timeout = 1
"#,
    )
    .unwrap();

    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("guarded").unwrap();
    registry.enable("guarded").unwrap();
    let active = registry.active_plugins()[0].clone();
    let authority = registry.authority_for("guarded").unwrap();
    let merged = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("guarded".to_string(), active, authority)],
    )
    .unwrap();
    assert!(
        merged.servers["plugin-7-guarded-local"]
            .reviewed_plugin
            .is_some(),
        "plugin provenance must survive through MCP pool construction"
    );
    let mut pool = McpPool::new(merged);

    // Adversarial mutation after trust, enablement, merge, and pool
    // construction. If the lazy child executes, it creates this marker before
    // closing stdio, so the regression proves denial happened pre-spawn.
    let executed_marker = plugin_base.join("executed.marker");
    fs::write(&server_path, "#!/bin/sh\n: > executed.marker\nexit 0\n").unwrap();

    let error = pool
        .get_or_connect("plugin-7-guarded-local")
        .await
        .err()
        .expect("changed reviewed component must be denied before spawn");
    let message = format!("{error:#}");
    assert!(
        message.contains("Refusing to use MCP server 'plugin-7-guarded-local'"),
        "unexpected pre-spawn denial: {message}"
    );
    assert!(message.contains("changed after review"));
    assert!(message.contains("/plugin reload"));
    assert!(
        !executed_marker.exists(),
        "mutated MCP component executed despite pre-spawn hash denial"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_mcp_inflight_call_is_cancelled_after_cross_process_revocation() {
    let _env_lock = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let call_marker = dir.path().join("call.marker");
    let _call_marker_env = crate::test_support::EnvVarGuard::set(
        "CODEWHALE_TEST_PLUGIN_CALL_MARKER",
        call_marker.as_os_str(),
    );
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("revoked");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::create_dir_all(dir.path().join("project")).unwrap();
    fs::write(
        plugin_base.join("server.sh"),
        r#"#!/bin/sh
trap 'exit 0' TERM INT
while IFS= read -r line; do
    case "$line" in
        *'"method":"notifications/initialized"'*)
            ;;
        *'"method":"initialize"'*)
            printf '%s\n' '{"jsonrpc":"2.0","id":"1","result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"revocation-test","version":"1.0.0"},"capabilities":{"tools":{}}}}'
            ;;
        *'"method":"tools/list"'*)
            printf '%s\n' '{"jsonrpc":"2.0","id":"2","result":{"tools":[{"name":"wait","description":"Wait until revoked","inputSchema":{"type":"object"}}]}}'
            ;;
        *'"method":"tools/call"'*)
            : > "$CALL_MARKER"
            while :; do sleep 1; done
            ;;
    esac
done
"#,
    )
    .unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        r#"
schema_version = 1
[plugin]
name = "revoked"
version = "1.0.0"

[mcp_servers.local]
command = "sh"
args = ["server.sh"]
connect_timeout = 2
execute_timeout = 30
read_timeout = 30

[mcp_servers.local.env]
CALL_MARKER = "${CODEWHALE_TEST_PLUGIN_CALL_MARKER}"
"#,
    )
    .unwrap();

    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("revoked").unwrap();
    registry.enable("revoked").unwrap();
    let active = registry.active_plugins()[0].clone();
    let authority = registry.authority_for("revoked").unwrap();
    let merged = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("revoked".to_string(), active, authority)],
    )
    .unwrap();
    let mut pool = McpPool::new(merged);
    pool.get_or_connect("plugin-7-revoked-local").await.unwrap();

    let call = tokio::spawn(async move {
        pool.call_tool("mcp_plugin-7-revoked-local_wait", serde_json::json!({}))
            .await
    });
    for _ in 0..100 {
        if call_marker.exists() {
            break;
        }
        if call.is_finished() {
            let early = call
                .await
                .expect("in-flight tool task panicked before reaching the server");
            panic!("in-flight tool call ended before reaching the server: {early:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        call_marker.exists(),
        "test server never observed the in-flight tool call"
    );

    let mut external = crate::plugins::discovery::discover_with_config(&discovery);
    external.revoke_trust("revoked").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("revocation watcher did not cancel the in-flight call")
        .unwrap();
    let error = result
        .expect_err("revoked in-flight call must not complete")
        .to_string();
    assert!(error.contains("cancelled after authority changed"));
    assert!(error.contains("disabled, revoked, or no longer matches"));
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_stdio_authority_cancellation_terminates_an_idle_child() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugins/idle-child");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"idle-child\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let (_, authority) = active_plugin_fixture(&plugin_base);
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec![
        "-c".to_string(),
        "trap 'exit 0' TERM INT; while :; do sleep 1; done".to_string(),
    ];
    config.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            None,
            Arc::new(crate::plugins::HostEnvironment::capture()),
        )
        .unwrap(),
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    let transport = StdioTransport::spawn(
        "idle-child",
        config.command.as_deref().unwrap(),
        &config,
        cancellation.clone(),
    )
    .unwrap();
    let child = transport.session.child_for_tests();
    assert!(child.lock().await.try_wait().unwrap().is_none());

    cancellation.cancel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if child.lock().await.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "authority cancellation left the plugin stdio child alive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn plugin_stdio_does_not_surface_reviewed_child_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugins/stderr-secret");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"stderr-secret\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let (_, authority) = active_plugin_fixture(&plugin_base);
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec![
        "-c".to_string(),
        "echo 'ARBITRARY_PLUGIN_CREDENTIAL' 1>&2; exit 1".to_string(),
    ];
    config.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            None,
            Arc::new(crate::plugins::HostEnvironment::capture()),
        )
        .unwrap(),
    );
    let mut transport = StdioTransport::spawn(
        "stderr-secret",
        config.command.as_deref().unwrap(),
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let error = transport
        .recv()
        .await
        .expect_err("reviewed child should have closed its transport")
        .to_string();
    assert!(error.contains("Stdio transport closed"));
    assert!(!error.contains("ARBITRARY_PLUGIN_CREDENTIAL"));
}

/// Shutdown reaches what the server started, not only the server: a
/// background grandchild (an `npx` wrapper's node, a shell job) must not
/// survive the transport.
#[cfg(unix)]
#[tokio::test]
async fn stdio_shutdown_also_terminates_the_servers_grandchildren() {
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec!["-c".to_string(), "sleep 300 & echo $!; wait".to_string()];
    let mut transport = StdioTransport::spawn(
        "grandparent",
        "sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let line = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("grandchild pid line")
        .unwrap();
    let grandchild: i32 = String::from_utf8(line).unwrap().trim().parse().unwrap();

    transport.shutdown().await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: signal 0 only probes whether the pid still exists.
        if unsafe { libc::kill(grandchild, 0) } != 0 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            // Do not leak the sleeper past a failing run.
            // SAFETY: plain kill(2) of the pid this test started.
            unsafe {
                libc::kill(grandchild, libc::SIGKILL);
            }
            panic!("the MCP server's grandchild outlived the transport");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// #6187: a crashed stdio child must stop reading as "ready" before any
/// call is in flight — `is_ready` probes the child, so the pool rebuilds
/// the connection on the next use instead of handing the dead transport
/// back.
#[cfg(unix)]
#[tokio::test]
async fn dead_stdio_child_stops_reading_ready_without_a_call_in_flight() {
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec!["-c".to_string(), "while :; do sleep 1; done".to_string()];
    let transport = StdioTransport::spawn(
        "idle",
        "sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let child = transport.session.child_for_tests();
    let connection = test_connection(Box::new(transport));

    // Alive child: the Ready state flag is the whole answer.
    assert!(
        connection.is_ready(),
        "a live stdio child must not be probed dead"
    );

    child.lock().await.start_kill().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if child.lock().await.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "killed stdio child was never reaped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        !connection.is_ready(),
        "a reaped stdio child must fail is_ready without a call in flight"
    );
}

#[tokio::test]
async fn revoked_plugin_mcp_denies_catalog_tool_resource_and_prompt_operations() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("catalog-guard");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::create_dir_all(dir.path().join("project")).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"catalog-guard\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("catalog-guard").unwrap();
    registry.enable("catalog-guard").unwrap();
    let authority = registry.authority_for("catalog-guard").unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut connection = test_connection(Box::new(ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::new(),
    }));
    let source = ReviewedPluginMcpSource::from_authority(
        authority,
        None,
        Arc::new(crate::plugins::HostEnvironment::capture()),
    )
    .unwrap();
    connection.config.reviewed_plugin = Some(source.clone());
    connection.tools.push(McpTool {
        name: "echo".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    });
    connection.resources.push(McpResource {
        uri: "memory://one".to_string(),
        name: "one".to_string(),
        description: None,
        mime_type: None,
    });
    connection.resource_templates.push(McpResourceTemplate {
        uri_template: "memory://{id}".to_string(),
        name: "memory".to_string(),
        description: None,
        mime_type: None,
    });
    connection.prompts.push(McpPrompt {
        name: "review".to_string(),
        description: None,
        arguments: Vec::new(),
    });
    let mut config = McpConfig::default();
    let mut server = test_server_config();
    server.reviewed_plugin = Some(source);
    config.servers.insert("guarded".to_string(), server);
    let mut pool = McpPool::new(config);
    pool.connections.insert("guarded".to_string(), connection);
    assert_eq!(pool.all_tools().len(), 1);
    assert_eq!(pool.all_resources().len(), 1);
    assert_eq!(pool.all_resource_templates().len(), 1);
    assert_eq!(pool.all_prompts().len(), 1);

    let mut external = crate::plugins::discovery::discover_with_config(&discovery);
    external.revoke_trust("catalog-guard").unwrap();
    assert!(pool.all_tools().is_empty());
    assert!(pool.all_resources().is_empty());
    assert!(pool.all_resource_templates().is_empty());
    assert!(pool.all_prompts().is_empty());

    let tool = pool
        .call_tool("mcp_guarded_echo", serde_json::json!({}))
        .await;
    let resource = pool.read_resource("guarded", "memory://one").await;
    let prompt = pool
        .get_prompt("guarded", "review", serde_json::json!({}))
        .await;
    let resource_catalog = pool
        .call_tool(
            "list_mcp_resources",
            serde_json::json!({"server": "guarded"}),
        )
        .await;
    let template_catalog = pool
        .call_tool(
            "list_mcp_resource_templates",
            serde_json::json!({"server": "guarded"}),
        )
        .await;
    for result in [tool, resource, prompt, resource_catalog, template_catalog] {
        let error = result
            .expect_err("revoked plugin MCP operation must fail closed")
            .to_string();
        assert!(error.contains("Refusing to use MCP server 'guarded'"));
    }
    assert!(
        sent.lock().unwrap().is_empty(),
        "revoked plugin MCP operation reached the transport"
    );
}

fn cached_reviewed_plugin_catalog_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, McpPool) {
    let dir = tempfile::tempdir().unwrap();
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("catalog-drift");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::create_dir_all(dir.path().join("project")).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"catalog-drift\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("catalog-drift").unwrap();
    registry.enable("catalog-drift").unwrap();
    let authority = registry.authority_for("catalog-drift").unwrap();
    let staged_manifest = authority.staged_manifest.clone();

    let mut connection = test_connection(Box::new(ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::new(),
    }));
    let source = ReviewedPluginMcpSource::from_authority(
        authority,
        None,
        Arc::new(crate::plugins::HostEnvironment::capture()),
    )
    .unwrap();
    connection.config.reviewed_plugin = Some(source.clone());
    connection.tools.push(McpTool {
        name: "echo".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    });
    connection.resources.push(McpResource {
        uri: "memory://one".to_string(),
        name: "one".to_string(),
        description: None,
        mime_type: None,
    });
    connection.resource_templates.push(McpResourceTemplate {
        uri_template: "memory://{id}".to_string(),
        name: "memory".to_string(),
        description: None,
        mime_type: None,
    });
    connection.prompts.push(McpPrompt {
        name: "review".to_string(),
        description: None,
        arguments: Vec::new(),
    });
    let mut config = McpConfig::default();
    let mut server = test_server_config();
    server.reviewed_plugin = Some(source);
    config.servers.insert("guarded".to_string(), server);
    let mut pool = McpPool::new(config);
    pool.connections.insert("guarded".to_string(), connection);
    assert_eq!(pool.all_tools().len(), 1);
    assert_eq!(pool.all_resources().len(), 1);
    assert_eq!(pool.all_resource_templates().len(), 1);
    assert_eq!(pool.all_prompts().len(), 1);

    (dir, plugin_base, staged_manifest, pool)
}

fn assert_reviewed_plugin_catalog_hidden(pool: &McpPool, boundary: &str) {
    assert!(pool.all_tools().is_empty());
    assert!(pool.all_resources().is_empty());
    assert!(pool.all_resource_templates().is_empty());
    assert!(pool.all_prompts().is_empty());
    assert!(
        pool.to_api_tools()
            .iter()
            .all(|tool| tool.name != "mcp_guarded_echo"),
        "{boundary} drift must remove cached reviewed tools from the model API catalog"
    );
    assert!(pool.parse_prefixed_name("mcp_guarded_echo").is_err());
}

#[test]
fn reviewed_plugin_source_drift_hides_every_cached_catalog_surface() {
    let (_dir, plugin_base, _staged_manifest, pool) = cached_reviewed_plugin_catalog_fixture();

    fs::write(plugin_base.join("unreviewed-companion.txt"), b"drift").unwrap();

    assert_reviewed_plugin_catalog_hidden(&pool, "source");
}

#[cfg(unix)]
#[test]
fn reviewed_plugin_stage_drift_hides_every_cached_catalog_surface() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, _plugin_base, staged_manifest, pool) = cached_reviewed_plugin_catalog_fixture();
    std::fs::set_permissions(&staged_manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&staged_manifest)
        .unwrap()
        .write_all(b"\n# test-only staged drift\n")
        .unwrap();

    assert_reviewed_plugin_catalog_hidden(&pool, "staged-tree");
}

#[tokio::test]
async fn reviewed_plugin_oauth_is_disabled_without_network_or_token_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_base = dir.path().join("plugins/oauth-disabled");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"oauth-disabled\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let (_, authority) = active_plugin_fixture(&plugin_base);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let mut server = test_server_config();
    server.command = None;
    server.url = Some(endpoint.clone());
    server.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            Some(&endpoint),
            Arc::new(crate::plugins::HostEnvironment::default()),
        )
        .unwrap(),
    );

    assert_eq!(
        oauth::auth_status_for_server("plugin-oauth", &server, None).await,
        oauth::McpAuthStatus::Unsupported
    );
    assert!(
        oauth::oauth_login_support(&server, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        oauth::McpOAuthRuntime::from_server_config(
            "plugin-oauth",
            &server,
            reqwest::header::HeaderMap::new(),
        )
        .await
        .unwrap()
        .is_none()
    );
    let login_error =
        oauth::perform_oauth_login_for_server("plugin-oauth", &server, None, None, None, None)
            .await
            .expect_err("plugin OAuth login must be disabled")
            .to_string();
    assert!(login_error.contains("disabled for plugin-contributed MCP servers"));
    let logout_error = oauth::delete_oauth_tokens_for_server("plugin-oauth", &server)
        .expect_err("plugin OAuth logout must not touch token storage")
        .to_string();
    assert!(logout_error.contains("storage is disabled"));

    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "plugin OAuth disabled paths must not probe the network"
    );
}

fn active_plugin_fixture(
    plugin_base: &Path,
) -> (
    crate::plugins::types::LoadedPlugin,
    crate::plugins::types::PluginAuthority,
) {
    let plugins_root = plugin_base.parent().expect("plugin parent").to_path_buf();
    // Callers use both `<temp>/plugins/<name>` and `<temp>/<name>` layouts.
    // Only peel the conventional `plugins` directory; otherwise using the
    // parent would place multiple parallel fixtures in the shared system temp
    // root and make their durable state files collide on Windows.
    let root = if plugins_root.file_name().and_then(|name| name.to_str()) == Some("plugins") {
        plugins_root.parent().unwrap_or(&plugins_root).to_path_buf()
    } else {
        plugins_root.clone()
    };
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: root.join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: root.join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: root.join("plugin-state").join(format!(
            "plugin-state-{}.json",
            plugin_base.file_name().unwrap().to_string_lossy()
        )),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    let name = registry
        .list()
        .first()
        .expect("discovered plugin")
        .name()
        .to_string();
    registry.trust(&name).unwrap();
    registry.enable(&name).unwrap();
    (
        registry.get(&name).unwrap().clone(),
        registry.authority_for(&name).unwrap(),
    )
}

#[test]
fn workspace_mcp_config_ignores_project_file_until_workspace_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    let plugin_base = dir.path().join("plugins").join("fixture");
    fs::create_dir_all(&project_dir).unwrap();
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    let plugins = registry_with_local_mcp("fixture", plugin_base, &workspace);
    let cfg = load_config_with_workspace_and_plugins(&global_path, &workspace, &plugins).unwrap();

    assert!(cfg.servers.contains_key("global"));
    assert!(!cfg.servers.contains_key("project"));
    assert!(
        cfg.servers
            .contains_key(&qualified_plugin_server_name("fixture", "local")),
        "user plugin MCP should not be gated by project workspace trust"
    );
}

#[test]
fn workspace_mcp_config_ignores_project_local_legacy_trust_marker() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    fs::create_dir_all(workspace.join(".deepseek")).unwrap();
    fs::write(workspace.join(".deepseek").join("trusted"), "").unwrap();
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    let cfg = load_config_with_workspace(&global_path, &workspace).unwrap();

    assert!(cfg.servers.contains_key("global"));
    assert!(!cfg.servers.contains_key("project"));
}

#[test]
fn workspace_mcp_config_ignores_invalid_untrusted_project_file() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();
    fs::write(project_dir.join("mcp.json"), "{ not json").unwrap();

    let cfg = load_config_with_workspace(&global_path, &workspace).unwrap();

    assert!(cfg.servers.is_empty());
}

#[test]
fn workspace_mcp_config_rejects_parent_components() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "node", "args": ["server.js"]}}}"#,
    )
    .unwrap();

    let workspace_with_parent = workspace.join("..").join("workspace");
    let err = load_config_with_workspace(&global_path, &workspace_with_parent)
        .expect_err("parent components in workspace should fail closed");

    assert!(
        format!("{err:#}").contains("workspace path cannot contain '..'"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn workspace_mcp_config_resolves_relative_cwd_from_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "node", "args": ["server.js"], "cwd": "tools/mcp"}}}"#,
    )
    .unwrap();

    let cfg = load_config_with_workspace(&global_path, &workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();

    let project = cfg.servers.get("project").unwrap();
    assert_eq!(
        project.cwd.as_deref(),
        Some(workspace.join("tools/mcp").as_path())
    );
}

#[test]
fn workspace_mcp_config_rejects_project_cwd_escape() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "node", "args": ["server.js"], "cwd": "../outside"}}}"#,
    )
    .unwrap();

    let err = load_config_with_workspace(&global_path, &workspace)
        .expect_err("project MCP cwd escape must be rejected");

    assert!(
        err.to_string()
            .contains("Project MCP server cwd must stay within workspace"),
        "unexpected error: {err}"
    );
}

#[cfg(unix)]
#[test]
fn workspace_mcp_config_rejects_symlinked_project_cwd_escape() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&project_dir).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("tools")).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "node", "args": ["server.js"], "cwd": "tools"}}}"#,
    )
    .unwrap();

    let err = load_config_with_workspace(&global_path, &workspace)
        .expect_err("project MCP symlink cwd escape must be rejected");

    assert!(
        err.to_string()
            .contains("Project MCP server cwd must stay within workspace"),
        "unexpected error: {err}"
    );
}

#[test]
fn workspace_mcp_config_rejects_workspace_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let bad_workspace = workspace.join("..").join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(&global_path, r#"{"servers": {}}"#).unwrap();

    let err = load_config_with_workspace(&global_path, &bad_workspace)
        .expect_err("workspace traversal should fail");
    assert!(
        format!("{err:#}").contains("workspace path cannot contain '..'"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn workspace_mcp_pool_reload_picks_up_project_config_creation() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&workspace).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path_with_workspace(&global_path, &workspace).unwrap();
    assert_eq!(pool.server_names(), vec!["global".to_string()]);

    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    assert!(pool.reload_if_config_changed().await.unwrap());
    let names: std::collections::BTreeSet<String> = pool.server_names().into_iter().collect();
    let expected: std::collections::BTreeSet<String> =
        ["global".to_string(), "project".to_string()]
            .into_iter()
            .collect();
    assert_eq!(names, expected);
}

#[tokio::test]
async fn workspace_mcp_pool_reload_picks_up_project_config_after_workspace_trust() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let trust_env = workspace_trust_config_guard(&workspace);
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path_with_workspace(&global_path, &workspace).unwrap();
    assert_eq!(pool.server_names(), vec!["global".to_string()]);

    write_workspace_trust_config(&trust_env.config_path, &workspace);

    assert!(pool.reload_if_config_changed().await.unwrap());
    let names: std::collections::BTreeSet<String> = pool.server_names().into_iter().collect();
    let expected: std::collections::BTreeSet<String> =
        ["global".to_string(), "project".to_string()]
            .into_iter()
            .collect();
    assert_eq!(names, expected);
}

#[tokio::test]
async fn workspace_mcp_pool_reload_drops_project_config_after_workspace_trust_removed() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let trust = mark_workspace_trusted(&workspace);
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("mcp.json"),
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path_with_workspace(&global_path, &workspace).unwrap();
    let names: std::collections::BTreeSet<String> = pool.server_names().into_iter().collect();
    let expected: std::collections::BTreeSet<String> =
        ["global".to_string(), "project".to_string()]
            .into_iter()
            .collect();
    assert_eq!(names, expected);

    fs::remove_file(&trust.config_path).unwrap();

    assert!(pool.reload_if_config_changed().await.unwrap());
    assert_eq!(pool.server_names(), vec!["global".to_string()]);
}

#[tokio::test]
async fn workspace_mcp_pool_reload_drops_project_config_after_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let global_path = dir.path().join("global-mcp.json");
    let workspace = dir.path().join("workspace");
    let project_dir = workspace.join(".codewhale");
    fs::create_dir_all(&project_dir).unwrap();
    let _trust = mark_workspace_trusted(&workspace);
    fs::write(
        &global_path,
        r#"{"servers": {"global": {"command": "node", "args": ["global.js"]}}}"#,
    )
    .unwrap();
    let project_path = project_dir.join("mcp.json");
    fs::write(
        &project_path,
        r#"{"servers": {"project": {"command": "php", "args": ["artisan", "boost:mcp"]}}}"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path_with_workspace(&global_path, &workspace).unwrap();
    let names: std::collections::BTreeSet<String> = pool.server_names().into_iter().collect();
    let expected: std::collections::BTreeSet<String> =
        ["global".to_string(), "project".to_string()]
            .into_iter()
            .collect();
    assert_eq!(names, expected);

    fs::remove_file(project_path).unwrap();

    assert!(pool.reload_if_config_changed().await.unwrap());
    assert_eq!(pool.server_names(), vec!["global".to_string()]);
}

#[test]
fn test_mcp_config_rejects_traversal_path() {
    let err = load_config(Path::new("../mcp.json")).expect_err("traversal path should fail");
    assert!(
        format!("{err:#}").contains("cannot contain '..'"),
        "got: {err:#}"
    );
}

#[cfg(unix)]
#[test]
fn mcp_config_rejects_symlinked_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target-mcp.json");
    let link = dir.path().join("mcp.json");
    fs::write(&target, r#"{"servers": {}}"#).expect("write target config");
    std::os::unix::fs::symlink(&target, &link).expect("symlink mcp config");

    let err = load_config(&link).expect_err("symlinked MCP config should fail");

    assert!(format!("{err:#}").contains("regular file"), "got: {err:#}");
}

#[test]
fn init_mcp_config_rejects_traversal_before_parent_creation() {
    let dir = tempfile::tempdir().unwrap();
    let outside_dir = dir.path().join("outside");
    let path = dir
        .path()
        .join("allowed")
        .join("..")
        .join("outside")
        .join("mcp.json");

    let err = init_config(&path, false).expect_err("traversal path should fail");

    assert!(
        format!("{err:#}").contains("cannot contain '..'"),
        "got: {err:#}"
    );
    assert!(
        !outside_dir.exists(),
        "init_config must validate before creating parent directories"
    );
}

/// A workspace's `.codewhale/mcp.json` is never written through a link: not
/// when `.codewhale` itself is a link, and not when the file is. Reads stay
/// unchanged and the outside directory is never touched.
#[cfg(unix)]
#[test]
fn project_mcp_config_writes_refuse_links_out_of_the_workspace() {
    use std::os::unix::fs::symlink;
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), workspace.path().join(".codewhale")).unwrap();
    let path = workspace_mcp_config_path(workspace.path());

    let err = init_config(&path, false).expect_err("a linked .codewhale is refused");
    assert!(
        format!("{err:#}").contains("Refusing symlinked"),
        "got: {err:#}"
    );
    let err = mutate_config(&path, None, |_| Ok(())).expect_err("mutation is refused too");
    assert!(
        format!("{err:#}").contains("Refusing symlinked"),
        "got: {err:#}"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);

    // A linked file inside a real `.codewhale` is refused as well.
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join(".codewhale")).unwrap();
    let target = outside.path().join("target.json");
    symlink(&target, other.path().join(".codewhale").join("mcp.json")).unwrap();
    assert!(init_config(&workspace_mcp_config_path(other.path()), false).is_err());
    assert!(
        !target.exists(),
        "a link at the file name must not be created through"
    );

    // An ordinary workspace still works.
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(
        init_config(&workspace_mcp_config_path(plain.path()), false).unwrap(),
        McpWriteStatus::Created
    );
}

#[test]
fn test_mcp_config_manager_actions_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");

    assert_eq!(init_config(&path, false).unwrap(), McpWriteStatus::Created);
    assert_eq!(
        init_config(&path, false).unwrap(),
        McpWriteStatus::SkippedExists
    );

    add_server_config(
        &path,
        "local".to_string(),
        Some("node".to_string()),
        None,
        vec!["server.js".to_string()],
        None,
    )
    .unwrap();
    set_server_enabled(&path, "local", false).unwrap();
    let disabled = manager_snapshot_from_config(&path, true).unwrap();
    let local = disabled
        .servers
        .iter()
        .find(|server| server.name == "local")
        .unwrap();
    assert!(!local.enabled);
    assert_eq!(local.transport, "stdio");

    remove_server_config(&path, "local").unwrap();
    let removed = manager_snapshot_from_config(&path, true).unwrap();
    assert!(removed.servers.iter().all(|server| server.name != "local"));
}

#[test]
fn test_mcp_config_adds_explicit_sse_transport() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");

    add_server_config(
        &path,
        "legacy".to_string(),
        None,
        Some("https://example.com/v1/mcp/sse".to_string()),
        Vec::new(),
        Some("sse".to_string()),
    )
    .unwrap();

    let cfg = load_config(&path).unwrap();
    assert_eq!(
        cfg.servers
            .get("legacy")
            .and_then(|server| server.transport.as_deref()),
        Some("sse")
    );

    let snapshot = manager_snapshot_from_config(&path, false).unwrap();
    assert_eq!(snapshot.servers[0].transport, "sse");
}

#[test]
fn test_mcp_config_rejects_unknown_transport() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");

    let err = add_server_config(
        &path,
        "bad".to_string(),
        None,
        Some("https://example.com/mcp".to_string()),
        Vec::new(),
        Some("streamable".to_string()),
    )
    .expect_err("unknown transport should fail");

    assert!(
        format!("{err:#}").contains("Unsupported MCP transport"),
        "got: {err:#}"
    );
}

#[test]
fn test_server_effective_timeouts() {
    let global = McpTimeouts::default();

    let server_with_override = McpServerConfig {
        command: Some("test".to_string()),
        args: vec![],
        env: HashMap::new(),
        cwd: None,
        url: None,
        transport: None,
        connect_timeout: Some(20),
        execute_timeout: None,
        read_timeout: Some(180),
        disabled: false,
        enabled: true,
        required: false,
        lazy: false,
        enabled_tools: Vec::new(),
        disabled_tools: Vec::new(),
        headers: HashMap::new(),
        env_headers: HashMap::new(),
        bearer_token_env_var: None,
        scopes: Vec::new(),
        oauth: None,
        oauth_resource: None,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    };

    assert_eq!(server_with_override.effective_connect_timeout(&global), 20);
    assert_eq!(
        server_with_override.effective_execute_timeout(&global),
        1800
    ); // global default
    assert_eq!(server_with_override.effective_read_timeout(&global), 180);
}

#[test]
fn test_mcp_pool_is_mcp_tool() {
    assert!(McpPool::is_mcp_tool("mcp_filesystem_read"));
    assert!(McpPool::is_mcp_tool("mcp_git_status"));
    assert!(McpPool::is_mcp_tool("list_mcp_resources"));
    assert!(McpPool::is_mcp_tool("list_mcp_resource_templates"));
    assert!(McpPool::is_mcp_tool("read_mcp_resource"));
    assert!(!McpPool::is_mcp_tool("read_file"));
    assert!(!McpPool::is_mcp_tool("exec_shell"));
}

struct ScriptedValueTransport {
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
    responses: VecDeque<Vec<u8>>,
}

#[async_trait::async_trait]
impl McpTransport for ScriptedValueTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&msg)?);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.responses
            .pop_front()
            .context("scripted transport exhausted")
    }
}

struct HangingValueTransport {
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait::async_trait]
impl McpTransport for HangingValueTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&msg)?);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        std::future::pending().await
    }
}

struct ScriptedThenHangingTransport {
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
    responses: VecDeque<Vec<u8>>,
}

#[async_trait::async_trait]
impl McpTransport for ScriptedThenHangingTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&msg)?);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        match self.responses.pop_front() {
            Some(response) => Ok(response),
            None => std::future::pending().await,
        }
    }
}

/// A transport that answers inside `send`, as Streamable HTTP reads the reply
/// within the POST, and never finishes that send.
struct HangingSendTransport;

#[async_trait::async_trait]
impl McpTransport for HangingSendTransport {
    async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
        std::future::pending().await
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        std::future::pending().await
    }
}

/// A transport whose write side is gone — the shape a crashed or exited
/// stdio MCP child leaves behind (EPIPE on the next `write_all`).
struct FailingSendTransport;

#[async_trait::async_trait]
impl McpTransport for FailingSendTransport {
    async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
        anyhow::bail!("Broken pipe (os error 32)")
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        std::future::pending().await
    }
}

struct DropCountingTransport {
    drops: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl McpTransport for DropCountingTransport {
    async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        std::future::pending().await
    }
}

impl Drop for DropCountingTransport {
    fn drop(&mut self) {
        self.drops.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

fn test_server_config() -> McpServerConfig {
    McpServerConfig {
        command: Some("mock".to_string()),
        args: Vec::new(),
        env: HashMap::new(),
        cwd: None,
        url: None,
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
        headers: HashMap::new(),
        env_headers: HashMap::new(),
        bearer_token_env_var: None,
        scopes: Vec::new(),
        oauth: None,
        oauth_resource: None,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    }
}

fn test_connection(transport: Box<dyn McpTransport>) -> McpConnection {
    McpConnection {
        name: "mock".to_string(),
        transport,
        tools: Vec::new(),
        resources: Vec::new(),
        resource_templates: Vec::new(),
        prompts: Vec::new(),
        request_id: AtomicU64::new(1),
        state: ConnectionState::Ready,
        config: test_server_config(),
        server_capabilities: None,
        instructions: None,
        discovery_timeout: Duration::from_secs(default_connect_timeout()),
        read_timeout_secs: default_read_timeout(),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        authority_revocation_reason: Arc::new(std::sync::Mutex::new(None)),
        authority_watch: None,
        catalog_generation: 0,
        decision_key: None,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn execute_timeout_after_partial_stdio_response_does_not_corrupt_next_call() -> Result<()> {
    use serde_json::{Value, json};
    struct SharedStdio(Arc<tokio::sync::Mutex<StdioTransport>>);
    #[async_trait::async_trait]
    impl McpTransport for SharedStdio {
        async fn send(&mut self, bytes: Vec<u8>) -> Result<()> {
            self.0.lock().await.send(bytes).await
        }
        async fn recv(&mut self) -> Result<Vec<u8>> {
            self.0.lock().await.recv().await
        }
    }
    let dir = tempfile::tempdir()?;
    let requests = dir.path().join("requests.jsonl");
    let ready = dir.path().join("ready");
    let script = r#"
printf '%s' '{"jsonrpc":"2.0","id":"1","result":'
: > "$2"
IFS= read -r first
printf '%s\n' "$first" >> "$1"
IFS= read -r second
printf '%s\n' "$second" >> "$1"
printf '%s\n' 'null}' '{"jsonrpc":"2.0","id":"2","result":{"ok":true}}'
IFS= read -r keep_open
"#;
    let mut config = test_server_config();
    config.args = vec![
        "-c".into(),
        script.into(),
        "cw-partial-frame-fixture".into(),
        requests.display().to_string(),
        ready.display().to_string(),
    ];
    let transport = Arc::new(tokio::sync::Mutex::new(StdioTransport::spawn(
        "partial-frame",
        "sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )?));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let mut connection = test_connection(Box::new(SharedStdio(Arc::clone(&transport))));
    let error = connection
        .call_tool("first", json!({}), 1)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_eq!(
        transport.lock().await.pending_line,
        br#"{"jsonrpc":"2.0","id":"1","result":"#
    );
    assert!(connection.is_ready());
    assert_eq!(
        connection.call_tool("second", json!({}), 5).await?,
        json!({"ok": true})
    );
    let sent = fs::read_to_string(requests)?;
    let sent = sent
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(sent.len(), 2, "neither request may be replayed");
    assert_eq!(sent[0]["id"], "1");
    assert_eq!(sent[1]["id"], "2");
    transport.lock().await.shutdown().await;
    Ok::<_, anyhow::Error>(())
}

fn json_frame(value: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn http_request_ceiling_covers_the_execute_budget() {
    let mut config = test_server_config();
    config.execute_timeout = Some(1800);
    config.read_timeout = Some(120);
    let global = McpTimeouts {
        connect_timeout: 10,
        execute_timeout: 1800,
        read_timeout: 120,
    };
    assert_eq!(http_request_ceiling_secs(&config, &global), 1800);

    // A per-server read knob above both stays intact.
    config.read_timeout = Some(3600);
    assert_eq!(http_request_ceiling_secs(&config, &global), 3600);
}

/// A transport that stays silent for a fixed delay, then answers with a
/// matching result — the shape of an MCP server executing a long tool call.
struct DelayedResponseTransport {
    delay: Duration,
    pending: Option<Vec<u8>>,
}

#[async_trait::async_trait]
impl McpTransport for DelayedResponseTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        // Echo the request id so every call gets its matching response.
        let request: serde_json::Value = serde_json::from_slice(&msg)?;
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": {"ok": true}
        });
        self.pending = Some(json_frame(response));
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        tokio::time::sleep(self.delay).await;
        self.pending.take().context("delayed transport exhausted")
    }
}

#[tokio::test]
async fn a_tool_response_after_the_read_knob_still_completes_within_the_execute_budget() {
    let mut connection = test_connection(Box::new(DelayedResponseTransport {
        // The response arrives after the read knob (1s) but well inside the
        // execute budget (30s): the read knob must not bound a request's
        // receive and mark the connection dead before the reply lands.
        delay: Duration::from_millis(1500),
        pending: None,
    }));
    connection.read_timeout_secs = 1;
    let result = connection
        .call_tool("slow", serde_json::json!({}), 30)
        .await
        .expect("a silent tool execution must not be cut off by the read knob");
    assert_eq!(result, serde_json::json!({"ok": true}));
    assert!(
        connection.is_ready(),
        "a completed call keeps the connection"
    );
}

/// A wedged server fails a request at the request's own budget, and the
/// request is abandoned, not the connection: its late reply carries the
/// abandoned id and is skipped (see the stdio fixtures below). The read knob
/// (1s) and the budget (2s) are distinct on purpose: equal deadlines used to
/// race, and whichever timer won decided whether the connection survived.
/// The per-frame read-knob disconnect is still pinned for the handshake by
/// `recv_times_out_waiting_for_mcp_response_and_disconnects`.
#[tokio::test]
async fn a_wedged_request_fails_at_its_own_budget_and_keeps_the_connection() {
    let mut connection = test_connection(Box::new(HangingValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
    }));
    connection.read_timeout_secs = 1;
    let error = connection
        .read_resource("file:///wedged", 2)
        .await
        .expect_err("a wedged server must fail the request at its budget");
    assert!(
        error
            .to_string()
            .contains("MCP method 'resources/read' on server 'mock' timed out after 2s"),
        "the request budget, not the read knob, must end the request: {error:#}"
    );
    assert!(
        connection.is_ready(),
        "an expired request must not declare the connection dead"
    );
}

/// A request whose transport blocks inside `send` (Streamable HTTP) still
/// ends at its own budget, not at the transport's larger client ceiling, and
/// the connection is rebuilt because the abandoned write may be partial.
#[tokio::test]
async fn a_request_blocked_inside_send_ends_at_its_own_budget() {
    let mut connection = test_connection(Box::new(HangingSendTransport));
    connection.read_timeout_secs = 30;
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connection.read_resource("file:///wedged-post", 1),
    )
    .await
    .expect("the request budget, not the transport, must end a blocked send")
    .expect_err("a POST that never completes must end at the request budget");
    assert!(
        error
            .to_string()
            .contains("MCP method 'resources/read' on server 'mock' timed out after 1s"),
        "{error:#}"
    );
    assert!(
        !connection.is_ready(),
        "a send abandoned mid-write leaves the frame boundary unknown, so the connection is rebuilt"
    );
}

#[tokio::test]
async fn call_method_skips_notifications_and_unmatched_responses() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"progress": 0.5}
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 99,
                "result": {"ignored": true}
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"ok": true}
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));

    let result = conn
        .call_method("tools/call", serde_json::json!({"name": "echo"}), 1)
        .await
        .unwrap();

    assert_eq!(result, serde_json::json!({"ok": true}));
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["jsonrpc"], "2.0");
    assert_eq!(sent[0]["id"], "1");
    assert_eq!(sent[0]["method"], "tools/call");
}

#[tokio::test]
async fn call_method_invalid_json_includes_server_output_preview() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([b"Allow Burp MCP connection? [y/N]".to_vec()]),
    };
    let mut conn = test_connection(Box::new(transport));

    let err = conn
        .call_method("tools/call", serde_json::json!({"name": "burp"}), 1)
        .await
        .expect_err("non-json MCP stdout should fail");
    let msg = err.to_string();

    assert!(msg.contains("Invalid MCP JSON-RPC message from server 'mock'"));
    assert!(msg.contains("Allow Burp MCP connection"));
    assert_eq!(conn.state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn recv_times_out_waiting_for_mcp_response_and_disconnects() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut conn = test_connection(Box::new(HangingValueTransport {
        sent: Arc::clone(&sent),
    }));
    conn.read_timeout_secs = 0;

    let err = conn
        .recv("1".to_string())
        .await
        .expect_err("hung transport should time out inside recv");

    assert!(
        err.to_string()
            .contains("Timed out waiting for MCP JSON-RPC response from server 'mock' after 0s"),
        "unexpected error: {err:#}"
    );
    assert_eq!(conn.state(), ConnectionState::Disconnected);
}

#[tokio::test]
async fn call_method_times_out_while_waiting_for_response() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut conn = test_connection(Box::new(HangingValueTransport {
        sent: Arc::clone(&sent),
    }));

    let err = conn
        .call_method("tools/call", serde_json::json!({"name": "echo"}), 0)
        .await
        .expect_err("hung receive should time out");

    assert!(
        err.to_string()
            .contains("MCP method 'tools/call' on server 'mock' timed out after 0s"),
        "unexpected error: {err:#}"
    );
    assert_eq!(sent.lock().unwrap().len(), 1);
}

/// JSON-RPC requires exactly one of `result` / `error` on a response. A
/// response carrying neither is a broken server, and reporting it as a
/// successful call with a `null` payload is a fake success: the tool result
/// reaches the model as `ToolResult::success("null")`, indistinguishable from
/// a tool that genuinely did nothing.
#[tokio::test]
async fn call_method_rejects_a_response_with_neither_result_nor_error() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));

    let err = conn
        .call_method("tools/call", serde_json::json!({"name": "echo"}), 1)
        .await
        .expect_err("a result-less, error-less response is not a successful call");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("neither a result nor an error"),
        "unexpected error: {rendered}"
    );
    assert!(
        rendered.contains("tools/call"),
        "unexpected error: {rendered}"
    );
}

/// …while an *explicit* `"result": null` is a well-formed empty success and
/// must keep flowing through unchanged.
#[tokio::test]
async fn call_method_preserves_an_explicit_null_result() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": null
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));

    let result = conn
        .call_method("tools/call", serde_json::json!({"name": "echo"}), 1)
        .await
        .expect("an explicit null result is a valid response");
    assert_eq!(result, serde_json::Value::Null);
}

/// A failed *write* has to disconnect the connection, exactly like a failed
/// read does. `McpPool::get_or_connect` reuses any connection whose
/// `is_ready()` is true, so a connection left in `Ready` after its transport
/// write side died is never rebuilt — the pool hands the same dead child back
/// on every later tool call and the server stays broken for the rest of the
/// session even though a reconnect would fix it.
#[tokio::test]
async fn call_method_disconnects_when_the_transport_write_side_is_gone() {
    let mut conn = test_connection(Box::new(FailingSendTransport));

    let err = conn
        .call_method("tools/call", serde_json::json!({"name": "echo"}), 1)
        .await
        .expect_err("a dead write side must fail the call");
    assert!(
        format!("{err:#}").contains("Broken pipe"),
        "unexpected error: {err:#}"
    );
    assert_eq!(conn.state(), ConnectionState::Disconnected);
    assert!(
        !conn.is_ready(),
        "a connection whose write side died must not be reused"
    );
}

/// The pool-level consequence of the same defect: `/mcp` (and every
/// `connected_servers` caller) reported a server with a dead write side as
/// still connected, and `get_or_connect` handed the dead connection back
/// instead of rebuilding it.
#[tokio::test]
async fn pool_stops_advertising_a_server_whose_write_side_died() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "mock": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": []
                }
            }
        }"#,
    )
    .unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    let mut conn = test_connection(Box::new(FailingSendTransport));
    conn.tools.push(McpTool {
        name: "echo".to_string(),
        description: None,
        input_schema: serde_json::json!({"type": "object"}),
        annotations: None,
    });
    pool.connections.insert("mock".to_string(), conn);
    assert_eq!(pool.connected_servers(), vec!["mock"]);

    let err = pool
        .call_tool("mcp_mock_echo", serde_json::json!({}))
        .await
        .expect_err("a dead write side must fail the call");
    assert!(
        format!("{err:#}").contains("Broken pipe"),
        "unexpected error: {err:#}"
    );

    assert!(
        pool.connected_servers().is_empty(),
        "a server whose write side died must not report as connected"
    );
    let reconnect = match pool.get_or_connect("mock").await {
        Ok(_) => panic!("the pool must rebuild rather than reuse the dead connection"),
        Err(error) => error,
    };
    assert!(
        format!("{reconnect:#}").contains("spawn failed"),
        "expected a fresh spawn attempt, got: {reconnect:#}"
    );
}

/// #6187: a failed reconnect must not erase the previous connection — the
/// last-good tool catalog stays registered (model-visible, since catalog
/// aggregation filters on authority, not liveness) for the whole outage,
/// while the restored connection stays non-ready so `get_or_connect`
/// keeps retrying per the backoff.
#[tokio::test]
async fn failed_reconnect_restores_last_good_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "mock": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": []
                }
            }
        }"#,
    )
    .unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    let mut conn = test_connection(Box::new(HangingValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
    }));
    conn.name = "mock".to_string();
    conn.config = pool.config.servers.get("mock").unwrap().clone();
    conn.catalog_generation = pool.current_catalog_generation();
    // The shape a crashed server leaves behind: not ready, but its
    // last-good catalog is still discovered on the connection.
    conn.state = ConnectionState::Disconnected;
    conn.tools.push(McpTool {
        name: "echo".to_string(),
        description: None,
        input_schema: serde_json::json!({"type": "object"}),
        annotations: None,
    });
    pool.connections.insert("mock".to_string(), conn);

    // `&mut McpConnection` is not `Debug`, so mirror the sibling test's
    // match instead of `expect_err`.
    let error = match pool.get_or_connect("mock").await {
        Ok(_) => panic!("reconnect against a missing binary must fail"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("spawn failed"),
        "unexpected error: {error:#}"
    );

    let restored = pool
        .connections
        .get("mock")
        .expect("failed reconnect must restore the previous connection");
    assert!(
        !restored.is_ready(),
        "the restored connection must stay non-ready so the pool keeps retrying"
    );
    assert!(
        pool.all_tools()
            .iter()
            .any(|(name, _)| name == "mcp_mock_echo"),
        "the model-visible tool surface must survive the failed reconnect"
    );
    assert_eq!(
        restored.tools.len(),
        1,
        "the restored connection must keep its last-good catalog"
    );
}

#[tokio::test]
async fn test_mcp_pool_empty_config() {
    let pool = McpPool::new(McpConfig::default());
    assert!(pool.server_names().is_empty());
    assert!(pool.all_tools().is_empty());
}

/// #1267 part 2: a pool built without a source path has no file to watch,
/// so `reload_if_config_changed` must short-circuit instead of trying
/// to stat `/`.
#[tokio::test]
async fn reload_if_config_changed_is_noop_without_source_path() {
    let mut pool = McpPool::new(McpConfig::default());
    let reloaded = pool.reload_if_config_changed().await.unwrap();
    assert!(!reloaded, "no source path → no reload");
}

/// #1267 part 2: when the on-disk config is byte-unchanged, the lazy
/// reload must not drop connections — every call to `get_or_connect`
/// would otherwise pay a full reconnect cycle on networked filesystems
/// where mtime granularity is coarse.
#[tokio::test]
async fn reload_if_config_changed_skips_when_content_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    // Force the mtime to advance without changing content.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
    let reloaded = pool.reload_if_config_changed().await.unwrap();
    assert!(
        !reloaded,
        "content-unchanged config must not trigger a reload"
    );
}

/// #1267 part 2: when the on-disk config changes content, the next
/// `reload_if_config_changed` call must swap in the new config and
/// (would) drop all live connections. We can't stand up a real
/// `McpConnection` in a unit test, so we observe the swap via the
/// publicly-readable side: server names go from empty to non-empty.
#[tokio::test]
async fn reload_if_config_changed_swaps_config_on_content_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    assert!(pool.server_names().is_empty());
    // Mutate the file so both the mtime and the hash change.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(
        &path,
        r#"{"servers":{"new":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();
    let reloaded = pool.reload_if_config_changed().await.unwrap();
    assert!(reloaded, "content-changed config must trigger reload");
    let names = pool.server_names();
    assert!(
        names.contains(&"new".to_string()),
        "expected new server in pool after reload, got {names:?}"
    );
}

#[tokio::test]
async fn stale_handshake_cannot_be_restamped_after_config_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(&path, r#"{"servers":{"local":{"command":"node"}}}"#).unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut connection = test_connection(Box::new(DropCountingTransport {
        drops: drops.clone(),
    }));
    connection.catalog_generation = pool.current_catalog_generation();
    std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
    pool.reload_from_config_sources(true).unwrap();
    let error = pool
        .store_ready_connection("local".to_string(), connection)
        .unwrap_err();
    assert!(error.to_string().contains("configuration changed"));
    assert!(!pool.connections.contains_key("local"));
    assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn reload_if_config_changed_drops_live_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"servers":{"local":{"command":"node","args":["server.js"]}}}"#,
    )
    .unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut conn = test_connection(Box::new(DropCountingTransport {
        drops: Arc::clone(&drops),
    }));
    conn.name = "local".to_string();
    conn.config = pool.config.servers.get("local").unwrap().clone();
    pool.connections.insert("local".to_string(), conn);

    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(
        &path,
        r#"{"servers":{"local":{"command":"node","args":["server-v2.js"]}}}"#,
    )
    .unwrap();

    let reloaded = pool.reload_if_config_changed().await.unwrap();
    assert!(reloaded, "content-changed config must trigger reload");
    assert_eq!(
        drops.load(AtomicOrdering::SeqCst),
        1,
        "reload must drop the stale live transport"
    );
    assert!(
        !pool.connections.contains_key("local"),
        "stale connection must not survive config reload"
    );
    assert_eq!(
        pool.config.servers.get("local").unwrap().args,
        vec!["server-v2.js".to_string()]
    );
}

#[tokio::test]
async fn connect_all_reloads_before_snapshotting_new_server_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(&path, r#"{"servers":{}}"#).unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();

    std::fs::write(
        &path,
        r#"{"servers":{"late":{"command":"codewhale-test-command-that-does-not-exist"}}}"#,
    )
    .unwrap();
    // Make the test independent of filesystem mtime granularity.
    pool.last_mtimes = vec![None];

    let errors = pool.connect_all().await;
    assert!(
        pool.server_names().contains(&"late".to_string()),
        "the first connect_all call must install the changed config"
    );
    assert!(
        errors.iter().any(|(name, _)| name == "late"),
        "the newly-added server must be attempted on the same call"
    );
}

#[tokio::test]
async fn explicit_reload_reconnects_unchanged_config_and_preserves_dynamic_servers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"servers":{"local":{"command":"node","disabled":true}}}"#,
    )
    .unwrap();
    let mut pool = McpPool::from_config_path(&path).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut conn = test_connection(Box::new(DropCountingTransport {
        drops: Arc::clone(&drops),
    }));
    conn.name = "local".to_string();
    conn.config = pool.config.servers.get("local").unwrap().clone();
    pool.connections.insert("local".to_string(), conn);
    let mut runtime_config = test_server_config();
    runtime_config.command = Some("runtime-server".to_string());
    pool.add_runtime_server_config("runtime".to_string(), runtime_config)
        .unwrap();
    let generation_before = pool.catalog_generation.load(AtomicOrdering::SeqCst);

    pool.force_reload_config_sources().unwrap();
    let errors = pool.connect_all().await;

    assert!(
        errors.is_empty(),
        "disabled config should not connect: {errors:?}"
    );
    assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
    assert!(!pool.connections.contains_key("local"));
    assert!(pool.server_names().contains(&"runtime".to_string()));
    assert_eq!(
        pool.catalog_generation.load(AtomicOrdering::SeqCst),
        generation_before + 1,
        "explicit reload must invalidate every previously advertised route"
    );
}

#[tokio::test]
async fn config_source_switch_preserves_dynamic_servers_in_the_shared_pool() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let initial_path = dir.path().join("initial.json");
    let invalid_path = dir.path().join("invalid.json");
    let replacement_path = dir.path().join("replacement.json");
    std::fs::write(
        &initial_path,
        r#"{"servers":{"local":{"command":"node","disabled":true}}}"#,
    )
    .unwrap();
    std::fs::write(&invalid_path, r#"{"servers":{"broken": trailing}}"#).unwrap();
    std::fs::write(&replacement_path, r#"{"servers":{}}"#).unwrap();
    let plugins = Arc::new(crate::plugins::PluginRegistry::empty(&workspace));
    let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
        &initial_path,
        &workspace,
        Arc::clone(&plugins),
    )
    .unwrap();
    let mut runtime_config = test_server_config();
    runtime_config.command = Some("runtime-server".to_string());
    pool.add_runtime_server_config("runtime".to_string(), runtime_config)
        .unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut conn = test_connection(Box::new(DropCountingTransport {
        drops: Arc::clone(&drops),
    }));
    conn.name = "local".to_string();
    conn.config = pool.config.servers.get("local").unwrap().clone();
    pool.connections.insert("local".to_string(), conn);
    let generation_before = pool.catalog_generation.load(AtomicOrdering::SeqCst);

    pool.switch_workspace_config_source(&invalid_path, &workspace, Arc::clone(&plugins))
        .expect_err("malformed replacement must fail closed");
    assert_eq!(pool.config_sources.first(), Some(&initial_path));
    assert!(pool.connections.contains_key("local"));
    assert_eq!(drops.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(
        pool.catalog_generation.load(AtomicOrdering::SeqCst),
        generation_before
    );

    pool.switch_workspace_config_source(&replacement_path, &workspace, plugins)
        .unwrap();
    let errors = pool.connect_all().await;

    assert!(errors.is_empty());
    assert!(pool.server_names().contains(&"runtime".to_string()));
    assert_eq!(pool.config_sources.first(), Some(&replacement_path));
    assert_eq!(drops.load(AtomicOrdering::SeqCst), 1);
}

/// #1267 part 2: hash-based comparison must be stable for byte-identical
/// configs and distinct for differing configs.
#[test]
fn hash_mcp_config_is_stable_and_change_sensitive() {
    let a = McpConfig::default();
    let b = McpConfig::default();
    assert_eq!(hash_mcp_config(&a), hash_mcp_config(&b));
    let mut c = McpConfig::default();
    c.servers.insert(
        "x".into(),
        McpServerConfig {
            command: Some("/bin/echo".into()),
            args: vec!["hi".into()],
            env: Default::default(),
            cwd: None,
            url: None,
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
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        },
    );
    assert_ne!(
        hash_mcp_config(&a),
        hash_mcp_config(&c),
        "hash must change when servers map changes"
    );
}

/// #1267 part 2: `hash_mcp_config` is the *only* thing standing between a
/// touched-but-unchanged config file and a full teardown of every live MCP
/// connection (stdio children included). `McpConfig::servers`, `env`,
/// `headers`, and `env_headers` are all `HashMap`s, and two `HashMap`s built
/// separately in one process iterate in different orders — so hashing the
/// config's `serde_json` bytes straight from the struct is not
/// content-addressed once a map holds more than one entry. Parse the same
/// bytes repeatedly and require one hash.
#[test]
fn hash_mcp_config_is_order_independent_across_identical_parses() {
    let raw = r#"{
        "servers": {
            "alpha":   { "command": "a", "env": { "A": "1", "B": "2", "C": "3", "D": "4" } },
            "bravo":   { "command": "b" },
            "charlie": { "command": "c" },
            "delta":   { "command": "d" },
            "echo":    { "command": "e" },
            "foxtrot": { "command": "f" },
            "golf":    { "command": "g" },
            "hotel":   { "command": "h" },
            "india":   { "command": "i" },
            "juliett": { "command": "j" }
        }
    }"#;
    let hashes: HashSet<u64> = (0..16)
        .map(|_| {
            let parsed: McpConfig = serde_json::from_str(raw).expect("fixture parses");
            hash_mcp_config(&parsed)
        })
        .collect();
    assert_eq!(
        hashes.len(),
        1,
        "byte-identical MCP config must hash identically; got {} distinct hashes",
        hashes.len()
    );
}

/// The same invariant for the nested per-server maps: an unchanged server
/// whose `env` / `headers` hold several entries must not look changed.
#[test]
fn hash_mcp_config_is_order_independent_for_nested_server_maps() {
    let raw = r#"{
        "servers": {
            "only": {
                "url": "https://example.invalid/mcp",
                "headers": { "H1": "1", "H2": "2", "H3": "3", "H4": "4", "H5": "5", "H6": "6" },
                "env_headers": { "E1": "V1", "E2": "V2", "E3": "V3", "E4": "V4" }
            }
        }
    }"#;
    let hashes: HashSet<u64> = (0..16)
        .map(|_| {
            let parsed: McpConfig = serde_json::from_str(raw).expect("fixture parses");
            hash_mcp_config(&parsed)
        })
        .collect();
    assert_eq!(
        hashes.len(),
        1,
        "byte-identical MCP config must hash identically; got {} distinct hashes",
        hashes.len()
    );
}

/// #1319: discovered tools must be sorted by name so the prompt prefix
/// is stable across runs (cache-hit stability), even when the server
/// returns them in arbitrary or paginated order.
#[tokio::test]
async fn discover_tools_sorts_by_name_for_cache_stability() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "tools": [
                        { "name": "zeta", "inputSchema": {} },
                        { "name": "alpha", "inputSchema": {} }
                    ],
                    "nextCursor": "page-2"
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        { "name": "mu", "inputSchema": {} },
                        { "name": "beta", "inputSchema": {} }
                    ]
                }
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.tools = conn
        .discover_tools(&mut McpCatalogBudget::new())
        .await
        .expect("discover");

    let names: Vec<&str> = conn.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "beta", "mu", "zeta"],
        "tools must be sorted by name regardless of server order or pagination"
    );
}

#[tokio::test]
async fn discover_tools_rejects_a_repeated_pagination_cursor_without_publishing_partials() {
    let transport = ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "tools": [{ "name": "first", "inputSchema": {} }],
                    "nextCursor": "same"
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [{ "name": "second", "inputSchema": {} }],
                    "nextCursor": "same"
                }
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));

    let error = conn
        .discover_tools(&mut McpCatalogBudget::new())
        .await
        .expect_err("repeated cursor must abort discovery");
    assert!(error.to_string().contains("repeated pagination cursor"));
    assert!(
        conn.tools.is_empty(),
        "an aborted catalogue must not publish attacker-controlled partial entries"
    );
}

#[test]
fn mcp_tool_description_formatter_is_one_line_and_unicode_safe() {
    let long_cjk = format!("{}\n这行不应显示", "鲸".repeat(81));
    assert_eq!(
        format_mcp_tool_description(Some(&long_cjk)),
        format!(": {}...", "鲸".repeat(80))
    );
    assert_eq!(
        format_mcp_tool_description(Some("第一行\r\n第二行")),
        ": 第一行"
    );
    assert_eq!(format_mcp_tool_description(Some("  \nignored")), "");
    assert_eq!(format_mcp_tool_description(None), "");
}

#[tokio::test]
async fn discover_all_honors_tools_only_server_capabilities() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "tools-only", "version": "1.0.0"},
                    "capabilities": {"tools": {}}
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [{"name": "idea_search", "inputSchema": {}}]
                }
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));

    conn.initialize().await.expect("initialize");
    conn.discover_all().await.expect("discover tools");

    assert_eq!(
        conn.server_capabilities,
        Some(McpServerCapabilities {
            tools: true,
            resources: false,
            prompts: false,
        })
    );
    assert_eq!(conn.tools.len(), 1);
    assert!(conn.resources.is_empty());
    assert!(conn.resource_templates.is_empty());
    assert!(conn.prompts.is_empty());
    let methods: Vec<_> = sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message.get("method").and_then(|method| method.as_str()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        methods,
        ["initialize", "notifications/initialized", "tools/list"]
    );
}

#[tokio::test]
async fn discover_all_populates_every_advertised_capability() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "full", "version": "1.0.0"},
                    "capabilities": {"tools": {}, "resources": {}, "prompts": {}}
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [{"name": "search", "inputSchema": {}}]}
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {"resources": [{"uri": "file:///readme", "name": "readme"}]}
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "result": {
                    "resourceTemplates": [{"uriTemplate": "file:///{path}", "name": "file"}]
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "result": {"prompts": [{"name": "review"}]}
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));

    conn.initialize().await.expect("initialize");
    conn.discover_all().await.expect("discover all");

    assert_eq!(
        conn.server_capabilities,
        Some(McpServerCapabilities {
            tools: true,
            resources: true,
            prompts: true,
        })
    );
    assert_eq!(conn.tools.len(), 1);
    assert_eq!(conn.resources.len(), 1);
    assert_eq!(conn.resource_templates.len(), 1);
    assert_eq!(conn.prompts.len(), 1);
    let methods: Vec<_> = sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message.get("method").and_then(|method| method.as_str()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
        ]
    );
}

#[tokio::test]
async fn legacy_optional_discovery_hangs_are_bounded_and_fail_soft() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedThenHangingTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": [{"name": "search", "inputSchema": {}}]}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.discovery_timeout = Duration::from_millis(60);

    let started = tokio::time::Instant::now();
    conn.discover_all()
        .await
        .expect("hung optional methods must not fail discovery");

    assert_eq!(conn.server_capabilities, None);
    assert_eq!(conn.tools.len(), 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "optional discovery exceeded its bounded budget: {:?}",
        started.elapsed()
    );
    let methods: Vec<_> = sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message.get("method").and_then(|method| method.as_str()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        methods,
        [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
        ]
    );
}

#[test]
fn manager_snapshot_preserves_advertised_and_legacy_capability_provenance() {
    let advertised_config = test_server_config();
    let legacy_config = test_server_config();
    let config = McpConfig {
        servers: HashMap::from([
            ("advertised".to_string(), advertised_config.clone()),
            ("legacy".to_string(), legacy_config.clone()),
        ]),
        ..McpConfig::default()
    };
    let mut pool = McpPool::new(config.clone());
    let drops = Arc::new(AtomicUsize::new(0));

    let mut advertised = test_connection(Box::new(DropCountingTransport {
        drops: Arc::clone(&drops),
    }));
    advertised.name = "advertised".to_string();
    advertised.config = advertised_config;
    advertised.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: false,
        prompts: true,
    });
    pool.connections
        .insert("advertised".to_string(), advertised);

    let mut legacy = test_connection(Box::new(DropCountingTransport { drops }));
    legacy.name = "legacy".to_string();
    legacy.config = legacy_config;
    pool.connections.insert("legacy".to_string(), legacy);

    let errors = HashMap::new();
    let snapshot = snapshot_from_config(
        Path::new("mcp.json"),
        true,
        false,
        &config,
        Some((&pool, &errors)),
    );

    assert_eq!(
        snapshot.servers[0].capability_metadata,
        McpServerCapabilityMetadata::Advertised(McpServerCapabilities {
            tools: true,
            resources: false,
            prompts: true,
        })
    );
    assert_eq!(
        snapshot.servers[1].capability_metadata,
        McpServerCapabilityMetadata::LegacyFallback
    );
}

#[tokio::test]
async fn mcp_pool_call_tool_preserves_tool_names_with_dashes() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"ok": true}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.name = "dephy".to_string();
    conn.tools = vec![McpTool {
        name: "company--search".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    }];

    let mut pool = McpPool::new(McpConfig {
        timeouts: McpTimeouts::default(),
        servers: HashMap::new(),
    });
    pool.connections.insert("dephy".to_string(), conn);

    let result = pool
        .call_tool(
            "mcp_dephy_company--search",
            serde_json::json!({"query": "dephy"}),
        )
        .await
        .unwrap();

    assert_eq!(result, serde_json::json!({"ok": true}));
    let sent = sent.lock().unwrap();
    assert_eq!(sent[0]["method"], "tools/call");
    assert_eq!(sent[0]["params"]["name"], "company--search");
    assert_eq!(
        sent[0]["params"]["arguments"],
        serde_json::json!({"query": "dephy"})
    );
}

#[tokio::test]
async fn mcp_pool_rejects_unadvertised_tool_without_sending_tools_call() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        // A malicious server could implement this hidden method, but local
        // catalog authorization must prevent the transport from seeing it.
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": {"deleted": true}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.name = "spy".to_string();
    conn.tools = vec![McpTool {
        name: "read".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    }];
    let mut pool = McpPool::new(McpConfig::default());
    pool.connections.insert("spy".to_string(), conn);

    let error = pool
        .call_tool("mcp_spy_delete", serde_json::json!({}))
        .await
        .expect_err("unadvertised hidden tool must fail locally");
    assert!(error.to_string().contains("Unknown MCP tool name"));
    assert!(sent.lock().unwrap().is_empty(), "zero tools/call requests");
}

#[tokio::test]
async fn mcp_pool_binds_prompts_and_resources_to_advertised_catalog() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": {"contents": []}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.name = "catalog".to_string();
    conn.prompts = vec![McpPrompt {
        name: "review".to_string(),
        description: None,
        arguments: Vec::new(),
    }];
    conn.resources = vec![McpResource {
        uri: "file:///readme".to_string(),
        name: "readme".to_string(),
        description: None,
        mime_type: None,
    }];
    conn.resource_templates = vec![McpResourceTemplate {
        uri_template: "repo://item/{id}".to_string(),
        name: "item".to_string(),
        description: None,
        mime_type: None,
    }];
    let mut pool = McpPool::new(McpConfig::default());
    pool.connections.insert("catalog".to_string(), conn);

    pool.get_prompt("catalog", "hidden", serde_json::json!({}))
        .await
        .expect_err("hidden prompt must fail locally");
    pool.read_resource("catalog", "file:///hidden")
        .await
        .expect_err("hidden literal resource must fail locally");
    assert!(sent.lock().unwrap().is_empty());

    let result = pool
        .read_resource("catalog", "repo://item/42")
        .await
        .expect("exact advertised template expansion is callable");
    assert_eq!(result, serde_json::json!({"contents": []}));
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["method"], "resources/read");
    assert_eq!(sent[0]["params"]["uri"], "repo://item/42");
}

#[tokio::test]
async fn mcp_pool_call_tool_preserves_server_names_with_underscores() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"ok": true}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.name = "my_db".to_string();
    conn.tools = vec![McpTool {
        name: "execute_sql".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    }];

    let mut pool = McpPool::new(McpConfig {
        timeouts: McpTimeouts::default(),
        servers: HashMap::new(),
    });
    pool.connections.insert("my_db".to_string(), conn);

    let result = pool
        .call_tool(
            "mcp_my_db_execute_sql",
            serde_json::json!({"query": "select 1"}),
        )
        .await
        .unwrap();

    assert_eq!(result, serde_json::json!({"ok": true}));
    let sent = sent.lock().unwrap();
    assert_eq!(sent[0]["method"], "tools/call");
    assert_eq!(sent[0]["params"]["name"], "execute_sql");
    assert_eq!(
        sent[0]["params"]["arguments"],
        serde_json::json!({"query": "select 1"})
    );
}

#[tokio::test]
async fn mcp_pool_hides_and_rejects_ambiguous_model_tool_names() {
    let sent_short = Arc::new(Mutex::new(Vec::new()));
    let short_transport = ScriptedValueTransport {
        sent: Arc::clone(&sent_short),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"short": true}
        }))]),
    };
    let mut short_conn = test_connection(Box::new(short_transport));
    short_conn.name = "my".to_string();
    short_conn.tools = vec![McpTool {
        name: "db_execute_sql".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    }];

    let sent_long = Arc::new(Mutex::new(Vec::new()));
    let long_transport = ScriptedValueTransport {
        sent: Arc::clone(&sent_long),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"long": true}
        }))]),
    };
    let mut long_conn = test_connection(Box::new(long_transport));
    long_conn.name = "my_db".to_string();
    long_conn.tools = vec![McpTool {
        name: "execute_sql".to_string(),
        description: None,
        input_schema: serde_json::json!({}),
        annotations: None,
    }];

    let mut pool = McpPool::new(McpConfig {
        timeouts: McpTimeouts::default(),
        servers: HashMap::new(),
    });
    pool.connections.insert("my".to_string(), short_conn);
    pool.connections.insert("my_db".to_string(), long_conn);

    assert!(
        pool.all_tools().is_empty(),
        "ambiguous names must never be advertised to the model"
    );
    let error = pool
        .call_tool(
            "mcp_my_db_execute_sql",
            serde_json::json!({"query": "select 1"}),
        )
        .await
        .expect_err("ambiguous tool route must fail closed");

    assert!(error.to_string().contains("Ambiguous MCP tool name"));
    assert!(
        sent_short.lock().unwrap().is_empty(),
        "neither authority may receive an ambiguous tool call"
    );
    assert!(
        sent_long.lock().unwrap().is_empty(),
        "neither authority may receive an ambiguous tool call"
    );
}

#[tokio::test]
async fn json_rpc_session_error_is_marked_stale() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32001,
                "message": "MCP session expired"
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));

    let err = conn
        .call_tool("search", serde_json::json!({"query": "dephy"}), 1)
        .await
        .expect_err("session error should fail");

    assert!(
        is_mcp_stale_session_error(&err),
        "JSON-RPC session error should be retryable, got: {err:#}"
    );
}

#[test]
fn sse_transport_closed_is_retryable() {
    let err = anyhow::anyhow!("SSE transport closed");
    assert!(
        is_mcp_stale_session_error(&err),
        "closed SSE stream should force reconnect before retry"
    );
}

#[test]
fn stdio_transport_closed_is_retryable() {
    let err = anyhow::anyhow!("Stdio transport closed (exit status: 1)");
    assert!(
        is_mcp_stale_session_error(&err),
        "dead stdio child should force reconnect before retry"
    );
}

#[test]
fn legacy_sse_post_disconnect_is_retryable() {
    let err = anyhow::anyhow!(
        "MCP SSE POST send failed (transport=sse endpoint=http://127.0.0.1:123/messages): connection closed before message completed"
    );
    assert!(
        is_mcp_stale_session_error(&err),
        "closed legacy SSE POST should force reconnect before retry"
    );

    let err = anyhow::anyhow!(
        "MCP SSE POST send failed (transport=sse endpoint=http://127.0.0.1:123/messages): connection reset by peer"
    );
    assert!(
        is_mcp_stale_session_error(&err),
        "reset legacy SSE POST should force reconnect before retry"
    );

    let err = anyhow::anyhow!(
        "MCP SSE POST send failed (transport=sse endpoint=http://127.0.0.1:123/messages): An existing connection was forcibly closed by the remote host."
    );
    assert!(
        is_mcp_stale_session_error(&err),
        "Windows reset wording should force reconnect before retry"
    );
}

#[tokio::test]
async fn discover_all_ignores_unsupported_optional_capabilities() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "tools": [
                        { "name": "search", "inputSchema": {} }
                    ]
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": {
                    "code": -32601,
                    "message": "resources not supported"
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "error": {
                    "code": -32601,
                    "message": "resource templates not supported"
                }
            })),
            json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "error": {
                    "code": -32601,
                    "message": "prompts not supported"
                }
            })),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: true,
        prompts: true,
    });

    conn.discover_all().await.expect("discover");

    assert_eq!(conn.tools.len(), 1);
    assert_eq!(conn.tools[0].name, "search");
    assert!(conn.resources.is_empty());
    assert!(conn.resource_templates.is_empty());
    assert!(conn.prompts.is_empty());
    let methods: Vec<_> = sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message.get("method").and_then(|method| method.as_str()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        methods,
        [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
        ]
    );
}

#[tokio::test]
async fn discover_all_keeps_advertised_tool_discovery_required() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent,
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32601, "message": "tools not supported"}
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: false,
        prompts: false,
    });

    let error = conn
        .discover_all()
        .await
        .expect_err("advertised tools/list failure must fail discovery");

    assert!(
        error.to_string().contains("MCP error in 'tools/list'"),
        "unexpected error: {error:#}"
    );
}

/// #1244: when an MCP stdio server fails to spawn, the underlying OS
/// error (e.g. ENOENT for a missing binary) must reach the user via the
/// snapshot.error string. Regression test for `err.to_string()` dropping
/// the anyhow chain — without `{err:#}` the user sees only the opaque
/// wrapper "MCP stdio spawn failed (...)" and has nothing to act on.
#[tokio::test]
async fn discover_snapshot_includes_underlying_spawn_error_in_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "broken": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": []
                }
            }
        }"#,
    )
    .unwrap();

    let snapshot = discover_manager_snapshot(&path, None, false).await.unwrap();
    let server = snapshot
        .servers
        .iter()
        .find(|s| s.name == "broken")
        .expect("broken server should appear in snapshot");
    let err = server
        .error
        .as_deref()
        .expect("broken server should have an error");
    let lowered = err.to_lowercase();
    assert!(
        lowered.contains("os error")
            || lowered.contains("not found")
            || lowered.contains("no such"),
        "expected underlying spawn error in chain, got: {err}"
    );
}

#[tokio::test]
async fn discover_snapshot_explains_a_missing_node_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    let missing_node = dir
        .path()
        .join(if cfg!(windows) { "node.exe" } else { "node" });
    fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "mcpServers": { "computer": { "command": missing_node, "args": [] } }
        }))
        .unwrap(),
    )
    .unwrap();
    let snapshot = discover_manager_snapshot(&config_path, None, false)
        .await
        .unwrap();
    let error = snapshot
        .servers
        .iter()
        .find(|server| server.name == "computer")
        .unwrap()
        .error
        .as_ref()
        .unwrap();
    assert!(error.contains("Node.js 20 or newer"), "{error}");
    assert!(error.contains("https://nodejs.org/"), "{error}");
}

/// The same guarantee for a server the user marked `required`. `connect_all`
/// appends a generic "required MCP server failed to initialize" entry after
/// the real per-server connect error, and every snapshot path folds the
/// returned pairs into a `HashMap<name, message>` — so the later, contentless
/// entry overwrites the diagnosis. Marking a server required must not blind
/// the user to *why* it did not start.
#[tokio::test]
async fn required_server_snapshot_keeps_the_real_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "broken": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": [],
                    "required": true
                }
            }
        }"#,
    )
    .unwrap();

    let snapshot = discover_manager_snapshot(&path, None, false).await.unwrap();
    let server = snapshot
        .servers
        .iter()
        .find(|s| s.name == "broken")
        .expect("broken server should appear in snapshot");
    let err = server
        .error
        .as_deref()
        .expect("broken server should have an error");
    let lowered = err.to_lowercase();
    assert!(
        lowered.contains("os error")
            || lowered.contains("not found")
            || lowered.contains("no such"),
        "required server must still report why it failed, got: {err}"
    );
}

/// `connect_all` must report one error per failed server. A `required`
/// server that already failed to connect got a second, contentless entry
/// appended for the same name.
#[tokio::test]
async fn connect_all_reports_one_error_per_failed_required_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "broken": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": [],
                    "required": true
                }
            }
        }"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path(&path).unwrap();
    let errors = pool.connect_all().await;
    let for_broken: Vec<_> = errors.iter().filter(|(name, _)| name == "broken").collect();
    assert_eq!(
        for_broken.len(),
        1,
        "expected exactly one error for 'broken', got: {:?}",
        errors
            .iter()
            .map(|(name, err)| format!("{name}: {err:#}"))
            .collect::<Vec<_>>()
    );
    let rendered = format!("{:#}", for_broken[0].1).to_lowercase();
    assert!(
        rendered.contains("spawn failed"),
        "the single error must be the real cause, got: {rendered}"
    );
}

/// A dead server must not be re-dialed on every turn.
///
/// The turn loop rebuilds the tool catalog on each user message, and that
/// path calls `connect_all`. Before the cooldown, a wall of unreachable
/// servers meant a full round of connect timeouts before every first token —
/// the "MCP is always reloading and slowing things down" report. The second
/// pass must produce the same diagnosis without dialing anything.
#[tokio::test]
async fn a_failed_server_waits_out_a_cooldown_instead_of_redialing_every_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{
            "mcpServers": {
                "broken": {
                    "command": "codewhale-tui-test-this-binary-does-not-exist-9f8e7d6c5b4a",
                    "args": []
                }
            }
        }"#,
    )
    .unwrap();

    let mut pool = McpPool::from_config_path(&path).unwrap();
    let first = pool.connect_all().await;
    assert_eq!(first.len(), 1, "first pass should dial and fail once");

    // Second pass: still reported as failing, but nothing is queued to dial.
    let (pending, errors) = pool.collect_pending_connects(None);
    assert!(
        pending.is_empty(),
        "a server inside its cooldown must not be re-dialed: {:?}",
        pending.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    assert_eq!(errors.len(), 1, "the failure must still be reported");
    assert_eq!(errors[0].0, "broken");
    assert!(
        format!("{:#}", errors[0].1)
            .to_lowercase()
            .contains("spawn"),
        "the replayed diagnosis must be the real one: {:#}",
        errors[0].1
    );

    // Asking for that server by name is explicit intent and lifts the wait.
    assert!(pool.retry_connection("broken").await.is_err());
    let (pending, _) = pool.collect_pending_connects(None);
    assert!(
        pending.is_empty(),
        "the failed retry restarts the ladder rather than clearing it"
    );
}

/// Lazy boot (#6033): the scoped collect starts only the eager set —
/// `required` servers plus ones an explicit tool selection covers — and the
/// pool tracks exactly those names as in-flight, so "connecting" never has
/// to be inferred from "enabled but unconnected".
#[test]
fn lazy_boot_scopes_pending_connects_and_tracks_in_flight() {
    let mut required_cfg = test_server_config();
    required_cfg.required = true;
    required_cfg.lazy = true;
    let mut selected_cfg = test_server_config();
    selected_cfg.lazy = true;
    let mut lazy_cfg = test_server_config();
    lazy_cfg.lazy = true;
    let mut pool = McpPool::new(McpConfig {
        timeouts: McpTimeouts::default(),
        servers: HashMap::from([
            ("needed".to_string(), required_cfg),
            ("selected".to_string(), selected_cfg),
            ("lazy".to_string(), lazy_cfg),
        ]),
    });
    let requested = vec!["mcp_selected_read".to_string()];

    let eager = pool.eager_boot_server_names(&requested);
    assert_eq!(
        eager,
        HashSet::from(["needed".to_string(), "selected".to_string()]),
        "the eager set is required servers plus selection-covered ones"
    );

    let (pending, errors) = pool.collect_pending_connects(Some(&eager));
    assert!(errors.is_empty());
    let pending_names: HashSet<String> = pending.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(pending_names, eager);
    assert_eq!(
        pool.connecting_servers()
            .into_iter()
            .collect::<HashSet<_>>(),
        pending_names,
        "in-flight marks must name exactly the spawned connects"
    );

    // A lazy server is neither spawned nor reported connecting.
    let (pending, errors) = pool.collect_pending_connects(Some(&eager));
    assert!(
        pending.is_empty() && errors.is_empty(),
        "an in-flight name is not re-queued by a second scoped pass"
    );

    // Explicit selection is intent: the lazy server starts on demand and is
    // marked in-flight while it does.
    let (pending, errors) = pool.take_pending_connects_for(&["lazy".to_string()]);
    assert!(errors.is_empty());
    assert_eq!(pending.len(), 1);
    assert!(pool.connecting_servers().contains(&"lazy".to_string()));

    // Aborting clears the marks without touching connection state.
    pool.cancel_connecting(&HashSet::from([
        "needed".to_string(),
        "selected".to_string(),
        "lazy".to_string(),
    ]));
    assert!(pool.connecting_servers().is_empty());
}

/// Selection coverage shared by lazy boot and the per-turn wait: exact
/// `mcp_<server>_<tool>` names and `mcp_<prefix>*` globs both count.
#[test]
fn tool_selection_covers_exact_names_and_globs() {
    let selected = vec![
        "mcp_fs_read".to_string(),
        "mcp_git_*".to_string(),
        "shell".to_string(),
    ];
    assert!(tool_selection_covers_server(&selected, "fs"));
    assert!(tool_selection_covers_server(&selected, "git_status"));
    assert!(!tool_selection_covers_server(&selected, "slack"));
    // A prefix glob reaches every server whose `mcp_<server>_` namespace
    // starts with it: `mcp_gi*` covers `git` and `gitea` alike.
    let glob = vec!["mcp_gi*".to_string()];
    assert!(tool_selection_covers_server(&glob, "git"));
    assert!(tool_selection_covers_server(&glob, "gitea"));
    assert!(!tool_selection_covers_server(&glob, "fs"));
}

#[test]
fn connect_backoff_doubles_then_holds_at_the_ceiling() {
    use std::time::Duration;
    assert_eq!(connect_backoff_delay(1), Duration::from_secs(30));
    assert_eq!(connect_backoff_delay(2), Duration::from_secs(60));
    assert_eq!(connect_backoff_delay(3), Duration::from_secs(120));
    assert_eq!(connect_backoff_delay(6), Duration::from_secs(600));
    assert_eq!(
        connect_backoff_delay(50),
        Duration::from_secs(600),
        "the ceiling holds; a long-dead server is retried every ten minutes"
    );
}

#[test]
fn parse_sse_message_data_extracts_message_events() {
    let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\r\n\r\n";
    let messages = parse_sse_message_data(body);
    assert_eq!(messages.len(), 1);
    let value: serde_json::Value = serde_json::from_slice(&messages[0]).unwrap();
    assert_eq!(value["id"], 1);
    assert!(value.get("result").is_some());
}

#[test]
fn response_id_matches_string_and_numeric_echoes() {
    assert!(response_id_matches(Some(&serde_json::json!("1")), "1"));
    assert!(response_id_matches(Some(&serde_json::json!(1)), "1"));
    assert!(!response_id_matches(Some(&serde_json::json!("2")), "1"));
}

#[test]
fn legacy_sse_transport_requires_explicit_config() {
    let mut server = test_server_config();
    server.url = Some("https://example.com/mcp/abc/sse".to_string());

    assert!(
        !is_legacy_sse_transport(&server),
        "/sse paths must not force legacy SSE without an explicit transport override"
    );

    server.transport = Some("sse".to_string());
    assert!(is_legacy_sse_transport(&server));

    server.transport = Some("SSE".to_string());
    assert!(is_legacy_sse_transport(&server));

    server.transport = Some("http".to_string());
    assert!(!is_legacy_sse_transport(&server));
}

#[test]
fn find_sse_event_separator_accepts_lf_and_crlf() {
    assert_eq!(
        find_sse_event_separator("event: endpoint\n\n"),
        Some((15, 2))
    );
    assert_eq!(
        find_sse_event_separator("event: endpoint\r\n\r\n"),
        Some((15, 4))
    );
}

#[test]
fn find_sse_event_separator_bytes_matches_str_and_survives_multibyte() {
    // Same offsets as the str version.
    assert_eq!(
        find_sse_event_separator_bytes(b"event: endpoint\n\n"),
        Some((15, 2))
    );
    assert_eq!(
        find_sse_event_separator_bytes(b"event: endpoint\r\n\r\n"),
        Some((15, 4))
    );
    // A frame whose data holds a multi-byte char, accumulated byte-wise and
    // split mid-char across two reads, decodes intact (no U+FFFD).
    let frame = "data: 你好\n\n";
    let bytes = frame.as_bytes();
    let split = bytes.len() - 3; // inside "好" / before the separator
    let mut buffer: Vec<u8> = Vec::new();
    buffer.extend_from_slice(&bytes[..split]);
    assert_eq!(find_sse_event_separator_bytes(&buffer), None);
    buffer.extend_from_slice(&bytes[split..]);
    let (pos, sep) = find_sse_event_separator_bytes(&buffer).expect("separator");
    let block = String::from_utf8_lossy(&buffer[..pos]).into_owned();
    assert_eq!(block, "data: 你好");
    assert!(!block.contains('\u{FFFD}'), "multibyte corrupted");
    assert_eq!(sep, 2);
}

#[tokio::test]
async fn mcp_connection_supports_streamable_http_event_stream_responses() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_http_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        let header_end = loop {
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before headers completed");
            request.extend_from_slice(&buf[..n]);
            if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
        };

        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let total_len = header_end + content_length;
        while request.len() < total_len {
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before body completed");
            request.extend_from_slice(&buf[..n]);
        }

        String::from_utf8(request).unwrap()
    }

    async fn write_json_sse(socket: &mut TcpStream, response: serde_json::Value) {
        let body = format!("event: message\ndata: {response}\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let request = read_http_request(&mut socket).await;
                assert!(request.starts_with("POST /mcp "));
                assert!(
                    request.contains("Accept: application/json, text/event-stream")
                        || request.contains("accept: application/json, text/event-stream")
                );
                let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                let value: serde_json::Value = serde_json::from_str(body).unwrap();
                let method = value["method"].as_str().unwrap();

                if method == "notifications/initialized" {
                    socket
                        .write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .unwrap();
                    return;
                }

                let id = value["id"].clone();
                let result = match method {
                    "initialize" => serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": {"name": "mock-streamable", "version": "1.0.0"},
                        "capabilities": {"tools": {}, "resources": {}, "prompts": {}}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{
                            "name": "read_wiki_structure",
                            "description": "Read wiki structure",
                            "inputSchema": {"type": "object"}
                        }]
                    }),
                    "resources/list" => serde_json::json!({"resources": []}),
                    "resources/templates/list" => {
                        serde_json::json!({"resourceTemplates": []})
                    }
                    "prompts/list" => serde_json::json!({"prompts": []}),
                    other => panic!("unexpected method: {other}"),
                };
                write_json_sse(
                    &mut socket,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    }),
                )
                .await;
            });
        }
    });

    let config = McpServerConfig {
        command: None,
        args: vec![],
        env: HashMap::new(),
        cwd: None,
        url: Some(format!("http://{addr}/mcp")),
        transport: None,
        connect_timeout: Some(5),
        execute_timeout: None,
        read_timeout: None,
        disabled: false,
        enabled: true,
        required: false,
        lazy: false,
        enabled_tools: Vec::new(),
        disabled_tools: Vec::new(),
        headers: HashMap::new(),
        env_headers: HashMap::new(),
        bearer_token_env_var: None,
        scopes: Vec::new(),
        oauth: None,
        oauth_resource: None,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    };

    let conn = McpConnection::connect_with_policy(
        "deepwiki".to_string(),
        config,
        &McpTimeouts::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(conn.state(), ConnectionState::Ready);
    assert_eq!(conn.tools().len(), 1);
    assert_eq!(conn.tools()[0].name, "read_wiki_structure");

    server.abort();
}

#[test]
fn mask_url_secrets_strips_userinfo() {
    let masked = mask_url_secrets("https://user:s3cret@host.example/api?foo=bar");
    assert!(masked.contains("***"), "expected masked userinfo: {masked}");
    assert!(!masked.contains("s3cret"), "secret leaked: {masked}");
    assert!(masked.contains("host.example"), "host preserved: {masked}");
}

#[test]
fn mask_url_secrets_passes_through_clean_url() {
    assert_eq!(
        mask_url_secrets("https://api.example.com/mcp"),
        "https://api.example.com/mcp"
    );
}

#[test]
fn redact_body_preview_masks_bearer_token() {
    let redacted = redact_body_preview(
        "Authorization: Bearer abc.def.ghi end; authorization: bearer second-token end",
    );
    assert_eq!(
        redacted.matches("Bearer ***").count() + redacted.matches("bearer ***").count(),
        2,
        "redacted: {redacted}"
    );
    assert!(
        !redacted.contains("abc.def.ghi") && !redacted.contains("second-token"),
        "leaked: {redacted}"
    );
}

#[test]
fn redact_proxy_userinfo_strips_password() {
    // Corporate-style proxy URL with embedded creds — the
    // password must never reach the on-disk log file. URL strings
    // are assembled from placeholder constants via `format!` so the
    // literal source never contains a scheme-prefixed username +
    // password pair (colon-separated, `@`-terminated) that
    // GitGuardian's "Basic Auth String" detector would flag as a
    // committed credential.
    let (placeholder_user, placeholder_pass) = ("PLACEHOLDER_USER", "PLACEHOLDER_PASS");
    let with_creds = format!("http://{placeholder_user}:{placeholder_pass}@proxy.example/");
    let redacted = redact_proxy_userinfo(&with_creds);
    assert_eq!(redacted, "http://***@proxy.example/");
    assert!(!redacted.contains(placeholder_pass));
    assert!(!redacted.contains(placeholder_user));

    // User only (no password) — still redacted.
    let with_user_only = format!("https://{placeholder_user}@proxy.example:8080");
    let redacted = redact_proxy_userinfo(&with_user_only);
    assert_eq!(redacted, "https://***@proxy.example:8080");

    // No userinfo segment — pass through.
    let redacted = redact_proxy_userinfo("http://proxy.example:3128/");
    assert_eq!(redacted, "http://proxy.example:3128/");

    // `@` appears only in the path, not as userinfo separator —
    // must not be mistaken for credentials.
    let redacted = redact_proxy_userinfo("http://proxy.example/path@thing");
    assert_eq!(redacted, "http://proxy.example/path@thing");

    // Garbage input (no `://`) returned unchanged — the
    // surrounding warning log is the only caller and is already
    // handling the malformed-URL case.
    assert_eq!(redact_proxy_userinfo("not-a-url"), "not-a-url");
}

#[test]
fn redact_body_preview_masks_api_key_param() {
    let redacted = redact_body_preview("error api_key=sk-12345&other=val then TOKEN=second-secret");
    assert!(redacted.contains("api_key=***"), "redacted: {redacted}");
    assert!(redacted.contains("TOKEN=***"), "redacted: {redacted}");
    assert!(
        !redacted.contains("sk-12345") && !redacted.contains("second-secret"),
        "leaked: {redacted}"
    );
    assert!(
        redacted.contains("other=val"),
        "non-secret preserved: {redacted}"
    );
}

#[test]
fn reviewed_plugin_server_errors_suppress_arbitrary_details() {
    let auth = McpHttpAuth {
        suppress_server_error_details: true,
        ..Default::default()
    };
    assert_eq!(
        auth.server_error_preview("arbitrary credential value"),
        "<server details suppressed for reviewed plugin>"
    );

    let response = serde_json::json!({
        "error": { "message": "arbitrary credential value" }
    });
    let error = response_result(&response, "tools/call", true)
        .expect_err("reviewed plugin JSON-RPC error must be generic")
        .to_string();
    assert!(!error.contains("arbitrary credential value"));
    assert!(error.contains("details suppressed"));
}

#[test]
fn invalid_json_preview_collapses_lines_and_redacts_secrets() {
    let preview = invalid_json_preview(
        b"Authorization: Bearer PLACEHOLDER_TOKEN\nAllow connection? api_key=PLACEHOLDER_KEY",
    );

    assert!(
        preview.contains("Authorization: Bearer *** Allow connection? api_key=***"),
        "preview: {preview}"
    );
    assert!(
        !preview.contains('\n'),
        "preview should be single-line: {preview}"
    );
    assert!(
        !preview.contains("PLACEHOLDER_TOKEN") && !preview.contains("PLACEHOLDER_KEY"),
        "secret leaked: {preview}"
    );
}

/// #420: `StdioTransport::shutdown` reaps the child process by sending
/// SIGTERM and giving it a brief grace period before drop fires SIGKILL.
/// The test spawns `cat` (which exits immediately on stdin EOF / SIGTERM)
/// and verifies the transport tears down cleanly. Unix-only because
/// SIGTERM doesn't exist on Windows; on Windows the test would just
/// duplicate the kill_on_drop path.
#[cfg(unix)]
#[tokio::test]
async fn stdio_transport_shutdown_terminates_child() {
    let mut config = test_server_config();
    config.command = Some("cat".to_string());
    let mut transport = StdioTransport::spawn(
        "shutdown-test",
        "cat",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("spawn cat through the production broker");
    let pid = transport
        .session
        .child_for_tests()
        .lock()
        .await
        .id()
        .expect("child pid");

    // shutdown() should send SIGTERM and complete within the grace window.
    let start = std::time::Instant::now();
    transport.shutdown().await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < STDIO_SHUTDOWN_GRACE + Duration::from_millis(500),
        "shutdown blocked beyond grace window: {elapsed:?}"
    );

    // The child should be reaped — kill(pid, 0) returning ESRCH means
    // the pid is gone. If it's still alive, kill(0) returns 0, which
    // means our shutdown didn't terminate it.
    // SAFETY: pid was just collected from a tokio Child we spawned.
    // libc::kill with signal 0 only checks pid existence and is
    // async-signal-safe.
    let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    assert!(
        !still_alive,
        "child {pid} survived StdioTransport::shutdown — SIGTERM not delivered"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_transport_drop_allows_child_cleanup() {
    let directory = tempfile::tempdir().expect("temporary cleanup receipt");
    let receipt = directory.path().join("cleaned");
    let config: McpServerConfig = serde_json::from_value(serde_json::json!({
        "args": [
            "-c",
            "trap 'sleep 0.1; printf cleaned > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; exit 0' TERM; printf 'ready\\n'; while :; do sleep 0.05; done",
            "cleanup-test",
            receipt.display().to_string(),
        ],
    }))
    .unwrap();
    let mut transport = StdioTransport::spawn(
        "drop-cleanup-test",
        "/bin/sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("spawn cleanup fixture");
    assert_eq!(transport.recv().await.unwrap(), b"ready");
    // Retaining a strong Child reference would mask immediate kill_on_drop.
    drop(transport);
    tokio::time::timeout(STDIO_SHUTDOWN_GRACE + Duration::from_secs(1), async {
        while !receipt.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("dropped transport lets SIGTERM cleanup finish");
    assert_eq!(std::fs::read_to_string(receipt).unwrap(), "cleaned");
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_transport_drop_kills_child_that_ignores_cleanup() {
    let config: McpServerConfig = serde_json::from_value(serde_json::json!({
        "args": [
            "-c",
            "trap '' TERM; printf 'ready\\n'; while :; do sleep 0.05; done",
        ],
    }))
    .unwrap();
    let mut transport = StdioTransport::spawn(
        "drop-hung-test",
        "/bin/sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("spawn unresponsive fixture");
    assert_eq!(transport.recv().await.unwrap(), b"ready");
    let pid = transport
        .session
        .child_for_tests()
        .lock()
        .await
        .id()
        .expect("live child");
    drop(transport);
    tokio::time::timeout(STDIO_SHUTDOWN_GRACE + Duration::from_secs(1), async {
        // Signal zero only observes the process; the owned Child sends kills.
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("dropped transport force-kills and reaps hung child");
}

/// Mid-run MCP server crash: the v0.8.x spawn path used `Stdio::null` for
/// stderr, so a server that died with a useful stderr message left the
/// caller with only "Stdio transport closed". Now stderr is piped into a
/// bounded ring buffer and surfaced when the read side fails.
#[cfg(unix)]
#[tokio::test]
async fn stdio_transport_recv_error_includes_stderr_tail() {
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec![
        "-c".to_string(),
        "echo 'mcp-server: failed to load plugin' 1>&2; exit 1".to_string(),
    ];
    let mut transport = StdioTransport::spawn(
        "stderr-test",
        "sh",
        &config,
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("spawn sh through the production broker");

    // Give the subprocess time to write its stderr line and exit.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let err = transport
        .recv()
        .await
        .expect_err("expected transport closed error");
    let err_str = format!("{err}");
    assert!(
        err_str.contains("Stdio transport closed"),
        "missing closed marker in: {err_str}"
    );
    assert!(
        err_str.contains("mcp-server: failed to load plugin"),
        "stderr context missing from error: {err_str}"
    );
}

#[tokio::test]
async fn sse_connect_waits_for_endpoint_before_first_send() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let post_seen = Arc::new(AtomicBool::new(false));
    let server_post_seen = Arc::clone(&post_seen);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let server_cancel = cancel_token.clone();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let post_seen = Arc::clone(&server_post_seen);
            let server_cancel = server_cancel.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                if request.starts_with("GET /sse ") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    socket
                        .write_all(b"event: endpoint\ndata: /messages\n\n")
                        .await
                        .unwrap();
                    server_cancel.cancelled().await;
                } else if request.starts_with("POST /messages ") {
                    post_seen.store(true, AtomicOrdering::SeqCst);
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await
                        .unwrap();
                }
            });
        }
    });

    let url = format!("http://{addr}/sse");
    let client = test_mcp_http_client(&url);
    let mut transport =
        SseTransport::connect(client, url, cancel_token.clone(), Duration::from_secs(2))
            .await
            .unwrap();

    transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        })))
        .await
        .unwrap();

    assert!(
        post_seen.load(AtomicOrdering::SeqCst),
        "first SSE send should POST to the discovered endpoint"
    );

    cancel_token.cancel();
    server.abort();
}

#[tokio::test]
async fn sse_connect_accepts_crlf_endpoint_events() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let post_seen = Arc::new(AtomicBool::new(false));
    let server_post_seen = Arc::clone(&post_seen);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let server_cancel = cancel_token.clone();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let post_seen = Arc::clone(&server_post_seen);
            let server_cancel = server_cancel.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                if request.starts_with("GET /sse ") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await
                        .unwrap();
                    socket
                        .write_all(b"event: endpoint\r\ndata: /messages\r\n\r\n")
                        .await
                        .unwrap();
                    server_cancel.cancelled().await;
                } else if request.starts_with("POST /messages ") {
                    post_seen.store(true, AtomicOrdering::SeqCst);
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await
                        .unwrap();
                }
            });
        }
    });

    let url = format!("http://{addr}/sse");
    let client = test_mcp_http_client(&url);
    let mut transport =
        SseTransport::connect(client, url, cancel_token.clone(), Duration::from_secs(2))
            .await
            .unwrap();

    transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        })))
        .await
        .unwrap();

    assert!(
        post_seen.load(AtomicOrdering::SeqCst),
        "first SSE send should POST to the CRLF-discovered endpoint"
    );

    cancel_token.cancel();
    server.abort();
}

#[tokio::test]
async fn sse_transport_applies_custom_headers_to_get_and_post() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let get_header_seen = Arc::new(AtomicBool::new(false));
    let post_header_seen = Arc::new(AtomicBool::new(false));
    let server_get_header_seen = Arc::clone(&get_header_seen);
    let server_post_header_seen = Arc::clone(&post_header_seen);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let server_cancel = cancel_token.clone();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let get_header_seen = Arc::clone(&server_get_header_seen);
            let post_header_seen = Arc::clone(&server_post_header_seen);
            let server_cancel = server_cancel.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let request_lower = request.to_lowercase();
                if request.starts_with("GET /sse ") {
                    if request_lower.contains("x-custom-auth: my-test-token") {
                        get_header_seen.store(true, AtomicOrdering::SeqCst);
                    }
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await
                        .unwrap();
                    socket
                        .write_all(b"event: endpoint\ndata: /messages\n\n")
                        .await
                        .unwrap();
                    server_cancel.cancelled().await;
                } else if request.starts_with("POST /messages ") {
                    if request_lower.contains("x-custom-auth: my-test-token") {
                        post_header_seen.store(true, AtomicOrdering::SeqCst);
                    }
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await
                        .unwrap();
                }
            });
        }
    });

    let url = format!("http://{addr}/sse");
    let client = test_mcp_http_client(&url);
    let mut headers = HashMap::new();
    headers.insert("X-Custom-Auth".to_string(), "my-test-token".to_string());
    let mut transport = SseTransport::connect(
        client.with_mcp_auth(McpHttpAuth {
            headers,
            ..Default::default()
        }),
        url,
        cancel_token.clone(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();

    transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        })))
        .await
        .unwrap();

    assert!(
        get_header_seen.load(AtomicOrdering::SeqCst),
        "legacy SSE GET must include user-configured custom headers"
    );
    assert!(
        post_header_seen.load(AtomicOrdering::SeqCst),
        "legacy SSE POST must include user-configured custom headers"
    );

    cancel_token.cancel();
    server.abort();
}

#[tokio::test]
async fn sse_post_error_includes_response_body_excerpt() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let server_cancel = cancel_token.clone();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let server_cancel = server_cancel.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                if request.starts_with("GET /sse ") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await
                        .unwrap();
                    socket
                        .write_all(b"event: endpoint\ndata: /messages\n\n")
                        .await
                        .unwrap();
                    server_cancel.cancelled().await;
                } else if request.starts_with("POST /messages ") {
                    socket
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 25\r\n\r\n{\"error\":\"missing query\"}",
                        )
                        .await
                        .unwrap();
                }
            });
        }
    });

    let url = format!("http://{addr}/sse");
    let client = test_mcp_http_client(&url);
    let mut transport =
        SseTransport::connect(client, url, cancel_token.clone(), Duration::from_secs(2))
            .await
            .unwrap();

    let err = transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        })))
        .await
        .expect_err("POST rejection should be returned");
    let err = format!("{err:#}");
    assert!(
        err.contains("400 Bad Request") && err.contains("missing query"),
        "SSE POST error should include status and body, got: {err}"
    );

    cancel_token.cancel();
    server.abort();
}

#[tokio::test]
async fn streamable_http_caps_chunked_bodies_without_content_length() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Serve chunked responses (no Content-Length) of the requested size:
    // GET /over streams past the cap, GET /under stays below it.
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let total: usize = if request.starts_with("GET /over ") {
                    256
                } else {
                    16
                };
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
                    )
                    .await
                    .unwrap();
                let chunk = [b'x'; 32];
                let mut sent = 0;
                while sent < total {
                    let n = chunk.len().min(total - sent);
                    let frame = format!("{n:x}\r\n");
                    socket.write_all(frame.as_bytes()).await.unwrap();
                    socket.write_all(&chunk[..n]).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                    sent += n;
                }
                socket.write_all(b"0\r\n\r\n").await.unwrap();
                socket.flush().await.unwrap();
            });
        }
    });

    let client = test_http_client();
    let cap = 64;

    let over = client
        .get(format!("http://{addr}/over"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        over.content_length(),
        None,
        "chunked response must not declare a length for this test to be meaningful"
    );
    let err = streamable_http::read_body_capped(over, cap)
        .await
        .expect_err("a chunked body past the cap must fail, not OOM");
    assert!(
        err.to_string().contains("exceeds"),
        "unexpected error: {err}"
    );

    let under = client
        .get(format!("http://{addr}/under"))
        .send()
        .await
        .unwrap();
    let body = streamable_http::read_body_capped(under, cap)
        .await
        .expect("a chunked body under the cap reads fine");
    assert_eq!(body, "x".repeat(16));

    server.abort();
}

#[tokio::test]
async fn error_body_excerpt_stops_at_cap_without_waiting_for_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_cancel = tokio_util::sync::CancellationToken::new();
    let task_cancel = server_cancel.clone();

    // Deliberately omit the terminating zero-sized chunk and keep the socket
    // open. A `.text()`-based diagnostic would wait for EOF; the bounded
    // reader must return as soon as the first chunk reaches the cap.
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        socket
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n100\r\n",
            )
            .await
            .unwrap();
        socket.write_all(&[b'x'; 256]).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        socket.flush().await.unwrap();
        task_cancel.cancelled().await;
    });

    let response = test_http_client()
        .get(format!("http://{addr}/preview"))
        .send()
        .await
        .unwrap();
    let preview = tokio::time::timeout(Duration::from_secs(1), bounded_body_excerpt(response, 64))
        .await
        .expect("bounded excerpt must not wait for an attacker-controlled EOF");
    assert_eq!(preview, format!("{}…", "x".repeat(64)));

    server_cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn streamable_http_stale_session_reconnects_and_retries_tool_call() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn write_response(socket: &mut tokio::net::TcpStream, response: &[u8]) {
        socket.write_all(response).await.unwrap();
        socket.flush().await.unwrap();
        socket.shutdown().await.unwrap();
    }

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let get_count = Arc::new(AtomicUsize::new(0));
    let stale_seen = Arc::new(AtomicBool::new(false));
    let success_seen = Arc::new(AtomicBool::new(false));
    let server_get_count = Arc::clone(&get_count);
    let server_stale_seen = Arc::clone(&stale_seen);
    let server_success_seen = Arc::clone(&success_seen);

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let get_count = Arc::clone(&server_get_count);
            let stale_seen = Arc::clone(&server_stale_seen);
            let success_seen = Arc::clone(&server_success_seen);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                let header_end = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let body = &request[header_end..header_end + content_length];
                let session_header = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("mcp-session-id")
                        .then(|| value.trim().to_string())
                });

                if headers.starts_with("GET /mcp ") {
                    let count = get_count.fetch_add(1, AtomicOrdering::SeqCst);
                    let session = if count == 0 { "sess-old" } else { "sess-new" };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nMcp-Session-Id: {session}\r\nContent-Length: 0\r\n\r\n"
                    );
                    write_response(&mut socket, response.as_bytes()).await;
                    return;
                }

                let request_json: serde_json::Value = serde_json::from_slice(body).unwrap();
                let method = request_json
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let id = request_json
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("0"));

                if method == "tools/call" && session_header.as_deref() == Some("sess-old") {
                    stale_seen.store(true, AtomicOrdering::SeqCst);
                    write_response(
                        &mut socket,
                        b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"session expired\"}",
                    )
                    .await;
                    return;
                }

                let result = match method {
                    "initialize" => serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {}, "prompts": {}}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [
                            { "name": "search", "inputSchema": {} }
                        ]
                    }),
                    "resources/list" => serde_json::json!({ "resources": [] }),
                    "resources/templates/list" => {
                        serde_json::json!({ "resourceTemplates": [] })
                    }
                    "prompts/list" => serde_json::json!({ "prompts": [] }),
                    "tools/call" => {
                        assert_eq!(session_header.as_deref(), Some("sess-new"));
                        success_seen.store(true, AtomicOrdering::SeqCst);
                        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                    }
                    _ => {
                        write_response(
                            &mut socket,
                            b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await;
                        return;
                    }
                };
                let response_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                write_response(&mut socket, response.as_bytes()).await;
            });
        }
    });

    let mut cfg = McpConfig::default();
    cfg.servers.insert(
        "dephy".to_string(),
        McpServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some(format!("http://{addr}/mcp")),
            transport: None,
            connect_timeout: Some(10),
            execute_timeout: Some(10),
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            lazy: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        },
    );
    let mut pool = McpPool::new(cfg);

    let result = pool
        .call_tool("mcp_dephy_search", serde_json::json!({ "query": "dephy" }))
        .await
        .unwrap();

    assert_eq!(
        result,
        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
    assert!(stale_seen.load(AtomicOrdering::SeqCst));
    assert!(success_seen.load(AtomicOrdering::SeqCst));
    assert_eq!(get_count.load(AtomicOrdering::SeqCst), 2);

    server.abort();
}

#[tokio::test]
async fn legacy_sse_session_expiry_is_marked_stale() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        let header_end = loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buf[..n]);
            if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        assert!(headers.starts_with("POST /messages "));
        socket
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"session expired\"}",
            )
            .await
            .unwrap();
    });

    let (_sender, receiver) = mpsc::channel(1);
    let sse_task = tokio::spawn(async {});
    let mut transport = SseTransport {
        client: test_mcp_http_client(&format!("http://{addr}/sse")),
        base_url: format!("http://{addr}/sse"),
        endpoint_url: Some(format!("http://{addr}/messages")),
        receiver,
        sse_task,
    };

    let err = transport
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#.to_vec())
        .await
        .expect_err("expired SSE session should fail");

    assert!(
        is_mcp_stale_session_error(&err),
        "SSE session expiry should be retryable, got: {err:#}"
    );

    server.abort();
}

/// Read one HTTP/1.1 request from a legacy SSE test server socket.
async fn read_legacy_sse_http_request(
    socket: &mut tokio::net::TcpStream,
) -> (String, serde_json::Value) {
    let mut request = Vec::new();
    let mut buf = [0; 4096];
    let header_end = loop {
        let n = tokio::io::AsyncReadExt::read(socket, &mut buf)
            .await
            .unwrap();
        if n == 0 {
            return (String::new(), serde_json::Value::Null);
        }
        request.extend_from_slice(&buf[..n]);
        if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let n = tokio::io::AsyncReadExt::read(socket, &mut buf)
            .await
            .unwrap();
        if n == 0 {
            return (headers, serde_json::Value::Null);
        }
        request.extend_from_slice(&buf[..n]);
    }
    let body = &request[header_end..header_end + content_length];
    let json = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(body).unwrap()
    };
    (headers, json)
}

#[tokio::test]
async fn legacy_sse_closed_stream_reports_unknown_outcome_and_reconnects_without_replay() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    // A concurrent proxy fixture changes process-wide HTTP_PROXY/NO_PROXY.
    // Hold the environment guard before the loopback guard, as other MCP
    // tests do, so this server is always reached directly.
    let _env = crate::test_support::lock_test_env();
    let _lock = lock_mcp_loopback_tests().await;
    let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let active_sse = Arc::new(Mutex::new(None::<mpsc::UnboundedSender<Option<String>>>));
    let get_count = Arc::new(AtomicUsize::new(0));
    let tool_call_count = Arc::new(AtomicUsize::new(0));
    let success_seen = Arc::new(AtomicBool::new(false));
    let server_active_sse = Arc::clone(&active_sse);
    let server_get_count = Arc::clone(&get_count);
    let server_tool_call_count = Arc::clone(&tool_call_count);
    let server_success_seen = Arc::clone(&success_seen);

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let active_sse = Arc::clone(&server_active_sse);
            let get_count = Arc::clone(&server_get_count);
            let tool_call_count = Arc::clone(&server_tool_call_count);
            let success_seen = Arc::clone(&server_success_seen);
            tokio::spawn(async move {
                let (headers, request_json) = read_legacy_sse_http_request(&mut socket).await;
                if headers.starts_with("GET /sse ") {
                    get_count.fetch_add(1, AtomicOrdering::SeqCst);
                    let (tx, mut rx) = mpsc::unbounded_channel::<Option<String>>();
                    *active_sse.lock().unwrap() = Some(tx);
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await
                        .unwrap();
                    socket
                        .write_all(b"event: endpoint\ndata: /messages\n\n")
                        .await
                        .unwrap();
                    while let Some(message) = rx.recv().await {
                        let Some(message) = message else {
                            return;
                        };
                        let event = format!("event: message\ndata: {message}\n\n");
                        socket.write_all(event.as_bytes()).await.unwrap();
                    }
                    return;
                }

                if !headers.starts_with("POST /messages ") {
                    return;
                }

                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();

                let method = request_json
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if method == "notifications/initialized" {
                    return;
                }

                let id = request_json
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("0"));

                if method == "tools/call" {
                    let count = tool_call_count.fetch_add(1, AtomicOrdering::SeqCst);
                    if count == 0 {
                        if let Some(tx) = active_sse.lock().unwrap().take() {
                            let _ = tx.send(None);
                        }
                        return;
                    }
                }

                let result = match method {
                    "initialize" => serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {}, "prompts": {}}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [
                            { "name": "search", "inputSchema": {} }
                        ]
                    }),
                    "resources/list" => serde_json::json!({ "resources": [] }),
                    "resources/templates/list" => {
                        serde_json::json!({ "resourceTemplates": [] })
                    }
                    "prompts/list" => serde_json::json!({ "prompts": [] }),
                    "tools/call" => {
                        success_seen.store(true, AtomicOrdering::SeqCst);
                        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                    }
                    other => panic!("unexpected method: {other}"),
                };
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                })
                .to_string();
                // Deliver the response over the *current* SSE channel. The
                // retry tool call can race ahead of the reconnecting GET
                // /sse that re-stores the sender; under parallel load those
                // two server tasks are scheduled in either order, so wait
                // briefly for the channel instead of dropping the response
                // (which left the client hanging until timeout) (#2597).
                let send_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let tx = loop {
                    if let Some(tx) = active_sse.lock().unwrap().as_ref().cloned() {
                        break Some(tx);
                    }
                    if std::time::Instant::now() >= send_deadline {
                        break None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                if let Some(tx) = tx {
                    let _ = tx.send(Some(response));
                }
            });
        }
    });

    let mut cfg = McpConfig::default();
    cfg.servers.insert(
        "dephy".to_string(),
        McpServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some(format!("http://{addr}/sse")),
            transport: Some("sse".to_string()),
            connect_timeout: Some(10),
            execute_timeout: Some(10),
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            lazy: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        },
    );
    let mut pool = McpPool::new(cfg);

    // The server received the call and then closed the stream: it may have
    // run the tool, so the call must fail as unknown instead of replaying.
    let err = pool
        .call_tool("mcp_dephy_search", serde_json::json!({ "query": "dephy" }))
        .await
        .expect_err("a call whose transport closed mid-flight must not be replayed");
    assert!(
        format!("{err:#}").contains("outcome unknown, not retried"),
        "unexpected error: {err:#}"
    );
    assert_eq!(tool_call_count.load(AtomicOrdering::SeqCst), 1);
    assert!(!success_seen.load(AtomicOrdering::SeqCst));

    // The dead connection was dropped, so the next call reconnects.
    let result = pool
        .call_tool("mcp_dephy_search", serde_json::json!({ "query": "dephy" }))
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
    assert_eq!(tool_call_count.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(get_count.load(AtomicOrdering::SeqCst), 2);
    assert!(success_seen.load(AtomicOrdering::SeqCst));

    server.abort();
}

/// Servers without an explicit `transport = "sse"` reach legacy SSE through
/// the Streamable HTTP fallback, inside `HttpTransport`. An event stream that
/// closes while idle must read as not ready there too, so the next call
/// reconnects before it dispatches instead of losing the result.
#[tokio::test]
async fn fallback_sse_stream_closed_while_idle_reconnects_before_dispatch() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    let _env = crate::test_support::lock_test_env();
    let _lock = lock_mcp_loopback_tests().await;
    let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let active_sse = Arc::new(Mutex::new(None::<mpsc::UnboundedSender<Option<String>>>));
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let tool_call_count = Arc::new(AtomicUsize::new(0));
    let server_active_sse = Arc::clone(&active_sse);
    let server_initialize_count = Arc::clone(&initialize_count);
    let server_tool_call_count = Arc::clone(&tool_call_count);

    let server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let active_sse = Arc::clone(&server_active_sse);
            let initialize_count = Arc::clone(&server_initialize_count);
            let tool_call_count = Arc::clone(&server_tool_call_count);
            tokio::spawn(async move {
                let (headers, request_json) = read_legacy_sse_http_request(&mut socket).await;
                if headers.starts_with("GET /mcp ") {
                    // The session preflight and the fallback stream both land
                    // here; the later GET replaces the earlier stream.
                    let (tx, mut rx) = mpsc::unbounded_channel::<Option<String>>();
                    *active_sse.lock().unwrap() = Some(tx);
                    let opened = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: endpoint\ndata: /messages\n\n")
                        .await;
                    if opened.is_err() {
                        return;
                    }
                    while let Some(Some(message)) = rx.recv().await {
                        let event = format!("event: message\ndata: {message}\n\n");
                        if socket.write_all(event.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                    return;
                }
                if headers.starts_with("POST /mcp ") {
                    // No Streamable HTTP: the client falls back to legacy SSE.
                    let _ = socket
                        .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return;
                }
                if !headers.starts_with("POST /messages ") {
                    return;
                }
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
                let method = request_json
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let result = match method {
                    "notifications/initialized" => return,
                    "initialize" => {
                        initialize_count.fetch_add(1, AtomicOrdering::SeqCst);
                        serde_json::json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}}
                        })
                    }
                    "tools/list" => serde_json::json!({
                        "tools": [{ "name": "search", "inputSchema": {} }]
                    }),
                    "resources/list" => serde_json::json!({ "resources": [] }),
                    "resources/templates/list" => serde_json::json!({ "resourceTemplates": [] }),
                    "prompts/list" => serde_json::json!({ "prompts": [] }),
                    "tools/call" => {
                        tool_call_count.fetch_add(1, AtomicOrdering::SeqCst);
                        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                    }
                    other => panic!("unexpected method: {other}"),
                };
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request_json.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "result": result
                })
                .to_string();
                let tx = active_sse.lock().unwrap().as_ref().cloned();
                if let Some(tx) = tx {
                    let _ = tx.send(Some(response));
                }
            });
        }
    });

    let mut cfg = McpConfig::default();
    let mut server_config = test_server_config();
    server_config.command = None;
    server_config.url = Some(format!("http://{addr}/mcp"));
    server_config.connect_timeout = Some(10);
    server_config.execute_timeout = Some(10);
    cfg.servers.insert("fallback".to_string(), server_config);
    let mut pool = McpPool::new(cfg);
    let ok = serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] });

    let result = pool
        .call_tool("mcp_fallback_search", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result, ok);

    // The server ends the event stream while the connection is idle.
    let stream = active_sse
        .lock()
        .unwrap()
        .take()
        .expect("fallback stream open");
    stream.send(None).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while pool.connections["fallback"].is_transport_ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a closed fallback SSE stream must stop reading as ready"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The next call reconnects first, so it is dispatched once and answered.
    let result = pool
        .call_tool("mcp_fallback_search", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result, ok);
    assert_eq!(tool_call_count.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(initialize_count.load(AtomicOrdering::SeqCst), 2);

    server.abort();
}

#[test]
fn session_id_starts_none() {
    let transport = StreamableHttpTransport::new(
        test_mcp_http_client("https://example.invalid/mcp"),
        "https://example.invalid/mcp".to_string(),
    );
    assert!(transport.session_id.is_none());
}

/// Session ID captured from a POST response is replayed on the next POST.
#[tokio::test]
async fn session_id_captured_from_post_response_and_replayed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.starts_with("POST "), "expected POST, got: {req}");

        // First POST: return a session ID so the transport captures it.
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-abc-123\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await
            .unwrap();
        socket.flush().await.unwrap();

        // Read the second POST — should contain the session ID.
        let mut buf2 = [0u8; 4096];
        let n2 = socket.read(&mut buf2).await.unwrap();
        let req2 = String::from_utf8_lossy(&buf2[..n2]);
        // reqwest lower-cases header names.
        let req2_lower = req2.to_lowercase();
        assert!(
            req2_lower.contains("mcp-session-id: sess-abc-123"),
            "second POST must replay captured session ID, got:\n{req2}"
        );

        socket
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/mcp");
    let mut transport = StreamableHttpTransport::new(test_mcp_http_client(&url), url);

    // First send: server returns Mcp-Session-Id.
    transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "initialize",
            "params": {}
        })))
        .await
        .unwrap();
    assert_eq!(
        transport.session_id.as_deref(),
        Some("sess-abc-123"),
        "session ID should be captured from response"
    );

    // Second send: should replay the session ID.
    transport
        .send(json_frame(serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "tools/list",
            "params": {}
        })))
        .await
        .unwrap();

    server.abort();
}

/// Custom headers configured in McpServerConfig are applied to the GET
/// preflight so servers that require auth on session-establishment GET
/// (e.g. Hindsight, #1629) can authenticate it.
#[tokio::test]
async fn custom_headers_applied_to_get_preflight() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Lock order is env first, then loopback — the OAuth pool tests take
    // them in that order, and inverting them deadlocks the suite.
    let _env = crate::test_support::lock_test_env();
    let _lock = lock_mcp_loopback_tests().await;
    // The fixture client honors an operator-configured proxy; pin loopback
    // out of any ambient proxy so a concurrent proxy-configuring test can
    // never route this GET away from the fixture server.
    let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // The test signals success by writing to this flag — the GET handler
    // sets it when it sees the expected header.
    let header_seen = Arc::new(AtomicBool::new(false));
    let header_seen_srv = Arc::clone(&header_seen);

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]);

        // reqwest lower-cases header names.
        if req.starts_with("GET ") && req.to_lowercase().contains("x-custom-auth: my-test-token") {
            header_seen_srv.store(true, AtomicOrdering::SeqCst);
        }

        socket
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/mcp");
    let mut headers = HashMap::new();
    headers.insert("X-Custom-Auth".to_string(), "my-test-token".to_string());

    let mut transport = HttpTransport::new(
        test_mcp_http_client(&url).with_mcp_auth(McpHttpAuth {
            headers,
            ..Default::default()
        }),
        url,
        tokio_util::sync::CancellationToken::new(),
        Duration::from_secs(10),
    );

    transport.try_establish_session().await.unwrap();

    server.abort();

    assert!(
        header_seen.load(AtomicOrdering::SeqCst),
        "GET preflight must include user-configured custom headers"
    );
}

// === add_runtime_server_config conflict tests ===

#[test]
fn add_runtime_server_config_rejects_static_conflict() {
    let config: McpConfig = serde_json::from_str(
        r#"{
        "servers": {
            "existing": {"command": "node server.js"}
        }
    }"#,
    )
    .unwrap();
    let pool = McpPool::new(config);

    let err = pool
        .add_runtime_server_config(
            "existing".to_string(),
            serde_json::from_str(r#"{"command": "npx other"}"#).unwrap(),
        )
        .unwrap_err();
    assert!(err.contains("already exists in the config file"));
}

#[test]
fn add_runtime_server_config_rejects_dynamic_duplicate() {
    let pool = McpPool::new(McpConfig::default());

    pool.add_runtime_server_config(
        "my_server".to_string(),
        serde_json::from_str(r#"{"command": "node a.js"}"#).unwrap(),
    )
    .unwrap();

    let err = pool
        .add_runtime_server_config(
            "my_server".to_string(),
            serde_json::from_str(r#"{"command": "node b.js"}"#).unwrap(),
        )
        .unwrap_err();
    assert!(err.contains("already started earlier"));
}

#[test]
fn add_runtime_server_config_accepts_new_name() {
    let pool = McpPool::new(McpConfig::default());

    pool.add_runtime_server_config(
        "brand_new".to_string(),
        serde_json::from_str(r#"{"command": "node x.js"}"#).unwrap(),
    )
    .unwrap();
}

/// Server attribution and the model-facing tool name must come from one
/// definition. If they ever drift, a human reading tool provenance would be
/// told which server owns a name the model never saw.
#[test]
fn mcp_model_tool_names_and_server_attribution_share_one_definition() {
    assert_eq!(
        McpPool::mcp_model_tool_name("files", "read"),
        "mcp_files_read"
    );
    // A server name containing `_` is exactly why the reverse split is a guess.
    assert_eq!(
        McpPool::mcp_model_tool_name("my_server", "read_file"),
        "mcp_my_server_read_file"
    );

    let resolved =
        McpPool::resolve_tool_server_map([("files", "read"), ("git", "status")].into_iter());
    assert_eq!(
        resolved.get("mcp_files_read").map(String::as_str),
        Some("files")
    );
    assert_eq!(
        resolved.get("mcp_git_status").map(String::as_str),
        Some("git")
    );

    // Ambiguity: two servers collapse onto the same model name. Neither wins,
    // so the name resolves to no server and callers report it as unknown —
    // the same rule `all_tools` applies when it hides the ambiguous tool.
    let ambiguous = McpPool::resolve_tool_server_map(
        [("a_b", "c"), ("a", "b_c"), ("solo", "tool")].into_iter(),
    );
    assert!(
        !ambiguous.contains_key("mcp_a_b_c"),
        "an ambiguous model name must resolve to no server"
    );
    assert_eq!(
        ambiguous.get("mcp_solo_tool").map(String::as_str),
        Some("solo")
    );
}

#[test]
fn removed_runtime_server_config_can_be_retried_with_same_name() {
    let mut pool = McpPool::new(McpConfig::default());
    let config: McpServerConfig = serde_json::from_str(r#"{"command": "node a.js"}"#).unwrap();

    pool.add_runtime_server_config("retryable".to_string(), config.clone())
        .unwrap();
    pool.remove_runtime_server_config("retryable");
    pool.add_runtime_server_config("retryable".to_string(), config)
        .expect("rollback must release the deterministic runtime name");
}

#[tokio::test]
async fn mcp_initialize_sends_empty_client_capabilities_and_accepts_2025_11_25() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-11-25",
                "serverInfo": {"name": "current-sdk", "version": "1.0.0"},
                "capabilities": {"tools": {}}
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));

    conn.initialize()
        .await
        .expect("a 2025-11-25 server must complete the handshake");

    let sent = sent.lock().unwrap();
    let initialize = sent
        .iter()
        .find(|message| message["method"] == "initialize")
        .expect("initialize sent");
    assert_eq!(
        initialize["params"],
        serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {"name": "codewhale-tui", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {}
        }),
        "client capabilities must not declare server-side tools/resources/prompts"
    );
}

#[tokio::test]
async fn mcp_initialize_rejection_from_expired_aws_sso_names_the_login_command() {
    let transport = ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32602,
                "message": "Error retrieving credentials: The SSO session associated with this profile has expired or is otherwise invalid."
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.config.args = vec![
        "mcp-proxy-for-aws@1.6.4".to_string(),
        "--profile".to_string(),
        "work-sso".to_string(),
    ];

    let error = format!("{:#}", conn.initialize().await.expect_err("rejected"));
    assert!(
        error.contains(
            "run `aws sso login --profile work-sso` in a terminal, then `/mcp retry mock`"
        ),
        "{error}"
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some(&error), false),
        Some(McpRecoveryKind::AwsLogin)
    );
}

#[test]
fn mcp_recovery_kind_routes_expired_aws_credentials_to_external_login() {
    for stderr in [
        "The SSO session associated with this profile has expired or is otherwise invalid. To refresh this SSO session run aws sso login with the corresponding profile.",
        "Error loading SSO Token: Token for my-sso does not exist",
        "An error occurred (ExpiredTokenException) when calling the GetCallerIdentity operation: The security token included in the request is expired",
        // An expired AWS token that also says 401/Unauthorized must not be
        // routed to `/mcp login`, an OAuth flow that cannot renew it.
        "UnauthorizedException (401): AWS token has expired",
    ] {
        assert_eq!(
            mcp_recovery_kind(true, true, false, Some(stderr), false),
            Some(McpRecoveryKind::AwsLogin),
            "{stderr}"
        );
    }
    // Unrelated text that merely shares substrings stays where it was.
    for (stderr, expected) in [
        ("processor token invalid", McpRecoveryKind::Diagnose),
        ("401 Unauthorized", McpRecoveryKind::Reauth),
        (
            "oauth token expired; invalid_grant",
            McpRecoveryKind::Reauth,
        ),
        (
            "laws of token expiry were expired",
            McpRecoveryKind::Diagnose,
        ),
    ] {
        assert_eq!(
            mcp_recovery_kind(true, true, false, Some(stderr), false),
            Some(expected),
            "{stderr}"
        );
    }
    assert_eq!(
        McpRecoveryKind::AwsLogin.slash_command("aws"),
        "/mcp retry aws"
    );
    assert_eq!(
        McpRecoveryKind::AwsLogin.slash_command("name with spaces"),
        "/mcp reload"
    );

    let mut config = test_server_config();
    assert_eq!(
        aws_login_hint(&config, "aws", ""),
        "AWS credentials expired: run `aws sso login` in a terminal, then `/mcp retry aws`"
    );
    config
        .env
        .insert("AWS_PROFILE".to_string(), "from-env".to_string());
    assert!(aws_login_hint(&config, "aws", "").contains("aws sso login --profile from-env"));
    config.args = vec!["--profile=from-arg".to_string()];
    assert!(aws_login_hint(&config, "aws", "").contains("aws sso login --profile from-arg"));
    // A profile the shell could misread is left out rather than quoted.
    config.args = vec!["--profile".to_string(), "x; rm -rf ~".to_string()];
    config.env.clear();
    assert!(aws_login_hint(&config, "aws", "").contains("run `aws sso login` in"));
    config.args = vec!["--profile=--no-sign-request".to_string()];
    assert!(aws_login_hint(&config, "aws", "").contains("run `aws sso login` in"));
}

/// Review of #6789: the AWS-before-OAuth ordering lived only in the free
/// classifier, while the panel read the typed needs-auth flag first. A stdio
/// AWS proxy whose token expired with `401`/`Unauthorized` wording must not
/// enter the needs-auth set, must not read `auth_required`, and its row must
/// route to the in-place retry with the login command in the detail.
#[test]
fn expired_aws_login_with_401_wording_never_reaches_oauth_needs_auth() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = McpConfig::default();
    let mut server = test_server_config();
    server.args = vec!["--profile".to_string(), "work".to_string()];
    config.servers.insert("aws".to_string(), server);
    let mut pool = McpPool::new(config);

    // Not the initialize branch: no hint was appended to this error.
    let error = anyhow::anyhow!("UnauthorizedException (401): AWS token has expired");
    pool.note_connect_failure("aws", &error);
    assert!(!pool.server_needs_auth("aws"));

    let errors = HashMap::from([("aws".to_string(), format_mcp_error_for_display(&error))]);
    let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &errors);
    let aws = snapshot
        .servers
        .iter()
        .find(|server| server.name == "aws")
        .expect("aws in snapshot");
    assert!(!aws.auth_required, "{aws:?}");
    let recovery = aws
        .recovery_kind(false)
        .expect("a failed server has a recovery");
    assert_eq!(recovery, McpRecoveryKind::AwsLogin);
    assert_eq!(recovery.slash_command("aws"), "/mcp retry aws");
    // Labelled for what it runs, not "re-auth".
    assert_eq!(
        recovery.label_key(),
        codewhale_localization::MessageId::ExtensionsActionReconnect
    );
    let detail = aws.error.as_deref().unwrap_or_default();
    assert!(
        detail.contains("run `aws sso login --profile work` in a terminal, then `/mcp retry aws`"),
        "{detail}"
    );

    // A genuine OAuth 401 on the same pool still enters the needs-auth set.
    pool.note_connect_failure("aws", &anyhow::anyhow!("HTTP 401 Unauthorized"));
    assert!(pool.server_needs_auth("aws"));
}

#[tokio::test]
async fn live_aws_expiry_retires_the_catalog_and_lists_external_recovery() {
    for method in ["tools/call", "resources/read", "prompts/get"] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = McpConfig::default();
        let server = test_server_config();
        config.servers.insert("aws".to_string(), server.clone());
        let mut pool = McpPool::new(config);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut connection = test_connection(Box::new(ScriptedValueTransport {
            sent: Arc::clone(&sent),
            responses: VecDeque::from([json_frame(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32000, "message": "UnauthorizedException (401): AWS token has expired" }
            }))]),
        }));
        connection.name = "aws".to_string();
        connection.config = server;
        connection.catalog_generation = pool.current_catalog_generation();
        connection.tools.push(McpTool {
            name: "lookup".to_string(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        });
        connection.resources.push(McpResource {
            uri: "aws://example".to_string(),
            name: "example".to_string(),
            description: None,
            mime_type: None,
        });
        connection.prompts.push(McpPrompt {
            name: "lookup".to_string(),
            description: None,
            arguments: Vec::new(),
        });
        pool.store_ready_connection("aws".to_string(), connection)
            .unwrap();

        let result = match method {
            "tools/call" => {
                pool.call_tool("mcp_aws_lookup", serde_json::json!({}))
                    .await
            }
            "resources/read" => pool.read_resource("aws", "aws://example").await,
            "prompts/get" => {
                pool.get_prompt("aws", "lookup", serde_json::json!({}))
                    .await
            }
            _ => unreachable!(),
        };
        let error = result.expect_err("expired AWS credentials fail the call");
        assert!(format!("{error:#}").contains("aws sso login"));
        assert_eq!(sent.lock().unwrap().len(), 1, "never replay the call");
        assert!(!pool.connected_servers().contains(&"aws"));
        assert!(!pool.server_needs_auth("aws"));
        assert!(
            pool.to_api_tools()
                .iter()
                .all(|tool| !tool.name.starts_with("mcp_aws_"))
        );

        // No external boot error map: the latest pool failure owns this row.
        let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &HashMap::new());
        let row = &snapshot.servers[0];
        assert!(!row.connected && !row.auth_required);
        assert_eq!(row.recovery_kind(false), Some(McpRecoveryKind::AwsLogin));
        assert!(
            row.error
                .as_deref()
                .unwrap_or_default()
                .contains("aws sso login")
        );

        // Backoff reuses the recorded failure, so neither listing spawns a
        // process or logs in. Both model surfaces name the external recovery.
        for items in [
            pool.list_resources(None).await.unwrap(),
            pool.list_resource_templates(None).await.unwrap(),
        ] {
            let item = items
                .iter()
                .find(|item| item["server"] == "aws")
                .expect("AWS failure item");
            assert_eq!(item["error"], "aws_login_required");
            assert!(item.get("authenticate_tool").is_none());
            let message = item["message"].as_str().unwrap();
            assert!(message.contains("aws sso login") && message.contains("/mcp retry aws"));
            assert!(!message.contains("/mcp login"));
        }
    }
}

#[test]
fn sso_wording_on_an_oauth_capable_server_stays_on_the_oauth_route() {
    // Corporate SSO in front of an OAuth HTTP server: `aws sso login` cannot
    // help it, `/mcp login` can.
    assert!(!mcp_error_is_aws_login(
        "401 Unauthorized: SSO token expired",
        true
    ));
    assert_eq!(
        mcp_recovery_kind(
            true,
            true,
            false,
            Some("401 Unauthorized: SSO token expired"),
            true
        ),
        Some(McpRecoveryKind::Reauth)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some("SSO token expired"), true),
        Some(McpRecoveryKind::Diagnose)
    );
    // The same text from a stdio server is still the AWS route.
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some("SSO token expired"), false),
        Some(McpRecoveryKind::AwsLogin)
    );

    let dir = tempfile::tempdir().unwrap();
    let mut config = McpConfig::default();
    let mut server = test_server_config();
    server.command = None;
    server.url = Some("https://mcp.example.test/mcp".to_string());
    // Automatic OAuth discovery works without explicit scopes/client config.
    assert!(mcp_server_oauth_capable(&server));
    config.servers.insert("corp".to_string(), server);
    let mut pool = McpPool::new(config);
    pool.note_connect_failure(
        "corp",
        &anyhow::anyhow!("401 Unauthorized: SSO token expired"),
    );
    assert!(pool.server_needs_auth("corp"));
    let errors = HashMap::new();
    let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &errors);
    let corp = &snapshot.servers[0];
    assert!(corp.auth_required);
    assert_eq!(corp.recovery_kind(true), Some(McpRecoveryKind::Reauth));
}

/// Review of #6789: a reviewed plugin's initialize/live error is suppressed to
/// protect environment-backed credentials, which also hid the AWS wording
/// the recovery keys on. The classification now runs on the raw text first
/// and only our fixed hint survives — never the server's own words, and
/// never the plugin's `AWS_PROFILE` env value.
#[tokio::test]
async fn reviewed_plugin_rejection_keeps_aws_recovery_but_not_server_text() {
    let dir = tempfile::tempdir().unwrap();
    let plugins_root = dir.path().join("plugins");
    let plugin_base = plugins_root.join("aws-guard");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::create_dir_all(dir.path().join("project")).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"aws-guard\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace: dir.path().join("project"),
        user_plugins_dir: plugins_root,
        workspace_plugins_dir: dir.path().join("workspace-plugins-unused"),
        builtin_plugin_dirs: Vec::new(),
        state_path: dir.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("aws-guard").unwrap();
    registry.enable("aws-guard").unwrap();
    let authority = registry.authority_for("aws-guard").unwrap();

    let mut config = test_server_config();
    config
        .env
        .insert("AWS_PROFILE".to_string(), "env-profile-secret".to_string());
    config.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            None,
            Arc::new(crate::plugins::HostEnvironment::capture()),
        )
        .unwrap(),
    );

    for initialize in [true, false] {
        for (server_error, login) in [
            (
                "Error retrieving credentials for acct-SECRET-123: The SSO session associated with this profile has expired or is otherwise invalid.",
                "aws sso login",
            ),
            (
                "acct-SECRET-123 LoginRefreshRequired: Please reauthenticate using aws login",
                "aws login",
            ),
        ] {
            let transport = ScriptedValueTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                responses: VecDeque::from([json_frame(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": { "code": -32602, "message": server_error }
                }))]),
            };
            let mut conn = test_connection(Box::new(transport));
            conn.config = config.clone();
            let error = if initialize {
                conn.initialize().await.expect_err("initialize rejected")
            } else {
                conn.call_method("tools/call", serde_json::json!({}), 5)
                    .await
                    .expect_err("live call rejected")
            };
            let error = format!("{error:#}");
            assert!(error.contains("server details suppressed"), "{error}");
            assert!(!error.contains("acct-SECRET-123"), "{error}");
            assert!(!error.contains("env-profile-secret"), "{error}");
            let hint = format!(
                "AWS credentials expired: run `{login}` in a terminal, then `/mcp retry mock`"
            );
            assert!(error.contains(&hint), "{error}");
            assert_eq!(
                mcp_recovery_kind(true, true, false, Some(&error), false),
                Some(McpRecoveryKind::AwsLogin)
            );
            // Pool/snapshot/listing classification sees only sanitized text;
            // it must preserve aws login versus aws sso login on that pass.
            assert_eq!(aws_login_hint(&config, "mock", &error), hint);
        }
    }
}

#[test]
fn mcp_recovery_kind_names_real_login_and_reload_commands() {
    assert_eq!(
        mcp_recovery_kind(false, true, false, None, false),
        Some(McpRecoveryKind::Enable)
    );
    assert_eq!(
        mcp_recovery_kind(true, false, false, None, false),
        Some(McpRecoveryKind::Connect)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some("connection refused"), false),
        Some(McpRecoveryKind::Diagnose)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some("connection refused"), true),
        Some(McpRecoveryKind::Diagnose)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, Some("401 Unauthorized"), true),
        Some(McpRecoveryKind::Reauth)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, None, true),
        Some(McpRecoveryKind::Reauth)
    );
    assert_eq!(
        mcp_recovery_kind(true, true, false, None, false),
        Some(McpRecoveryKind::Reconnect)
    );
    // Enabled, inspected, connected, no error: nothing to recover.
    assert_eq!(mcp_recovery_kind(true, true, true, None, false), None);

    assert_eq!(
        McpRecoveryKind::Reauth.slash_command("github"),
        "/mcp login github"
    );
    // One row, one server. A `[reconnect] github` row that reloads every
    // configured server is not the action it named.
    assert_eq!(
        McpRecoveryKind::Connect.slash_command("github"),
        "/mcp retry github"
    );
    assert_eq!(
        McpRecoveryKind::Reconnect.slash_command("github"),
        "/mcp retry github"
    );
    // A name the command line cannot carry safely falls back to the blunt
    // reload rather than emitting an argument that would not survive parsing.
    assert_eq!(
        McpRecoveryKind::Reconnect.slash_command("name with spaces"),
        "/mcp reload"
    );
    assert_eq!(
        McpRecoveryKind::Diagnose.slash_command("github"),
        "/mcp validate github"
    );
    assert_eq!(
        McpRecoveryKind::Diagnose.slash_command("name with spaces"),
        "/mcp validate"
    );
    assert!(
        !McpRecoveryKind::Reauth
            .slash_command("github")
            .contains("/mcp auth")
    );
    assert!(mcp_name_is_command_safe("github"));
    assert!(!mcp_name_is_command_safe("github mcp"));
}

// === Synthetic self-serve OAuth authenticate tool (agent self-serve auth) ===
//
// One loopback origin serving both the MCP endpoint and the OAuth
// authorization-server APIs the login/refresh flows need:
// `/.well-known/oauth-authorization-server*` metadata, RFC 7591 `/register`,
// and a `/token` endpoint that rejects the `rt-stale` refresh token with
// `invalid_grant` and accepts everything else. The MCP endpoint 401s until
// the request carries `Authorization: Bearer cw-test-access`.

struct OAuthMcpMock {
    addr: std::net::SocketAddr,
    token_requests: Arc<AtomicUsize>,
    frames: Arc<Mutex<Vec<Value>>>,
    /// When set, the provider has revoked every grant: `/mcp` 401s even with
    /// the previously accepted bearer and `/token` rejects every refresh
    /// with `invalid_grant` — a mid-session revocation.
    revoked: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl OAuthMcpMock {
    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn revoke_all_grants(&self) {
        self.revoked.store(true, AtomicOrdering::SeqCst);
    }

    async fn spawn() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        async fn read_request(socket: &mut TcpStream) -> String {
            let mut request = Vec::new();
            let mut buf = [0; 2048];
            let header_end = loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before headers completed");
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let total_len = header_end + content_length;
            while request.len() < total_len {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before body completed");
                request.extend_from_slice(&buf[..n]);
            }
            String::from_utf8(request).unwrap()
        }

        async fn write_json(socket: &mut TcpStream, status: &str, body: &str) {
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }

        async fn write_empty(socket: &mut TcpStream, status: &str) {
            socket
                .write_all(
                    format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
        }

        async fn write_mcp_sse(
            socket: &mut TcpStream,
            id: serde_json::Value,
            result: serde_json::Value,
        ) {
            let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
            let body = format!("event: message\ndata: {payload}\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token_requests = Arc::new(AtomicUsize::new(0));
        let server_token_requests = Arc::clone(&token_requests);
        let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server_revoked = Arc::clone(&revoked);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let server_frames = Arc::clone(&frames);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let token_requests = Arc::clone(&server_token_requests);
                let revoked = Arc::clone(&server_revoked);
                let frames = Arc::clone(&server_frames);
                tokio::spawn(async move {
                    let request = read_request(&mut socket).await;
                    let first_line = request.lines().next().unwrap_or("").to_string();
                    let mut parts = first_line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let path_only = path.split('?').next().unwrap_or("").to_string();
                    let body = request.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                    let revoked = revoked.load(AtomicOrdering::SeqCst);
                    let authorized = !revoked
                        && request
                            .to_ascii_lowercase()
                            .contains("authorization: bearer cw-test-access");

                    // RFC 8414: the authorization server lives at the origin
                    // root, so it publishes its metadata only at the canonical
                    // `/.well-known/oauth-authorization-server` and its
                    // `issuer` is the root origin. The path-insertion
                    // candidates rmcp probes first (`.../oauth-authorization-server/mcp`)
                    // belong to a *different* issuer and must 404 here: rmcp
                    // 3.2 validates the discovered `issuer` against the
                    // discovery URL and rejects metadata served at the wrong
                    // one.
                    if method == "GET" && path_only == "/.well-known/oauth-authorization-server" {
                        let metadata = serde_json::json!({
                            "issuer": format!("http://{addr}"),
                            "authorization_endpoint": format!("http://{addr}/authorize"),
                            "token_endpoint": format!("http://{addr}/token"),
                            "registration_endpoint": format!("http://{addr}/register"),
                            "response_types_supported": ["code"],
                        });
                        write_json(&mut socket, "200 OK", &metadata.to_string()).await;
                    } else if method == "GET" && path_only.starts_with("/.well-known/") {
                        write_empty(&mut socket, "404 Not Found").await;
                    } else if method == "POST" && path_only == "/register" {
                        write_json(
                            &mut socket,
                            "200 OK",
                            r#"{"client_id":"cw-test-client","redirect_uris":[]}"#,
                        )
                        .await;
                    } else if method == "POST" && path_only == "/token" {
                        token_requests.fetch_add(1, AtomicOrdering::SeqCst);
                        if revoked || body.contains("refresh_token=rt-stale") {
                            write_json(
                                &mut socket,
                                "400 Bad Request",
                                r#"{"error":"invalid_grant","error_description":"stale grant"}"#,
                            )
                            .await;
                        } else if body.contains("refresh_token=rt-rotated") {
                            write_json(
                                &mut socket,
                                "200 OK",
                                r#"{"access_token":"cw-rotated-access-2","token_type":"Bearer","expires_in":3600,"refresh_token":"rt-rotated-2"}"#,
                            )
                            .await;
                        } else {
                            write_json(
                                &mut socket,
                                "200 OK",
                                r#"{"access_token":"cw-test-access","token_type":"Bearer","expires_in":3600,"refresh_token":"rt-fresh"}"#,
                            )
                            .await;
                        }
                    } else if path_only == "/mcp" {
                        // The Streamable HTTP session preflight is a bodyless
                        // GET; the real protocol starts at POST. 405 is the
                        // spec-shaped refusal and never reaches the parser.
                        if method == "GET" {
                            write_empty(&mut socket, "405 Method Not Allowed").await;
                            return;
                        }
                        let frame: Value = serde_json::from_str(&body).unwrap();
                        frames.lock().unwrap().push(frame);
                        if !authorized {
                            write_empty(&mut socket, "401 Unauthorized").await;
                            return;
                        }
                        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
                        let rpc_method = value["method"].as_str().unwrap_or("");
                        if rpc_method == "notifications/initialized" {
                            write_empty(&mut socket, "202 Accepted").await;
                            return;
                        }
                        let id = value["id"].clone();
                        let result = match rpc_method {
                            "initialize" => serde_json::json!({
                                "protocolVersion": "2024-11-05",
                                "serverInfo": {"name": "mock-oauth", "version": "1.0.0"},
                                "capabilities": {"tools": {}}
                            }),
                            "tools/list" => serde_json::json!({
                                "tools": [{
                                    "name": "wiki_lookup",
                                    "description": "Look up a wiki page",
                                    "inputSchema": {"type": "object"}
                                }]
                            }),
                            _ => serde_json::json!({}),
                        };
                        write_mcp_sse(&mut socket, id, result).await;
                    } else {
                        write_empty(&mut socket, "404 Not Found").await;
                    }
                });
            }
        });
        OAuthMcpMock {
            addr,
            token_requests,
            frames,
            revoked,
            task,
        }
    }
}

fn mock_oauth_server_config(addr: std::net::SocketAddr) -> McpServerConfig {
    let mut config = test_server_config();
    config.command = None;
    config.url = Some(format!("http://{addr}/mcp"));
    config
}

fn seed_oauth_tokens(
    server_name: &str,
    url: &str,
    access_token: &str,
    refresh_token: &str,
    expires_at: Option<u64>,
) {
    let tokens: oauth::StoredMcpOAuthTokens = serde_json::from_value(serde_json::json!({
        "server_name": server_name,
        "url": url,
        "client_id": "cw-test-client",
        "token_response": {
            "access_token": access_token,
            "token_type": "Bearer",
            "refresh_token": refresh_token,
            "expires_in": 3600
        },
        "expires_at": expires_at
    }))
    .unwrap();
    oauth::save_oauth_tokens(&tokens).unwrap();
}

fn millis_from_now(offset_ms: u64) -> u64 {
    (std::time::SystemTime::now() + Duration::from_millis(offset_ms))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn model_reconnect_reuses_configured_credentials_without_restarting_siblings() {
    use crate::tools::runtime_mcp::StartRuntimeMcpServer;
    use crate::tools::spec::{ToolContext, ToolSpec};

    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;
    let mock = OAuthMcpMock::spawn().await;
    let name = "existing_server";
    let mut config = McpConfig::default();
    config
        .servers
        .insert(name.into(), mock_oauth_server_config(mock.addr));
    config
        .servers
        .insert("healthy".into(), mock_oauth_server_config(mock.addr));
    seed_oauth_tokens(
        "healthy",
        &mock.url(),
        "cw-test-access",
        "rt-fresh",
        Some(millis_from_now(3_600_000)),
    );
    let mut pool = McpPool::new(config);
    pool.get_or_connect("healthy").await.unwrap();
    let sibling_cancel = pool.connections["healthy"].cancel_token.clone();
    assert!(
        pool.get_or_connect(name).await.is_err(),
        "boot before login must require auth"
    );
    let pool = Arc::new(tokio::sync::Mutex::new(pool));
    let tool = StartRuntimeMcpServer::new(Arc::clone(&pool));
    let mut context = ToolContext::new(dir.path());

    // Credentials arrive from the separate login process under the original key.
    seed_oauth_tokens(
        name,
        &mock.url(),
        "cw-test-access",
        "rt-fresh",
        Some(millis_from_now(3_600_000)),
    );
    context.disallowed_tools = vec![format!("mcp_{name}_*")];
    assert!(
        tool.execute(serde_json::json!({"name": name}), &context)
            .await
            .is_err()
    );
    assert!(!pool.lock().await.connected_servers().contains(&name));
    context.disallowed_tools.clear();
    let result = tool
        .execute(serde_json::json!({"name": name}), &context)
        .await
        .unwrap();
    assert_eq!(
        result.metadata,
        Some(serde_json::json!({"mcp_catalog_changed": true}))
    );
    let mut lock = pool.lock().await;
    assert!(lock.connected_servers().contains(&name));
    assert!(
        lock.all_tools()
            .iter()
            .any(|(tool, _)| tool == "mcp_existing_server_wiki_lookup")
    );
    assert!(
        lock.dynamic_servers.read().is_empty(),
        "reconnect cannot add an alias"
    );
    assert!(
        !sibling_cancel.is_cancelled(),
        "healthy sibling was restarted"
    );
    lock.call_tool("mcp_existing_server_wiki_lookup", serde_json::json!({}))
        .await
        .unwrap();
    drop(lock);
    assert!(
        tool.execute(serde_json::json!({"name": "absent"}), &context)
            .await
            .is_err()
    );
    assert!(pool.lock().await.dynamic_servers.read().is_empty());
    assert!(
        oauth::load_oauth_tokens("existing-server", &mock.url())
            .unwrap()
            .is_none(),
        "name must not be sanitized into another credential key"
    );
    mock.task.abort();
}

#[tokio::test]
async fn needs_auth_server_advertises_synthetic_authenticate_tool() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;
    let mock = OAuthMcpMock::spawn().await;

    let mut mcp_config = McpConfig::default();
    mcp_config.servers.insert(
        "wikiserver".to_string(),
        mock_oauth_server_config(mock.addr),
    );
    // A server carrying manual bearer configuration that also 401s must not
    // get the OAuth tool: the flow is gated to servers OAuth can serve.
    let mut bearer_config = mock_oauth_server_config(mock.addr);
    bearer_config.bearer_token_env_var = Some("CW_TEST_UNSET_BEARER".to_string());
    mcp_config
        .servers
        .insert("beareronly".to_string(), bearer_config);
    let mut pool = McpPool::new(mcp_config);

    let errors = pool.connect_all().await;
    assert_eq!(errors.len(), 2, "{errors:?}");
    for (name, err) in &errors {
        assert!(
            oauth::error_looks_auth_required(err),
            "{name} should fail auth-required: {err:#}"
        );
    }
    assert!(
        pool.all_tools().is_empty(),
        "needs-auth servers advertise no real tools"
    );

    let tools = pool.to_api_tools();
    let auth_tool = tools
        .iter()
        .find(|tool| tool.name == "mcp_wikiserver_authenticate")
        .expect("needs-auth server must expose the synthetic authenticate tool");
    // The coaching contract: show the URL verbatim, the call blocks, real
    // tools replace the synthetic one on success.
    assert!(
        auth_tool
            .description
            .contains("shown to the user in the session status while this call waits"),
        "{}",
        auth_tool.description
    );
    assert!(
        auth_tool.description.contains("blocks (up to 15 minutes)"),
        "{}",
        auth_tool.description
    );
    assert!(
        auth_tool
            .description
            .contains("replace this synthetic authenticate tool"),
        "{}",
        auth_tool.description
    );
    assert!(
        auth_tool.description.contains("/mcp login wikiserver"),
        "{}",
        auth_tool.description
    );
    assert!(
        tools
            .iter()
            .all(|tool| !tool.name.starts_with("mcp_wikiserver_")
                || tool.name == "mcp_wikiserver_authenticate"),
        "no real wikiserver tools while needs-auth: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .all(|tool| !tool.name.starts_with("mcp_beareronly_")),
        "a bearer-configured server must not get the OAuth tool: {tools:?}"
    );

    // The execution predicate the engine consults: only the needs-auth
    // server's exact synthetic name resolves.
    assert_eq!(
        pool.authenticate_tool_target("mcp_wikiserver_authenticate")
            .as_deref(),
        Some("wikiserver")
    );
    assert_eq!(
        pool.authenticate_tool_target("mcp_beareronly_authenticate"),
        None,
        "manual bearer auth is not OAuth-servable"
    );
    assert_eq!(
        pool.authenticate_tool_target("mcp_wikiserver_wiki_lookup"),
        None
    );
    assert_eq!(
        pool.authenticate_tool_target("mcp_unknown_authenticate"),
        None
    );

    // The typed `◆ auth required` state the TUI surfaces derive from the
    // same pool state: both 401 servers carry it, and the OAuth-servable one
    // routes to `/mcp login`.
    assert!(pool.server_needs_auth("wikiserver"));
    assert!(pool.server_needs_auth("beareronly"));
    let error_map: HashMap<String, String> = errors
        .iter()
        .map(|(name, err)| (name.clone(), format_mcp_error_for_display(err)))
        .collect();
    let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &error_map);
    let wiki = snapshot
        .servers
        .iter()
        .find(|server| server.name == "wikiserver")
        .expect("wikiserver in snapshot");
    assert!(wiki.auth_required, "{wiki:?}");
    assert!(!wiki.connected);
    let recovery = wiki
        .recovery_kind(false)
        .expect("a server needing auth has a recovery");
    assert_eq!(recovery, McpRecoveryKind::Reauth);
    assert_eq!(
        recovery.slash_command("wikiserver"),
        "/mcp login wikiserver"
    );

    // A real tool name from a catalog built before the login lapsed must
    // not dead-end: the error names the synthetic tool that recovers it.
    let err = pool
        .call_tool("mcp_wikiserver_wiki_lookup", serde_json::json!({}))
        .await
        .expect_err("a needs-auth server cannot serve real tools");
    let text = format!("{err:#}");
    assert!(text.contains("mcp_wikiserver_authenticate"), "{text}");
    assert!(text.contains("◆ auth required"), "{text}");
    assert!(text.contains("/mcp login wikiserver"), "{text}");
    // Still needs-auth after the failed real call (no state churn).
    assert!(pool.server_needs_auth("wikiserver"));

    mock.task.abort();
}

#[tokio::test]
async fn dead_refresh_grant_flips_server_to_auth_required_and_offers_login_tool() {
    use oauth2::TokenResponse as _;

    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    // A stored credential whose refresh grant the provider now rejects
    // (`invalid_grant`, nobody rotated it): the login has definitively
    // lapsed. Before this slice the server showed as a plain failure and
    // every reconnect replayed the same rejected refresh.
    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let stored = oauth::load_oauth_tokens("wikiserver", &url)
        .unwrap()
        .expect("seeded tokens");
    assert_eq!(
        stored.token_response.0.access_token().secret(),
        "cw-stale-access"
    );

    let mut mcp_config = McpConfig::default();
    mcp_config.servers.insert("wikiserver".to_string(), config);
    let mut pool = McpPool::new(mcp_config);
    let errors = pool.connect_all().await;
    let (_, err) = errors
        .iter()
        .find(|(name, _)| name == "wikiserver")
        .expect("connect fails");
    assert!(
        oauth::error_looks_auth_required(err),
        "a dead grant classifies auth-required: {err:#}"
    );
    assert!(format!("{err:#}").contains("invalid_grant"), "{err:#}");

    // The dead credential is invalidated so auth status no longer claims
    // "logged in", the typed state flips, and the model gets the login tool.
    assert!(
        oauth::load_oauth_tokens("wikiserver", &url)
            .unwrap()
            .is_none(),
        "a definitively rejected grant is removed from the store"
    );
    assert!(pool.server_needs_auth("wikiserver"));
    assert_eq!(
        pool.authenticate_tool_target("mcp_wikiserver_authenticate")
            .as_deref(),
        Some("wikiserver")
    );
    assert!(
        pool.to_api_tools()
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_authenticate"),
        "dead grant must offer the self-serve login tool"
    );
    assert_eq!(
        mock.token_requests.load(AtomicOrdering::SeqCst),
        1,
        "one rejected refresh; no retry against an unchanged store"
    );

    mock.task.abort();
}

#[tokio::test]
async fn selfserve_auth_flow_persists_tokens_and_swaps_real_tools_back() {
    use oauth2::TokenResponse as _;

    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    // Before any login, the pool's connect fails auth-required and the model
    // catalog exposes the synthetic tool in place of the unavailable real
    // tools.
    let mut mcp_config = McpConfig::default();
    mcp_config
        .servers
        .insert("wikiserver".to_string(), config.clone());
    let mut pool = McpPool::new(mcp_config);
    let errors = pool.connect_all().await;
    assert!(
        errors
            .iter()
            .any(|(name, err)| name == "wikiserver" && oauth::error_looks_auth_required(err)),
        "{errors:?}"
    );
    assert!(pool.all_tools().is_empty());
    assert!(
        pool.to_api_tools()
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_authenticate")
    );

    // A declined browser flow must surface a truthful error the model can
    // relay, and leave the server in needs-auth.
    let login =
        oauth::begin_oauth_login_for_server_tool("wikiserver", &config, None, None, None, None)
            .await
            .unwrap();
    let auth_url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let redirect_uri = auth_url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries redirect_uri");
    let flow = tokio::spawn(login.finish());
    test_http_client()
        .get(format!(
            "{redirect_uri}?error=access_denied&error_description=not-today"
        ))
        .send()
        .await
        .unwrap();
    let err = flow
        .await
        .unwrap()
        .expect_err("a declined flow must error, not silently succeed");
    assert!(format!("{err:#}").contains("access_denied"), "{err:#}");
    assert!(
        oauth::load_oauth_tokens("wikiserver", &url)
            .unwrap()
            .is_none(),
        "a declined flow persists no tokens"
    );

    // The approved flow: drive the loopback callback in-test.
    let login =
        oauth::begin_oauth_login_for_server_tool("wikiserver", &config, None, None, None, None)
            .await
            .unwrap();
    let auth_url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let state = auth_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries state");
    let redirect_uri = auth_url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries redirect_uri");
    let flow = tokio::spawn(login.finish());
    test_http_client()
        .get(format!("{redirect_uri}?code=cw-test-code&state={state}"))
        .send()
        .await
        .unwrap();
    flow.await.unwrap().expect("approved flow completes");

    let stored = oauth::load_oauth_tokens("wikiserver", &url)
        .unwrap()
        .expect("successful flow persists tokens");
    assert_eq!(
        stored.token_response.0.access_token().secret(),
        "cw-test-access"
    );

    // The synthetic tool now adopts the completed login and swaps the real
    // tools back through call_tool (the already-authorized branch, since the
    // flow above persisted tokens to the shared store).
    let result = pool
        .call_tool("mcp_wikiserver_authenticate", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result["status"], "already_authorized", "{result}");
    assert!(
        result["tools"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("mcp_wikiserver_wiki_lookup")),
        "{result}"
    );

    let real_names: Vec<String> = pool
        .all_tools()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    assert_eq!(real_names, vec!["mcp_wikiserver_wiki_lookup".to_string()]);
    let catalog = pool.to_api_tools();
    assert!(
        catalog
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_wiki_lookup"),
        "real tools must be in the catalog after auth: {catalog:?}"
    );
    assert!(
        catalog
            .iter()
            .all(|tool| tool.name != "mcp_wikiserver_authenticate"),
        "a connected server no longer advertises the synthetic tool: {catalog:?}"
    );

    mock.task.abort();
}

#[tokio::test]
async fn invalid_grant_refresh_adopts_rotated_on_disk_token() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    // This runtime loaded a stale credential; its refresh grant fails.
    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap()
    .expect("stored tokens produce a runtime");
    // Another process rotates the on-disk credential before our refresh runs.
    seed_oauth_tokens(
        "wikiserver",
        &url,
        "cw-rotated-access",
        "rt-rotated",
        Some(millis_from_now(3_600_000)),
    );

    let header = runtime
        .authorization_header()
        .await
        .unwrap()
        .expect("adopted token produces a header");
    assert_eq!(header, "Bearer cw-rotated-access");
    assert_eq!(
        mock.token_requests.load(AtomicOrdering::SeqCst),
        1,
        "a fresh rotated token is adopted without burning a second refresh"
    );

    mock.task.abort();
}

#[tokio::test]
async fn invalid_grant_refresh_retries_once_with_rotated_grant() {
    use oauth2::TokenResponse as _;

    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap()
    .expect("stored tokens produce a runtime");
    // The rotated credential is itself expired, so the refresh must be
    // retried once with the rotated grant.
    seed_oauth_tokens(
        "wikiserver",
        &url,
        "cw-rotated-stale",
        "rt-rotated",
        Some(1),
    );

    let header = runtime
        .authorization_header()
        .await
        .unwrap()
        .expect("retry with the rotated grant succeeds");
    assert_eq!(header, "Bearer cw-rotated-access-2");
    assert_eq!(
        mock.token_requests.load(AtomicOrdering::SeqCst),
        2,
        "stale grant failed once, rotated grant retried once"
    );
    let stored = oauth::load_oauth_tokens("wikiserver", &url)
        .unwrap()
        .expect("refreshed tokens persisted");
    assert_eq!(
        stored.token_response.0.access_token().secret(),
        "cw-rotated-access-2"
    );

    mock.task.abort();
}

#[tokio::test]
async fn invalid_grant_refresh_with_unchanged_store_invalidates_dead_grant() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap()
    .expect("stored tokens produce a runtime");

    let err = runtime
        .authorization_header()
        .await
        .expect_err("an unchanged store must surface the provider error");
    assert!(format!("{err:#}").contains("invalid_grant"), "{err:#}");
    assert!(
        oauth::error_looks_auth_required(&err),
        "a dead grant is an auth-required failure: {err:#}"
    );
    assert!(
        oauth::load_oauth_tokens("wikiserver", &url)
            .unwrap()
            .is_none(),
        "a grant the provider definitively rejected, that nobody rotated, is invalidated"
    );
    assert_eq!(
        mock.token_requests.load(AtomicOrdering::SeqCst),
        1,
        "no retry when the on-disk credential is unchanged"
    );

    mock.task.abort();
}

#[tokio::test]
async fn invalidation_never_deletes_a_credential_rotated_after_the_re_read() {
    use oauth2::TokenResponse as _;

    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    // Both the held and the rotated credential are dead grants, so the
    // rotated one is adopted, retried, and rejected too. The runtime then
    // invalidates — but a peer that wrote a third credential in between
    // owns the durable winner, which must survive.
    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap()
    .expect("stored tokens produce a runtime");
    let err = runtime
        .authorization_header()
        .await
        .expect_err("dead grant fails");
    assert!(format!("{err:#}").contains("invalid_grant"), "{err:#}");
    assert!(
        oauth::load_oauth_tokens("wikiserver", &url)
            .unwrap()
            .is_none(),
        "unchanged store: invalidated"
    );

    // Now the peer-wins case: a different credential lands on disk before
    // the (second) runtime invalidates its own stale copy.
    seed_oauth_tokens("wikiserver", &url, "cw-stale-access", "rt-stale", Some(1));
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap()
    .expect("stored tokens produce a runtime");
    // Peer rotates to a fresh, non-expired credential the mock accepts.
    seed_oauth_tokens(
        "wikiserver",
        &url,
        "cw-peer-access",
        "rt-fresh",
        Some(millis_from_now(3_600_000)),
    );
    let header = runtime
        .authorization_header()
        .await
        .unwrap()
        .expect("adopts the peer's fresh credential");
    assert_eq!(header, "Bearer cw-peer-access");
    let stored = oauth::load_oauth_tokens("wikiserver", &url)
        .unwrap()
        .expect("the peer's credential is still on disk");
    assert_eq!(
        stored
            .token_response
            .0
            .refresh_token()
            .map(|t| t.secret().as_str()),
        Some("rt-fresh")
    );

    mock.task.abort();
}

#[tokio::test]
async fn mid_session_revocation_lands_in_the_same_auth_required_state() {
    mid_session_revocation_for_backend(McpBackend::Rust).await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_mid_session_revocation_lands_in_the_same_auth_required_state_without_replay() {
    mid_session_revocation_for_backend(McpBackend::Host).await;
}
async fn mid_session_revocation_for_backend(backend: McpBackend) {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;
    let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(true);
    let manager = (backend == McpBackend::Host).then(|| {
        let node = crate::extension_host::tests::node_for_tests("Host revoked OAuth parity")
            .expect("Host parity requires Node");
        Arc::new(crate::extension_host::ExtensionHostManager::new(
            crate::extension_host::ExtensionHostOptions {
                root: Some(dir.path().to_path_buf()),
                node_override: Some(node),
                ..Default::default()
            },
        ))
    });
    let _manager = manager
        .as_ref()
        .map(|manager| crate::extension_host::TestManagerGuard::install(Arc::clone(manager)));

    let mock = OAuthMcpMock::spawn().await;
    let url = mock.url();
    let config = mock_oauth_server_config(mock.addr);

    // A healthy session: stored credential accepted, real tools advertised.
    seed_oauth_tokens(
        "wikiserver",
        &url,
        "cw-test-access",
        "rt-fresh",
        Some(millis_from_now(3_600_000)),
    );
    let mut mcp_config = McpConfig::default();
    mcp_config.servers.insert("wikiserver".to_string(), config);
    let mut pool = McpPool::new(mcp_config).with_backend(backend);
    let errors = pool.connect_all().await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!pool.server_needs_auth("wikiserver"));
    assert!(
        pool.to_api_tools()
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_wiki_lookup")
    );

    // The provider revokes every grant mid-session: the live call 401s, the
    // reactive refresh is rejected with invalid_grant, and the failure must
    // land in the same typed state a failed connect produces — not a dead
    // transport error on a connection the pool still calls "ready".
    // Cross a whole second: reloading the same durable credential now has a
    // smaller derived expires_in, which must not look like a peer rotation.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    mock.revoke_all_grants();
    let err = pool
        .call_tool("mcp_wikiserver_wiki_lookup", serde_json::json!({}))
        .await
        .expect_err("a revoked credential cannot serve real tools");
    let text = format!("{err:#}");
    assert!(text.contains("◆ auth required"), "{text}");
    assert!(text.contains("mcp_wikiserver_authenticate"), "{text}");
    assert!(pool.server_needs_auth("wikiserver"));
    assert!(
        oauth::load_oauth_tokens("wikiserver", &url)
            .unwrap()
            .is_none(),
        "the definitively rejected credential is invalidated; refresh requests: {}; failure: {text}",
        mock.token_requests
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    let catalog = pool.to_api_tools();
    assert!(
        catalog
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_authenticate"),
        "{catalog:?}"
    );
    assert!(
        catalog
            .iter()
            .all(|tool| tool.name != "mcp_wikiserver_wiki_lookup"),
        "a dropped connection advertises no real tools: {catalog:?}"
    );

    // Listing surfaces carry the same recovery, naming the synthetic tool.
    let resources = pool.list_resources(None).await.unwrap();
    let item = resources
        .iter()
        .find(|item| item["error"] == "authentication_required")
        .expect("needs-auth server yields an auth-required listing item");
    assert_eq!(item["server"], "wikiserver");
    assert_eq!(item["authenticate_tool"], "mcp_wikiserver_authenticate");
    assert!(
        item["message"]
            .as_str()
            .unwrap()
            .contains("mcp_wikiserver_authenticate"),
        "{item}"
    );

    // Every TUI surface derives from the same typed state.
    let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &HashMap::new());
    let wiki = snapshot
        .servers
        .iter()
        .find(|server| server.name == "wikiserver")
        .expect("wikiserver in snapshot");
    assert!(wiki.auth_required, "{wiki:?}");
    assert!(!wiki.connected);
    assert_eq!(wiki.recovery_kind(false), Some(McpRecoveryKind::Reauth));

    assert_eq!(
        mock.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "tools/call")
            .count(),
        1,
        "rejected refresh must not replay a tool"
    );
    pool.shutdown_all().await;
    if let Some(manager) = manager {
        manager.shutdown().await;
    }
    mock.task.abort();
}

#[tokio::test]
async fn authenticate_tool_via_pool_releases_the_lock_during_the_browser_wait() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let mock = OAuthMcpMock::spawn().await;
    let mut mcp_config = McpConfig::default();
    mcp_config.servers.insert(
        "wikiserver".to_string(),
        mock_oauth_server_config(mock.addr),
    );
    let pool = Arc::new(tokio::sync::Mutex::new(McpPool::new(mcp_config)));
    let errors = pool.lock().await.connect_all().await;
    assert!(
        errors
            .iter()
            .any(|(name, err)| name == "wikiserver" && oauth::error_looks_auth_required(err)),
        "{errors:?}"
    );

    // The engine path: the URL reaches the runtime before the wait, and the
    // pool lock is free while the user signs in.
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let flow = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move {
            authenticate_tool_via_pool(&pool, "wikiserver", |url| {
                let _ = url_tx.send(url.to_string());
            })
            .await
        }
    });
    let auth_url = url_rx
        .await
        .expect("authorization URL announced before the wait");
    {
        let guard = tokio::time::timeout(Duration::from_secs(5), pool.lock())
            .await
            .expect("pool lock must not be held during the browser wait");
        assert!(guard.server_needs_auth("wikiserver"));
        assert!(
            guard
                .to_api_tools()
                .iter()
                .any(|tool| tool.name == "mcp_wikiserver_authenticate"),
            "still needs-auth until the flow completes"
        );
    }

    let parsed = reqwest::Url::parse(&auth_url).unwrap();
    let state = parsed
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries state");
    let redirect_uri = parsed
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries redirect_uri");
    test_http_client()
        .get(format!("{redirect_uri}?code=cw-test-code&state={state}"))
        .send()
        .await
        .unwrap();
    let result = flow
        .await
        .unwrap()
        .expect("approved flow reconnects the server");
    assert_eq!(result["status"], "authenticated", "{result}");
    assert_eq!(result["authorization_url"], auth_url, "{result}");
    assert!(
        result["tools"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("mcp_wikiserver_wiki_lookup")),
        "{result}"
    );

    let guard = pool.lock().await;
    assert!(!guard.server_needs_auth("wikiserver"));
    let catalog = guard.to_api_tools();
    assert!(
        catalog
            .iter()
            .any(|tool| tool.name == "mcp_wikiserver_wiki_lookup"),
        "{catalog:?}"
    );
    assert!(
        catalog
            .iter()
            .all(|tool| tool.name != "mcp_wikiserver_authenticate"),
        "{catalog:?}"
    );
    drop(guard);

    mock.task.abort();
}

#[tokio::test]
async fn plugin_contributed_server_auth_required_names_its_env_credential_not_oauth_login() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;

    let plugin_base = dir.path().join("plugins/wiki-plugin");
    fs::create_dir_all(&plugin_base).unwrap();
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"wiki-plugin\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let (_, authority) = active_plugin_fixture(&plugin_base);

    // A plugin-contributed server whose reviewed environment-backed header is
    // absent, against an endpoint that 401s without it.
    let mock = OAuthMcpMock::spawn().await;
    let endpoint = mock.url();
    let mut server = mock_oauth_server_config(mock.addr);
    server.env_headers.insert(
        "Authorization".to_string(),
        "CW_TEST_WIKI_PLUGIN_TOKEN".to_string(),
    );
    server.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            Some(&endpoint),
            Arc::new(crate::plugins::HostEnvironment::default()),
        )
        .unwrap(),
    );
    let mut mcp_config = McpConfig::default();
    mcp_config.servers.insert("wikiplugin".to_string(), server);
    let mut pool = McpPool::new(mcp_config);

    let errors = pool.connect_all().await;
    let (_, err) = errors
        .iter()
        .find(|(name, _)| name == "wikiplugin")
        .expect("plugin server fails to connect");
    assert!(oauth::error_looks_auth_required(err), "{err:#}");

    // Same typed state as an OAuth server, but never the OAuth tool: plugin
    // servers authenticate through their reviewed environment header.
    assert!(pool.server_needs_auth("wikiplugin"));
    assert_eq!(
        pool.authenticate_tool_target("mcp_wikiplugin_authenticate"),
        None
    );
    assert!(
        pool.to_api_tools()
            .iter()
            .all(|tool| !tool.name.starts_with("mcp_wikiplugin_")),
        "no synthetic OAuth tool for a plugin-contributed server"
    );

    let err = pool
        .call_tool("mcp_wikiplugin_wiki_lookup", serde_json::json!({}))
        .await
        .expect_err("needs-auth plugin server cannot serve real tools");
    let text = format!("{err:#}");
    assert!(text.contains("◆ auth required"), "{text}");
    assert!(text.contains("plugin 'wiki-plugin'"), "{text}");
    assert!(text.contains("CW_TEST_WIKI_PLUGIN_TOKEN"), "{text}");
    assert!(text.contains("/mcp reload"), "{text}");
    assert!(!text.contains("/mcp login"), "{text}");
    assert!(!text.contains("mcp_wikiplugin_authenticate"), "{text}");

    let snapshot = pool.manager_snapshot(&dir.path().join("mcp.json"), false, &HashMap::new());
    let plugin = snapshot
        .servers
        .iter()
        .find(|server| server.name == "wikiplugin")
        .expect("plugin server in snapshot");
    assert!(plugin.auth_required, "{plugin:?}");

    mock.task.abort();
}

#[test]
fn mcp_display_target_shows_command_names_only() {
    // stdio: no `./…` path prefix, no directories, no args.
    assert_eq!(
        mcp_display_target("stdio", "./mcp/custom-server --port 8080"),
        "custom-server"
    );
    assert_eq!(mcp_display_target("stdio", "node server.js"), "node");
    assert_eq!(mcp_display_target("stdio", "/usr/local/bin/foo -x"), "foo");
    assert_eq!(
        mcp_display_target("stdio", "C:\\tools\\mcp.exe --stdio"),
        "mcp.exe"
    );
    assert_eq!(mcp_display_target("stdio", "(missing)"), "(missing)");
    // URL transports keep the full URL: it is the identity.
    assert_eq!(
        mcp_display_target("http/sse", "https://example.invalid/mcp"),
        "https://example.invalid/mcp"
    );
    assert_eq!(
        mcp_display_target("sse", "https://example.invalid/sse?token=abc"),
        "https://example.invalid/sse?token=abc"
    );
}

fn test_mcp_http_client(url: &str) -> super::http_client::McpHttpClient {
    super::http_client::McpHttpClient::new(
        url,
        false,
        false,
        false,
        None,
        Duration::from_secs(10),
        Duration::from_secs(120),
    )
    .expect("MCP fixture client")
}

fn ceiling_test_connection(name: &str, sent: Arc<Mutex<Vec<serde_json::Value>>>) -> McpConnection {
    let mut connection = test_connection(Box::new(ScriptedValueTransport {
        sent,
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": {"ok": true}
        }))]),
    }));
    connection.name = name.to_string();
    connection.tools = ["read", "delete"]
        .into_iter()
        .map(|name| McpTool {
            name: name.to_string(),
            description: None,
            input_schema: serde_json::json!({}),
            annotations: None,
        })
        .collect();
    connection.resources = vec![McpResource {
        name: "one".to_string(),
        uri: "memory://one".to_string(),
        description: None,
        mime_type: None,
    }];
    connection.resource_templates = vec![McpResourceTemplate {
        name: "items".to_string(),
        uri_template: "memory://{id}".to_string(),
        description: None,
        mime_type: None,
    }];
    connection.prompts = vec![McpPrompt {
        name: "review".to_string(),
        description: None,
        arguments: vec![],
    }];
    connection
}

#[tokio::test]
async fn mcp_ceiling_denied_server_is_absent_across_cached_boot_meta_auth_and_runtime_paths() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let connection = ceiling_test_connection("private_a", Arc::clone(&sent));
    let mut config = connection.config.clone();
    config.required = true;
    config.url = Some("https://mcp.example.com".to_string());
    config.command = None;
    config.scopes = vec!["read".to_string()];
    let mut pool = McpPool::new(McpConfig {
        servers: HashMap::from([("private_a".to_string(), config.clone())]),
        timeouts: McpTimeouts::default(),
    })
    .with_disallowed_tools(vec!["MCP_PRIVATE_A_*".to_string()]);
    // A previously connected or auth-failed entry must not become reachable.
    pool.connections.insert("private_a".to_string(), connection);
    pool.needs_auth_servers.insert("private_a".to_string());
    assert!(pool.all_tools().is_empty());
    assert!(pool.all_resources().is_empty());
    assert!(pool.all_resource_templates().is_empty());
    assert!(pool.all_prompts().is_empty());
    assert!(pool.resolved_tool_servers().is_empty());
    assert!(pool.to_api_tools().is_empty());
    assert!(pool.model_tool_names(&pool.to_api_tools()).is_empty());
    assert!(pool.enabled_server_names().is_empty());
    assert!(pool.server_names().is_empty());
    assert!(pool.connected_servers().is_empty());
    assert!(!pool.server_needs_auth("private_a"));
    assert!(
        pool.authenticate_tool_target("mcp_private_a_authenticate")
            .is_none()
    );
    let (pending, errors) = pool.collect_pending_connects(None);
    assert!(pending.is_empty() && errors.is_empty());
    assert!(
        pool.connect_all().await.is_empty(),
        "a denied required server is absent"
    );
    assert!(
        pool.manager_snapshot(Path::new("/unused"), false, &HashMap::new())
            .servers
            .is_empty()
    );
    for method in [
        "list_mcp_resources",
        "list_mcp_resource_templates",
        "mcp_read_resource",
        "read_mcp_resource",
        "mcp_get_prompt",
    ] {
        let error = pool
            .call_tool(
                method,
                serde_json::json!({"server": "private_a", "uri": "memory://one", "name": "review"}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Failed to find MCP server: private_a",
            "{method}"
        );
    }
    for method in [
        "mcp_private_a_read",
        "mcp_private_a_delete",
        "mcp_private_a_authenticate",
    ] {
        assert_eq!(
            pool.call_tool(method, serde_json::json!({}))
                .await
                .unwrap_err()
                .to_string(),
            format!("Unknown MCP tool name: {method}")
        );
    }
    assert!(pool.begin_authenticate_tool("private_a").await.is_err());
    assert!(pool.retry_connection("private_a").await.is_err());
    assert!(
        pool.add_runtime_server_config("private_a".to_string(), config.clone())
            .is_err()
    );
    let denied = pool
        .get_or_connect("private_a")
        .await
        .err()
        .unwrap()
        .to_string();
    let mut absent = McpPool::new(McpConfig::default());
    let missing = absent
        .get_or_connect("private_a")
        .await
        .err()
        .unwrap()
        .to_string();
    assert_eq!(denied, missing);
    assert!(sent.lock().unwrap().is_empty(), "no MCP request is sent");
    let stale = ceiling_test_connection("private_a", Arc::clone(&sent));
    assert!(
        pool.store_ready_connection("private_a".to_string(), stale)
            .is_err()
    );
}

#[tokio::test]
async fn mcp_ceiling_individual_tool_denial_preserves_server_resources_and_sibling_tool() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let connection = ceiling_test_connection("private_a", Arc::clone(&sent));
    let mut pool = McpPool::new(McpConfig::default())
        .with_disallowed_tools(vec!["MCP_PRIVATE_A_DELETE".to_string()]);
    pool.connections.insert("private_a".to_string(), connection);
    assert_eq!(
        pool.all_tools()
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp_private_a_read"]
    );
    assert_eq!(pool.all_resources().len(), 1);
    assert_eq!(pool.all_prompts().len(), 1);
    assert!(
        pool.call_tool("mcp_private_a_delete", serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(sent.lock().unwrap().is_empty());
    let resources = pool
        .call_tool(
            "list_mcp_resources",
            serde_json::json!({"server": "private_a"}),
        )
        .await
        .unwrap();
    assert_eq!(resources["resources"].as_array().unwrap().len(), 1);
    assert_eq!(
        pool.call_tool("mcp_private_a_read", serde_json::json!({}))
            .await
            .unwrap(),
        serde_json::json!({"ok": true})
    );
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(sent.lock().unwrap()[0]["params"]["name"], "read");
}

#[tokio::test]
async fn mcp_ceiling_child_scoped_meta_calls_do_not_widen_or_mutate_sibling_policy() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let private = ceiling_test_connection("private_a", Arc::clone(&sent));
    let public = ceiling_test_connection("public", Arc::clone(&sent));
    let mut pool = McpPool::new(McpConfig {
        servers: HashMap::from([
            ("private_a".to_string(), private.config.clone()),
            ("public".to_string(), public.config.clone()),
        ]),
        timeouts: McpTimeouts::default(),
    });
    pool.connections.insert("private_a".to_string(), private);
    pool.connections.insert("public".to_string(), public);
    let child_rules = vec!["mcp_private_a_*".to_string()];
    for (method, field) in [
        ("list_mcp_resources", "resources"),
        ("list_mcp_resource_templates", "templates"),
    ] {
        let child = pool
            .call_tool_with_disallowed(method, serde_json::json!({}), &child_rules, None)
            .await
            .unwrap();
        assert_eq!(child[field].as_array().unwrap().len(), 1);
        assert_eq!(child[field][0]["server"], "public");
        let sibling = pool
            .call_tool_with_disallowed(method, serde_json::json!({}), &[], None)
            .await
            .unwrap();
        assert_eq!(sibling[field].as_array().unwrap().len(), 2);
    }
    assert!(
        pool.call_tool_with_disallowed(
            "read_mcp_resource",
            serde_json::json!({"server":"private_a","uri":"memory://one"}),
            &child_rules,
            None
        )
        .await
        .is_err()
    );
    assert!(
        pool.call_tool_with_disallowed(
            "mcp_private_a_read",
            serde_json::json!({}),
            &child_rules,
            None
        )
        .await
        .is_err()
    );
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(
        pool.call_tool_with_disallowed("mcp_private_a_read", serde_json::json!({}), &[], None)
            .await
            .unwrap(),
        serde_json::json!({"ok":true})
    );
}

#[tokio::test]
async fn mcp_ceiling_survives_source_reload_and_blocks_new_runtime_names() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("mcp.json");
    fs::write(&source, r#"{"mcpServers": {}}"#).unwrap();
    let mut pool = McpPool::from_config_path(&source)
        .unwrap()
        .with_disallowed_tools(vec!["mcp_private*".to_string()]);
    fs::write(&source, r#"{"mcpServers":{"private":{"command":"must-not-execute-private","required":true},"private_a":{"command":"must-not-execute-private"}}}"#).unwrap();
    pool.force_reload_config_sources().unwrap();
    assert!(pool.connect_all().await.is_empty());
    assert!(pool.enabled_server_names().is_empty());
    assert!(pool.get_or_connect("private_a").await.is_err());
    assert!(
        pool.add_runtime_server_config("private_new".to_string(), test_server_config())
            .is_err()
    );
    assert!(!pool.dynamic_servers.read().contains_key("private_new"));
    pool.add_runtime_server_config("public".to_string(), test_server_config())
        .unwrap();
    assert_eq!(pool.enabled_server_names(), vec!["public"]);
}

#[test]
fn mcp_ceiling_namespace_rules_keep_individual_denials_distinct_and_aliases_consistent() {
    assert!(McpPool::server_denied_by(&["MCP_A_B_*".to_string()], "a_b"));
    assert!(!McpPool::server_denied_by(&["MCP_A_B_*".to_string()], "a"));
    assert!(!McpPool::server_denied_by(
        &["mcp_a_delete".to_string()],
        "a"
    ));
    assert!(!McpPool::server_denied_by(
        &["mcp_a_delete*".to_string()],
        "a"
    ));
    assert!(McpPool::server_denied_by(
        &["mcp*".to_string()],
        "any_server"
    ));
    for name in ["mcp_read_resource", "read_mcp_resource"] {
        assert!(
            McpPool::authorize_call(
                &["mcp_a_*".to_string()],
                name,
                &serde_json::json!({"server":"a"})
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn mcp_ceiling_preserves_ordinary_tool_result_tools_field() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let expected = serde_json::json!({"tools":[{"name":"server-owned-data"}], "ok":true});
    let mut connection = ceiling_test_connection("public", Arc::clone(&sent));
    connection.transport = Box::new(ScriptedValueTransport {
        sent,
        responses: VecDeque::from([json_frame(
            serde_json::json!({"jsonrpc":"2.0","id":1,"result":expected}),
        )]),
    });
    let mut pool = McpPool::new(McpConfig::default());
    pool.connections.insert("public".to_string(), connection);
    assert_eq!(
        pool.call_tool_with_disallowed(
            "mcp_public_read",
            serde_json::json!({}),
            &["mcp_private_*".to_string()],
            None
        )
        .await
        .unwrap(),
        expected
    );
    for alias in ["read_mcp_resource", "mcp_read_resource"] {
        for denied in ["read_mcp_resource", "mcp_read_resource"] {
            assert!(
                McpPool::authorize_call(
                    &[denied.to_string()],
                    alias,
                    &serde_json::json!({"server":"public"})
                )
                .is_err()
            );
        }
    }
}

/// #6213 T7: the resource-URI template check is an authorization decision that
/// runs per URI per advertised template. Pin what it accepts, what it refuses,
/// and that the anchored pattern is compiled once rather than per call.
#[test]
fn resource_uri_template_matching_is_anchored_and_fail_closed() {
    // Literal templates are anchored: no suffix may sneak past.
    assert!(resource_uri_matches_template(
        "file:///readme",
        "file:///readme"
    ));
    assert!(!resource_uri_matches_template(
        "file:///readme/extra",
        "file:///readme"
    ));

    // `{id}` is a simple expansion, so it must not cross a path separator.
    assert!(resource_uri_matches_template("file:///a", "file:///{id}"));
    assert!(!resource_uri_matches_template(
        "file:///a/b",
        "file:///{id}"
    ));

    // `{+path}` is a reserved expansion, so it may.
    assert!(resource_uri_matches_template(
        "file:///a/b/c",
        "file:///{+path}"
    ));

    // An operator this subset does not implement, and a template that never
    // closes its expression, both stay uncallable rather than over-matching.
    assert!(!resource_uri_matches_template("x", "x{?query}"));
    assert!(!resource_uri_matches_template("x", "x{id"));

    // The compile happens once per template and is reused.
    let first = compiled_resource_template("file:///{path}").expect("template compiles");
    let second = compiled_resource_template("file:///{path}").expect("template compiles");
    assert!(Arc::ptr_eq(&first, &second));
    assert!(compiled_resource_template("x{?query}").is_none());
}

struct DeadTransport;

#[async_trait::async_trait]
impl McpTransport for DeadTransport {
    async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn probe_dead(&self) -> bool {
        true
    }
}

fn supervised_pool(name: &str) -> McpPool {
    let mut servers = HashMap::new();
    servers.insert(name.to_string(), test_server_config());
    McpPool::new(McpConfig {
        timeouts: McpTimeouts::default(),
        servers,
    })
}

/// #6187: a dead connection is planned for reconnect, and a failed attempt
/// reports the death once with the diagnosis.
#[test]
fn supervisor_plans_dead_connection_and_reports_failed_reconnect() {
    let mut pool = supervised_pool("alpha");
    let mut connection = test_connection(Box::new(DeadTransport));
    connection.name = "alpha".to_string();
    pool.connections.insert("alpha".to_string(), connection);

    let plan = pool.plan_supervision();
    assert_eq!(plan.due.len(), 1);
    assert_eq!(plan.due[0].name, "alpha");
    assert!(plan.due[0].fresh_death);
    assert!(plan.recovered.is_empty());

    let update = pool.resolve_supervision_attempt(
        "alpha",
        true,
        Err(anyhow::anyhow!("connection reset by peer")),
    );
    assert_eq!(update.died.len(), 1);
    assert!(update.died[0].1.contains("connection reset"));
    assert!(update.failed.is_empty() && update.recovered.is_empty());

    // The failure bought a cooldown: the next sweep attempts nothing and
    // reports nothing new.
    let plan = pool.plan_supervision();
    assert!(plan.due.is_empty());
    assert!(plan.recovered.is_empty() && plan.parked.is_empty());
}

/// #6187: recovery is reported on the transition back to alive.
#[test]
fn supervisor_reports_recovery_on_transition() {
    let mut pool = supervised_pool("alpha");
    let mut dead = test_connection(Box::new(DeadTransport));
    dead.name = "alpha".to_string();
    pool.connections.insert("alpha".to_string(), dead);

    let plan = pool.plan_supervision();
    assert_eq!(plan.due.len(), 1);
    let update = pool.resolve_supervision_attempt("alpha", true, Err(anyhow::anyhow!("boom")));
    assert_eq!(update.died.len(), 1);

    // The transport reads alive again (a flapping probe, or a connection
    // restored outside the store path): the next sweep reports recovery.
    let mut live = test_connection(Box::new(DropCountingTransportForSupervision));
    live.name = "alpha".to_string();
    pool.connections.insert("alpha".to_string(), live);
    let plan = pool.plan_supervision();
    assert_eq!(plan.recovered, vec!["alpha".to_string()]);
    assert!(plan.due.is_empty());

    // Reported once: the sweep after is silent.
    let plan = pool.plan_supervision();
    assert!(plan.recovered.is_empty() && plan.due.is_empty());
}

struct DropCountingTransportForSupervision;

#[async_trait::async_trait]
impl McpTransport for DropCountingTransportForSupervision {
    async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

/// #6187: five consecutive failures park the server — no more auto attempts
/// until an explicit retry — and the park is reported once.
#[test]
fn supervisor_parks_after_repeated_failures() {
    let mut pool = supervised_pool("alpha");
    let mut dead = test_connection(Box::new(DeadTransport));
    dead.name = "alpha".to_string();
    pool.connections.insert("alpha".to_string(), dead);

    for attempt in 0..5 {
        let update = pool.resolve_supervision_attempt(
            "alpha",
            attempt == 0,
            Err(anyhow::anyhow!("refused")),
        );
        if attempt < 4 {
            assert!(update.parked.is_empty(), "parks on the fifth failure");
        } else {
            assert_eq!(update.parked, vec!["alpha".to_string()]);
        }
    }
    // Parked: the plan attempts nothing further.
    let plan = pool.plan_supervision();
    assert!(plan.due.is_empty());

    // A stored-ready connection clears the park.
    let mut live = test_connection(Box::new(DropCountingTransportForSupervision));
    live.name = "alpha".to_string();
    live.catalog_generation = pool.current_catalog_generation();
    pool.store_ready_connection("alpha".to_string(), live)
        .expect("stores");
    assert!(!pool.supervised_parked.contains("alpha"));
    assert!(!pool.supervised_dead.contains("alpha"));
}

/// #6187: an explicit retry restarts supervision even when the retry itself
/// fails — the user asked, so the park and dead mark clear.
#[tokio::test]
async fn manual_retry_clears_supervision_marks() {
    let mut pool = supervised_pool("alpha");
    pool.supervised_dead.insert("alpha".to_string());
    pool.supervised_parked.insert("alpha".to_string());
    // `mock` is not a real binary, so the retry fails; the marks still clear.
    let _ = pool.retry_connection("alpha").await;
    assert!(!pool.supervised_dead.contains("alpha"));
    assert!(!pool.supervised_parked.contains("alpha"));
}

/// #6187: a dead pipe/socket rebuilds the connection, not just a stale
/// session. Only a refused session id proves the server never ran the
/// request, so only that class may replay a tool call.
#[test]
fn connection_lost_covers_closed_transports_but_only_rejected_sessions_replay() {
    use super::wire::{
        McpSessionRejected, is_mcp_connection_lost_error, is_mcp_session_rejected_error,
    };
    for rejected in [
        "MCP session expired (transport=sse endpoint=x status=400 Bad Request): session invalid",
        "MCP Streamable HTTP session expired; retry with a new session required (404)",
    ] {
        let err = anyhow::Error::from(McpSessionRejected(rejected.to_string()))
            .context("MCP method 'tools/call' failed");
        assert!(is_mcp_connection_lost_error(&err), "{rejected}");
        assert!(is_mcp_session_rejected_error(&err), "{rejected}");
        // The same words without the transport's type are not a refusal:
        // a JSON-RPC error answering the request can carry them.
        let text_only = anyhow::anyhow!("{rejected}");
        assert!(is_mcp_connection_lost_error(&text_only), "{rejected}");
        assert!(!is_mcp_session_rejected_error(&text_only), "{rejected}");
    }
    for ambiguous in [
        "MCP session expired: {\"code\":-32000,\"message\":\"session invalid\"}",
        "connection reset by peer",
        "Stdio transport closed",
        "Stdio transport closed (exit status: 1)\nsession invalid",
        "SSE transport closed",
        "MCP SSE POST send failed (transport=sse endpoint=x): connection closed",
    ] {
        let err = anyhow::anyhow!("{ambiguous}");
        assert!(is_mcp_connection_lost_error(&err), "{ambiguous}");
        assert!(!is_mcp_session_rejected_error(&err), "{ambiguous}");
    }
    let app = anyhow::anyhow!("tool returned an application error");
    assert!(!is_mcp_connection_lost_error(&app));
    assert!(!is_mcp_session_rejected_error(&app));
}

/// A stdio server that logs every `tools/call` it receives to `$CALL_LOG`,
/// so a test can count how many times the tool really ran. `$FIRST_CALL`
/// picks what happens to the first call after it has run: `exit` kills the
/// child before replying, `session-error` answers it with a JSON-RPC error
/// whose text mentions an invalid session.
#[cfg(unix)]
const COUNTING_STDIO_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    case "$line" in
        *'"method":"notifications/'*)
            ;;
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":"%s","result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"counter","version":"1.0.0"},"capabilities":{"tools":{}}}}\n' "$id"
            ;;
        *'"method":"tools/list"'*)
            printf '{"jsonrpc":"2.0","id":"%s","result":{"tools":[{"name":"act","inputSchema":{"type":"object"}}]}}\n' "$id"
            ;;
        *'"method":"tools/call"'*)
            echo call >> "$CALL_LOG"
            if [ "$(wc -l < "$CALL_LOG")" -eq 1 ]; then
                case "$FIRST_CALL" in
                    exit) exit 0 ;;
                    session-error)
                        printf '{"jsonrpc":"2.0","id":"%s","error":{"code":-32000,"message":"session invalid"}}\n' "$id"
                        continue
                        ;;
                esac
            fi
            printf '{"jsonrpc":"2.0","id":"%s","result":{"content":[{"type":"text","text":"ok"}]}}\n' "$id"
            ;;
        *)
            [ -n "$id" ] && printf '{"jsonrpc":"2.0","id":"%s","result":{}}\n' "$id"
            ;;
    esac
done
"#;

#[cfg(unix)]
fn counting_stdio_pool(dir: &Path, first_call: &str) -> (McpPool, PathBuf) {
    let script = dir.join("server.sh");
    fs::write(&script, COUNTING_STDIO_SERVER).unwrap();
    let call_log = dir.join("calls.log");
    let mut server = test_server_config();
    server.command = Some("sh".to_string());
    server.args = vec![script.to_string_lossy().into_owned()];
    server.env.insert(
        "CALL_LOG".to_string(),
        call_log.to_string_lossy().into_owned(),
    );
    server
        .env
        .insert("FIRST_CALL".to_string(), first_call.to_string());
    server.connect_timeout = Some(10);
    server.execute_timeout = Some(10);
    let mut cfg = McpConfig::default();
    cfg.servers.insert("counter".to_string(), server);
    (McpPool::new(cfg), call_log)
}

#[cfg(unix)]
fn server_call_count(call_log: &Path) -> usize {
    fs::read_to_string(call_log)
        .map(|log| log.lines().count())
        .unwrap_or(0)
}

/// A stdio child that dies after reading `tools/call` may have run it: the
/// call fails as unknown, runs once, and the next call gets a fresh child.
#[cfg(unix)]
#[tokio::test]
async fn stdio_child_exit_during_tool_call_is_not_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut pool, call_log) = counting_stdio_pool(dir.path(), "exit");

    let err = pool
        .call_tool("mcp_counter_act", serde_json::json!({}))
        .await
        .expect_err("a call whose child exited mid-flight must not be replayed");
    assert!(
        format!("{err:#}").contains("outcome unknown, not retried"),
        "unexpected error: {err:#}"
    );
    assert_eq!(server_call_count(&call_log), 1);

    let result = pool
        .call_tool("mcp_counter_act", serde_json::json!({}))
        .await
        .expect("the next call reconnects");
    assert_eq!(
        result,
        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
    assert_eq!(server_call_count(&call_log), 2);
}

/// A JSON-RPC error answering the call id means the server processed the
/// request, even when its text mentions an invalid session: the tool may
/// have acted before failing, so the call is not sent again.
#[cfg(unix)]
#[tokio::test]
async fn json_rpc_session_error_on_tool_call_is_not_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut pool, call_log) = counting_stdio_pool(dir.path(), "session-error");

    let err = pool
        .call_tool("mcp_counter_act", serde_json::json!({}))
        .await
        .expect_err("a session error answering the call must not be replayed");
    assert!(
        format!("{err:#}").contains("outcome unknown, not retried"),
        "unexpected error: {err:#}"
    );
    assert_eq!(server_call_count(&call_log), 1);

    let result = pool
        .call_tool("mcp_counter_act", serde_json::json!({}))
        .await
        .expect("the next call reconnects");
    assert_eq!(
        result,
        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
    assert_eq!(server_call_count(&call_log), 2);
}

/// A sequential stdio server for the `tools/call` deadline fixtures. Every
/// `initialize` appends `init <pid>` to `$INIT_LOG`, so a test can tell a
/// reused connection from a rebuilt one; every `tools/call` appends
/// `<tool> <id>` to `$CALL_LOG` before it runs, and its reply text is that
/// same line, so a test can tell which request a reply answers. `slow` sleeps
/// `$SLOW_SECS` before replying, `hang` never replies, `fast` replies at once.
#[cfg(unix)]
const DEADLINE_STDIO_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    case "$line" in
        *'"method":"notifications/'*)
            ;;
        *'"method":"initialize"'*)
            echo "init $$" >> "$INIT_LOG"
            printf '{"jsonrpc":"2.0","id":"%s","result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"deadline","version":"1.0.0"},"capabilities":{"tools":{}}}}\n' "$id"
            ;;
        *'"method":"tools/list"'*)
            printf '{"jsonrpc":"2.0","id":"%s","result":{"tools":[{"name":"slow","inputSchema":{"type":"object"}},{"name":"fast","inputSchema":{"type":"object"}},{"name":"hang","inputSchema":{"type":"object"}}]}}\n' "$id"
            ;;
        *'"method":"tools/call"'*)
            tool=$(printf '%s\n' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
            echo "$tool $id" >> "$CALL_LOG"
            case "$tool" in
                slow) sleep "$SLOW_SECS" ;;
                hang) while :; do sleep 0.05; done ;;
            esac
            printf '{"jsonrpc":"2.0","id":"%s","result":{"content":[{"type":"text","text":"%s %s"}]}}\n' "$id" "$tool" "$id"
            ;;
        *)
            [ -n "$id" ] && printf '{"jsonrpc":"2.0","id":"%s","result":{}}\n' "$id"
            ;;
    esac
done
"#;

/// A pool with one `deadline` stdio server; returns it with the call and
/// init log paths.
#[cfg(unix)]
fn deadline_stdio_pool(
    dir: &Path,
    slow_secs: &str,
    configure: impl FnOnce(&mut McpServerConfig),
) -> (McpPool, PathBuf, PathBuf) {
    let script = dir.join("server.sh");
    fs::write(&script, DEADLINE_STDIO_SERVER).unwrap();
    let call_log = dir.join("calls.log");
    let init_log = dir.join("inits.log");
    let mut server = test_server_config();
    server.command = Some("sh".to_string());
    server.args = vec![script.to_string_lossy().into_owned()];
    for (key, value) in [
        ("CALL_LOG", call_log.to_string_lossy().into_owned()),
        ("INIT_LOG", init_log.to_string_lossy().into_owned()),
        ("SLOW_SECS", slow_secs.to_string()),
    ] {
        server.env.insert(key.to_string(), value);
    }
    configure(&mut server);
    let mut cfg = McpConfig::default();
    cfg.servers.insert("deadline".to_string(), server);
    (McpPool::new(cfg), call_log, init_log)
}

#[cfg(unix)]
fn log_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|log| log.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The result `DEADLINE_STDIO_SERVER` returns for the call it logged as `call`.
#[cfg(unix)]
fn deadline_reply(call: &str) -> serde_json::Value {
    serde_json::json!({ "content": [{ "type": "text", "text": call }] })
}

/// #6741 over a real stdio child: a `tools/call` whose reply arrives after
/// the read knob — the receive budget every request used to get — but inside
/// the execute budget completes, and the next call on the same connection gets
/// its own reply. The late frame is consumed once, by the request it answers;
/// nothing is replayed and the child is not restarted.
#[cfg(unix)]
#[tokio::test]
async fn stdio_tool_reply_after_the_read_knob_completes_within_the_execute_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (mut pool, call_log, init_log) = deadline_stdio_pool(dir.path(), "3", |server| {
        server.read_timeout = Some(1);
        server.execute_timeout = Some(30);
    });

    let started = std::time::Instant::now();
    let slow = pool
        .call_tool("mcp_deadline_slow", serde_json::json!({}))
        .await
        .expect("a reply inside the execute budget must not be cut off at the read knob");
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "the reply must really have outlived the 1s read knob"
    );
    let fast = pool
        .call_tool("mcp_deadline_fast", serde_json::json!({}))
        .await
        .expect("the next call on the same connection must get its own reply");

    let calls = log_lines(&call_log);
    assert_eq!(calls.len(), 2, "neither call may be replayed: {calls:?}");
    assert!(calls[0].starts_with("slow ") && calls[1].starts_with("fast "));
    assert_eq!(slow, deadline_reply(&calls[0]));
    assert_eq!(fast, deadline_reply(&calls[1]));
    assert_eq!(
        log_lines(&init_log).len(),
        1,
        "both calls must share one connection and child"
    );
}

/// An explicit `execute_timeout` shorter than the read knob still governs a
/// real stdio `tools/call`: the call fails at its own 2s budget, not at the
/// 120s read knob or the 1800s default. The request is abandoned, not the
/// connection: the child's late reply to it reaches the pipe ahead of the
/// next call's reply and is skipped, so the next call gets its own reply on
/// the same child.
#[cfg(unix)]
#[tokio::test]
async fn explicit_shorter_execute_budget_ends_a_stdio_tool_call_and_skips_its_late_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (mut pool, call_log, init_log) = deadline_stdio_pool(dir.path(), "3", |server| {
        server.execute_timeout = Some(2);
    });
    pool.get_or_connect("deadline").await.unwrap();

    let started = std::time::Instant::now();
    let error = pool
        .call_tool("mcp_deadline_slow", serde_json::json!({}))
        .await
        .expect_err("an explicit shorter budget must end the call");
    let elapsed = started.elapsed();
    assert!(
        format!("{error:#}")
            .contains("MCP method 'tools/call' on server 'deadline' timed out after 2s"),
        "unexpected error: {error:#}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "the explicit budget, not the 3s reply, must end the call: {elapsed:?}"
    );

    let fast = pool
        .call_tool("mcp_deadline_fast", serde_json::json!({}))
        .await
        .expect("the kept connection must answer the next call");
    let calls = log_lines(&call_log);
    assert_eq!(calls.len(), 2, "neither call may be replayed: {calls:?}");
    assert!(calls[0].starts_with("slow ") && calls[1].starts_with("fast "));
    assert_eq!(
        fast,
        deadline_reply(&calls[1]),
        "the abandoned call's late reply must not answer the next call"
    );
    assert_eq!(
        log_lines(&init_log).len(),
        1,
        "an expired request must not rebuild the connection"
    );
}

/// Owned cancellation of an in-flight stdio `tools/call` under the default
/// 1800s execute budget: cancelling the connection's own token ends the call
/// promptly instead of waiting out the budget, and marks the connection dead.
/// The next call rebuilds it on a fresh child, the cancelled call's child is
/// terminated rather than left running, and the cancelled call is not replayed.
#[cfg(unix)]
#[tokio::test]
async fn cancelling_an_inflight_stdio_tool_call_does_not_wait_for_the_execute_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (mut pool, call_log, init_log) = deadline_stdio_pool(dir.path(), "0", |_| {});
    assert_eq!(
        pool.config.servers["deadline"].effective_execute_timeout(&pool.config.timeouts),
        1800,
        "the fixture must run under the default execute budget"
    );
    pool.get_or_connect("deadline").await.unwrap();
    let cancel = pool.connections["deadline"].cancel_token.clone();

    let call = tokio::spawn(async move {
        let result = pool
            .call_tool("mcp_deadline_hang", serde_json::json!({}))
            .await;
        (pool, result)
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while log_lines(&call_log).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the child never received the call");

    cancel.cancel();
    let (mut pool, result) = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("cancellation must not wait for the execute budget")
        .expect("the call task panicked");
    let error = result.expect_err("a cancelled call must not report success");
    assert!(
        format!("{error:#}").contains("MCP connection 'deadline' was cancelled"),
        "unexpected error: {error:#}"
    );
    assert!(
        pool.connected_servers().is_empty(),
        "a cancelled connection must not be reused"
    );

    let fast = pool
        .call_tool("mcp_deadline_fast", serde_json::json!({}))
        .await
        .expect("the next call must rebuild the connection");
    let calls = log_lines(&call_log);
    assert_eq!(
        calls.len(),
        2,
        "the cancelled call must not be replayed: {calls:?}"
    );
    assert!(calls[0].starts_with("hang ") && calls[1].starts_with("fast "));
    assert_eq!(fast, deadline_reply(&calls[1]));

    let inits = log_lines(&init_log);
    assert_eq!(
        inits.len(),
        2,
        "the next call must run on a fresh child: {inits:?}"
    );
    let cancelled_pid: i32 = inits[0]
        .strip_prefix("init ")
        .and_then(|pid| pid.parse().ok())
        .expect("the init log records the child pid");
    tokio::time::timeout(STDIO_SHUTDOWN_GRACE + Duration::from_secs(1), async {
        // SAFETY: signal zero only checks whether the pid still exists.
        while unsafe { libc::kill(cancelled_pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the cancelled call's child must be terminated, not left running");
}

/// A Streamable HTTP server that reads the whole `tools/call` POST and then
/// drops the connection may have run it: the call runs once and the next
/// call succeeds on a rebuilt connection.
#[tokio::test]
async fn streamable_http_reset_after_tool_call_post_is_not_replayed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _lock = lock_mcp_loopback_tests().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let server_tool_calls = Arc::clone(&tool_calls);

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let tool_calls = Arc::clone(&server_tool_calls);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                let header_end = loop {
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                if headers.starts_with("GET ") {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await;
                    return;
                }
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let request_json: serde_json::Value =
                    serde_json::from_slice(&request[header_end..header_end + content_length])
                        .unwrap();
                let method = request_json["method"].as_str().unwrap_or("");
                let Some(id) = request_json.get("id").cloned() else {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        )
                        .await;
                    return;
                };
                let result = match method {
                    "initialize" => serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{ "name": "act", "inputSchema": {"type": "object"} }]
                    }),
                    "tools/call" => {
                        // The tool runs, then the connection drops before
                        // any reply is written.
                        if tool_calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                            drop(socket);
                            return;
                        }
                        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
                    }
                    _ => serde_json::json!({}),
                };
                let body =
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let mut server_config = test_server_config();
    server_config.command = None;
    server_config.url = Some(format!("http://{addr}/mcp"));
    server_config.connect_timeout = Some(10);
    server_config.execute_timeout = Some(10);
    let mut cfg = McpConfig::default();
    cfg.servers.insert("remote".to_string(), server_config);
    let mut pool = McpPool::new(cfg);

    let err = pool
        .call_tool("mcp_remote_act", serde_json::json!({}))
        .await
        .expect_err("a call whose connection dropped mid-flight must not be replayed");
    assert!(
        format!("{err:#}").contains("outcome unknown, not retried"),
        "unexpected error: {err:#}"
    );
    assert_eq!(tool_calls.load(AtomicOrdering::SeqCst), 1);

    let result = pool
        .call_tool("mcp_remote_act", serde_json::json!({}))
        .await
        .expect("the next call reconnects");
    assert_eq!(
        result,
        serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
    assert_eq!(tool_calls.load(AtomicOrdering::SeqCst), 2);

    server.abort();
}

// Executed both as an ordinary no-op test and as an isolated OS-process worker.
#[test]
fn mcp_transaction_child_worker() {
    let Some(path) = std::env::var_os("CW_MCP_TRANSACTION_TEST_PATH") else {
        return;
    };
    let path = PathBuf::from(path);
    let mode = std::env::var("CW_MCP_TRANSACTION_TEST_MODE").unwrap();
    if mode == "init" {
        init_config(&path, false).unwrap();
        return;
    }
    mutate_config(&path, None, |cfg| {
        if mode == "hold" {
            fs::write(path.with_extension("entered"), b"ready")?;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !path.with_extension("release").exists() {
                anyhow::ensure!(
                    std::time::Instant::now() < deadline,
                    "fixture release timed out"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let server: McpServerConfig =
            serde_json::from_value(serde_json::json!({"command":"fixture-command"}))?;
        cfg.servers.insert(mode.clone(), server);
        Ok(())
    })
    .unwrap();
}

fn mcp_transaction_spawn_worker(path: &Path, mode: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mcp::tests::mcp_transaction_child_worker",
            "--nocapture",
        ])
        .env("CW_MCP_TRANSACTION_TEST_PATH", path)
        .env("CW_MCP_TRANSACTION_TEST_MODE", mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn mcp_transaction_independent_process_writers_and_init_preserve_updates() {
    for mode in ["second", "init"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mcp.json");
        let first = mcp_transaction_spawn_worker(&path, "hold");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.with_extension("entered").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "first worker did not acquire lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // On Unix the competing process uses an alias of the same directory.
        #[cfg(unix)]
        let other_path = {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &alias).unwrap();
            alias.join("mcp.json")
        };
        #[cfg(not(unix))]
        let other_path = path.clone();
        let mut second = mcp_transaction_spawn_worker(&other_path, mode);
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            second.try_wait().unwrap().is_none(),
            "second writer must wait for shared lock"
        );
        fs::write(path.with_extension("release"), b"go").unwrap();
        for child in [first, second] {
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let cfg = load_config(&path).unwrap();
        assert!(cfg.servers.contains_key("hold"));
        if mode == "second" {
            assert!(cfg.servers.contains_key("second"));
        }
        assert!(
            !cfg.servers.contains_key("example"),
            "init must not overwrite a concurrent add"
        );
    }
}

#[test]
fn mcp_transaction_preserves_unknown_fields_alias_and_rejects_stale_revision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mcp.json");
    fs::write(&path, r#"{"owner_note":{"keep":true},"timeouts":{"connect_timeout":10,"custom":42},"mcpServers":{"one":{"command":"one","extension":{"keep":1}}}}"#).unwrap();
    let before = read_config_revision(&path).unwrap();
    let (_, after) = mutate_config(&path, Some(&before), |cfg| {
        cfg.servers.get_mut("one").unwrap().enabled = false;
        Ok(())
    })
    .unwrap();
    assert_ne!(before, after);
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["owner_note"]["keep"], true);
    assert_eq!(raw["timeouts"]["custom"], 42);
    assert_eq!(raw["mcpServers"]["one"]["extension"]["keep"], 1);
    assert!(raw.get("servers").is_none());
    let bytes = fs::read(&path).unwrap();
    let err = mutate_config(&path, Some(&before), |cfg| {
        cfg.servers.clear();
        Ok(())
    })
    .unwrap_err();
    assert!(err.is::<McpRevisionConflict>());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        mutate_config(&path, Some(&after), |_| Ok(())).unwrap().1,
        after
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "no-op must not rewrite the document"
    );
}

#[test]
fn mcp_transaction_fails_closed_for_malformed_document_and_symlink() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mcp.json");
    fs::write(&path, "{private-malformed-fixture").unwrap();
    let err = mutate_config(&path, None, |_| Ok(())).unwrap_err();
    assert!(!err.to_string().contains("private-malformed-fixture"));
    assert!(init_config(&path, true).is_err());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{private-malformed-fixture"
    );
    #[cfg(unix)]
    {
        let link = root.path().join("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(mutate_config(&link, None, |_| Ok(())).is_err());
        assert!(init_config(&link, true).is_err());
    }
}

#[test]
fn only_a_reviewed_plugin_read_only_hint_relaxes_approval() {
    let tool: McpTool = serde_json::from_value(serde_json::json!({
        "name": "page_snapshot",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "destructiveHint": false}
    }))
    .expect("annotated tool parses");
    assert_eq!(
        approval_hint_for(&tool, true),
        Some(McpToolApprovalHint::TrustedReadOnly)
    );
    // The same claim from a server no plugin review covers is not trusted.
    assert_eq!(approval_hint_for(&tool, false), None);

    let destructive: McpTool = serde_json::from_value(serde_json::json!({
        "name": "delete_rows",
        "annotations": {"readOnlyHint": true, "destructiveHint": true}
    }))
    .expect("annotated tool parses");
    // A tool that claims both keeps its prompt, from any server.
    assert_eq!(
        approval_hint_for(&destructive, true),
        Some(McpToolApprovalHint::Destructive)
    );
    assert_eq!(
        approval_hint_for(&destructive, false),
        Some(McpToolApprovalHint::Destructive)
    );

    let bare: McpTool = serde_json::from_value(serde_json::json!({"name": "echo"}))
        .expect("unannotated tool parses");
    assert_eq!(approval_hint_for(&bare, true), None);
}

fn stdio_server(args: Vec<String>) -> McpServerConfig {
    serde_json::from_value(serde_json::json!({ "command": "node", "args": args }))
        .expect("stdio server config")
}

#[test]
fn user_server_launching_the_computer_use_bundle_is_recognized() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bundle = dir
        .path()
        .join("Codewhale Computer Use.app/Contents/Resources/plugin");
    std::fs::create_dir_all(bundle.join("mcp")).expect("bundle dirs");
    std::fs::write(bundle.join("mcp/server.mjs"), "").expect("server");
    std::fs::write(bundle.join("plugin.json"), r#"{"name": "computer-use"}"#).expect("manifest");
    let script = bundle.join("mcp/server.mjs").to_string_lossy().to_string();
    assert_eq!(
        launches_computer_use_plugin(&stdio_server(vec![script.clone()])),
        Some(script)
    );

    // Another plugin's server with the same layout is not a duplicate.
    let other = dir.path().join("other-plugin");
    std::fs::create_dir_all(other.join("mcp")).expect("other dirs");
    std::fs::write(other.join("plugin.json"), r#"{"name": "browser-tools"}"#).expect("manifest");
    let other_script = other.join("mcp/server.mjs").to_string_lossy().to_string();
    assert_eq!(
        launches_computer_use_plugin(&stdio_server(vec![other_script])),
        None
    );

    // Without a readable manifest the bundle's path shape still counts.
    let shaped = "/opt/computer-use/mcp/server.mjs".to_string();
    assert_eq!(
        launches_computer_use_plugin(&stdio_server(vec![shaped.clone()])),
        Some(shaped)
    );
    assert_eq!(
        launches_computer_use_plugin(&stdio_server(vec!["/opt/tools/mcp/server.mjs".to_string()])),
        None
    );
}

#[test]
fn computer_use_duplicate_warning_needs_the_builtin_bundle_enabled() {
    // A user entry alone (no enabled built-in bundle) is never flagged.
    let mut config = McpConfig::default();
    config.servers.insert(
        "codewhale-cu".to_string(),
        stdio_server(vec!["/opt/computer-use/mcp/server.mjs".to_string()]),
    );
    assert!(duplicate_computer_use_servers(&config).is_empty());
}

#[test]
fn computer_use_duplicate_warning_names_user_copies_of_the_enabled_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugin_base = dir.path().join("plugins/computer-use");
    fs::create_dir_all(plugin_base.join("mcp")).expect("plugin dirs");
    fs::write(
        plugin_base.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"computer-use\"\nversion = \"1.0.0\"\n",
    )
    .expect("plugin manifest");
    let (_, authority) = active_plugin_fixture(&plugin_base);
    let mut builtin = stdio_server(vec![
        plugin_base
            .join("mcp/server.mjs")
            .to_string_lossy()
            .to_string(),
    ]);
    builtin.reviewed_plugin = Some(
        ReviewedPluginMcpSource::from_authority(
            authority,
            None,
            Arc::new(crate::plugins::HostEnvironment::default()),
        )
        .expect("reviewed source"),
    );

    let mut config = McpConfig::default();
    config
        .servers
        .insert("plugin-computer-use".to_string(), builtin);
    config.servers.insert(
        "codewhale-cu".to_string(),
        stdio_server(vec!["/opt/computer-use/mcp/server.mjs".to_string()]),
    );
    config.servers.insert(
        "browser-tools".to_string(),
        stdio_server(vec!["/opt/tools/mcp/server.mjs".to_string()]),
    );
    let mut disabled_copy = stdio_server(vec!["/srv/computer_use/mcp/server.mjs".to_string()]);
    disabled_copy.enabled = false;
    config.servers.insert("old-cu".to_string(), disabled_copy);

    // Only the enabled user copy is named, with the argument that gave it
    // away; the bundle itself, other plugins and disabled entries are not.
    assert_eq!(
        duplicate_computer_use_servers(&config),
        vec![(
            "codewhale-cu".to_string(),
            "/opt/computer-use/mcp/server.mjs".to_string()
        )]
    );

    // Disabling the built-in bundle removes the warning.
    config
        .servers
        .get_mut("plugin-computer-use")
        .expect("bundle entry")
        .enabled = false;
    assert!(duplicate_computer_use_servers(&config).is_empty());
}

/// Founder run: an `uvx mcp-proxy-for-aws` server answered `initialize` with
/// JSON-RPC -32602 and the only thing surfaced was
/// "MCP error in 'initialize': {...}". The proxy had explained itself on
/// stderr (expired AWS login). The error now says the server rejected the
/// handshake, names what was launched, and carries that last stderr line.
#[cfg(unix)]
#[tokio::test]
async fn initialize_rejection_names_the_server_command_and_its_stderr_reason() {
    let mut config = test_server_config();
    config.command = Some("sh".to_string());
    config.args = vec![
        "-c".to_string(),
        concat!(
            "read line; ",
            "echo 'LoginRefreshRequired: Please reauthenticate using aws login' 1>&2; ",
            "echo '{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"error\":{\"code\":-32602,\"message\":\"Invalid request parameters\"}}'; ",
            "sleep 5"
        )
        .to_string(),
    ];
    let error = McpConnection::connect_with_policy(
        "aws".to_string(),
        config,
        &McpTimeouts::default(),
        None,
    )
    .await
    .err()
    .expect("a JSON-RPC error on initialize ends the handshake");
    let text = format_mcp_error_for_display(&error);
    assert!(
        text.contains("MCP server 'aws' rejected initialize"),
        "{text}"
    );
    assert!(text.contains("command `sh`"), "{text}");
    assert!(text.contains("-32602"), "{text}");
    assert!(
        text.contains("server stderr: LoginRefreshRequired: Please reauthenticate using aws login"),
        "{text}"
    );
    // The founder's exact stderr is an AWS CLI `aws login` session, renewed
    // with `aws login` (not `aws sso login`).
    assert!(
        text.contains(
            "AWS credentials expired: run `aws login` in a terminal, then `/mcp retry aws`"
        ),
        "{text}"
    );
}

/// The file the founder had carried both `"disabled": true` and
/// `"enabled": false`; a hand edit can leave them disagreeing. Enabling (and
/// disabling) must always write the pair so the two keys agree.
#[test]
fn set_server_enabled_makes_the_enabled_and_disabled_keys_agree() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    fs::write(
        &path,
        r#"{"servers":{"linear":{"url":"https://mcp.linear.app/mcp","enabled":true,"disabled":true}}}"#,
    )
    .unwrap();
    assert!(!load_config(&path).unwrap().servers["linear"].is_enabled());

    set_server_enabled(&path, "linear", true).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["servers"]["linear"]["enabled"], serde_json::json!(true));
    assert_eq!(
        raw["servers"]["linear"]["disabled"],
        serde_json::json!(false)
    );
    assert!(load_config(&path).unwrap().servers["linear"].is_enabled());

    set_server_enabled(&path, "linear", false).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        raw["servers"]["linear"]["enabled"],
        serde_json::json!(false)
    );
    assert_eq!(
        raw["servers"]["linear"]["disabled"],
        serde_json::json!(true)
    );
}

/// Lazy boot (#6033) leaves an unselected server with no connection, no
/// failure and no observed capabilities. That is "never started" — its
/// recovery is `connect`, not `reconnect`, and an OAuth-capable one is not
/// yet known to need a login.
#[test]
fn never_started_server_recovers_with_connect_not_reconnect() {
    let snapshot = McpServerSnapshot {
        name: "lazy".into(),
        enabled: true,
        required: false,
        transport: "stdio".into(),
        command_or_url: "lazy-mcp".into(),
        connect_timeout: 5,
        execute_timeout: 5,
        read_timeout: 5,
        connected: false,
        error: None,
        auth_required: false,
        capability_metadata: McpServerCapabilityMetadata::NotObserved,
        tools: Vec::new(),
        resources: Vec::new(),
        prompts: Vec::new(),
    };
    assert!(!snapshot.started());
    assert_eq!(
        snapshot.recovery_kind(false),
        Some(McpRecoveryKind::Connect)
    );
    assert_eq!(snapshot.recovery_kind(true), Some(McpRecoveryKind::Connect));

    // A server that had a live connection and lost it is a reconnect.
    let dropped = McpServerSnapshot {
        capability_metadata: McpServerCapabilityMetadata::LegacyFallback,
        ..snapshot
    };
    assert!(dropped.started());
    assert_eq!(
        dropped.recovery_kind(false),
        Some(McpRecoveryKind::Reconnect)
    );
}

#[tokio::test]
async fn initialize_captures_sanitized_server_instructions() {
    let transport = ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "guided", "version": "1.0.0"},
                "capabilities": {"tools": {}},
                "instructions": "  Prefer search\u{7} before fetch.\r\n\tKeep queries short.\u{202E}  "
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.initialize().await.expect("initialize");
    assert_eq!(
        conn.instructions(),
        Some("Prefer search before fetch.\n\tKeep queries short.")
    );

    let transport = ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "verbose", "version": "1.0.0"},
                "capabilities": {"tools": {}},
                "instructions": "x".repeat(10_000)
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.initialize().await.expect("initialize");
    let capped = conn.instructions().expect("capped guidance");
    assert!(capped.len() <= codewhale_mcp::MAX_SERVER_INSTRUCTIONS_BYTES);
    assert!(capped.ends_with("[truncated]"));
}

#[tokio::test]
async fn initialize_ignores_non_string_instructions_without_failing_the_handshake() {
    let transport = ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([json_frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "odd", "version": "1.0.0"},
                "capabilities": {"tools": {}},
                "instructions": {"text": "not a string"}
            }
        }))]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.initialize()
        .await
        .expect("non-string instructions must not fail the handshake");
    assert_eq!(conn.instructions(), None);
}

#[tokio::test]
async fn mcp_server_instructions_exclude_servers_whose_tools_are_all_denied() {
    let mut pool = McpPool::new(McpConfig::default())
        .with_disallowed_tools(vec!["mcp_denied_write".to_string()]);
    pool.insert_test_connection("guided", &["search"], Some("Use search."));
    pool.insert_test_connection("denied", &["write"], Some("Denied guidance."));
    pool.insert_test_connection("hidden", &["read"], Some("Hidden guidance."));
    pool.insert_test_connection("silent", &["ping"], None);

    // `hidden` is allowed by the pool but absent from the turn's catalog.
    let visible = |name: &str| name != "mcp_hidden_read";
    assert_eq!(
        pool.model_server_instructions(visible),
        vec![("guided".to_string(), "Use search.".to_string())]
    );
}

#[tokio::test]
async fn computer_use_call_carries_an_attested_decision_only_when_a_person_approved() {
    let respond = |id: u64| {
        json_frame(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}))
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut connection = test_connection(Box::new(ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([respond(1), respond(2), respond(3)]),
    }));
    let key = [7_u8; 32];
    connection.decision_key = Some(key);
    let args = serde_json::json!({"action": "allow", "app": "Safari", "remember": 1.0});
    let decision = crate::core::engine::HumanDecision::for_test("mcp_fixture_consent", &args);
    connection
        .call_tool_decided("consent", args.clone(), 5, Some(&decision))
        .await
        .expect("decided call");
    connection
        .call_tool_decided("consent", args.clone(), 5, None)
        .await
        .expect("plain call");
    connection.decision_key = None;
    connection
        .call_tool_decided("consent", args.clone(), 5, Some(&decision))
        .await
        .expect("call without a shared key");

    let sent = sent.lock().unwrap().clone();
    let attested = &sent[0]["params"]["_meta"][COMPUTER_USE_DECISION_META];
    let nonce = attested["nonce"].as_str().expect("nonce");
    let args_json = attested["args_json"].as_str().expect("args_json");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(args_json).unwrap(),
        sent[0]["params"]["arguments"]
    );
    let message = [
        b"consent".as_slice(),
        b"\0",
        args_json.as_bytes(),
        b"\0",
        nonce.as_bytes(),
    ]
    .concat();
    let expected = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
        &message,
    );
    assert_eq!(attested["mac"], hex_encode(expected.as_ref()));
    assert!(sent[1]["params"].get("_meta").is_none(), "{}", sent[1]);
    assert!(sent[2]["params"].get("_meta").is_none(), "{}", sent[2]);
}

#[tokio::test]
async fn copied_human_decision_cannot_authorize_another_mcp_call() {
    let input = serde_json::json!({"action": "allow", "app": "Safari"});
    let decision = crate::core::engine::HumanDecision::for_test("mcp_fixture_consent", &input);
    let mut pool = McpPool::new(McpConfig::default());
    for (name, arguments) in [
        (
            "mcp_fixture_consent",
            serde_json::json!({"action": "allow", "app": "Terminal"}),
        ),
        ("mcp_other_consent", input.clone()),
    ] {
        let error = pool
            .call_tool_with_decision(name, arguments, Some(&decision))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not authorize this exact MCP call"),
            "{error:#}"
        );
    }
    assert!(decision.authorizes("mcp_fixture_consent", &input));
}

/// Actual shipped plugin, reviewed and staged by the normal plugin authority.
/// Every backend and state path is isolated; this fixture never drives a real app.
pub(crate) fn computer_use_test_fixture() -> (
    tempfile::TempDir,
    Arc<crate::plugins::PluginRegistry>,
    McpPool,
    String,
) {
    let root = tempfile::tempdir().expect("private fixture root");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let plugin_base = root.path().join("plugins/computer-use");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/computer-use");
    let mut pending = vec![(source, plugin_base.clone())];
    while let Some((from, to)) = pending.pop() {
        fs::create_dir_all(&to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let destination = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                pending.push((entry.path(), destination));
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
    let state = root.path().join("cu-state");
    let recordings = root.path().join("recordings");
    let fake = plugin_base.join("tests/fixtures/fake-backend.mjs");
    fs::write(
        plugin_base.join("plugin.toml"),
        format!(
            "schema_version = 1\n[plugin]\nname = \"computer-use\"\nversion = \"1.0.0\"\n\
             [mcp_servers.local]\ncommand = \"node\"\nargs = [\"mcp/server.mjs\"]\nconnect_timeout = 5\n\
             [mcp_servers.local.env]\nCODEWHALE_CU_APP = \"off\"\n\
             CODEWHALE_CU_STATE_DIR = {}\nCODEWHALE_CU_RECORDINGS_DIR = {}\nCODEWHALE_CU_TEST_BACKEND = {}\n",
            serde_json::to_string(&state.to_string_lossy()).unwrap(),
            serde_json::to_string(&recordings.to_string_lossy()).unwrap(),
            serde_json::to_string(&fake.to_string_lossy()).unwrap(),
        ),
    )
    .unwrap();
    let discovery = crate::plugins::discovery::DiscoveryConfig {
        workspace,
        user_plugins_dir: root.path().join("plugins"),
        workspace_plugins_dir: root.path().join("unused-workspace-plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path: root.path().join("plugin-state/state.json"),
    };
    let mut registry = crate::plugins::discovery::discover_with_config(&discovery);
    registry.trust("computer-use").unwrap();
    registry.enable("computer-use").unwrap();
    let plugin = registry.get("computer-use").unwrap().clone();
    let authority = registry.authority_for("computer-use").unwrap();
    let config = merge_plugin_mcp_servers_from_plugins(
        McpConfig::default(),
        vec![("computer-use".to_string(), plugin, authority)],
    )
    .unwrap();
    let server = config
        .servers
        .keys()
        .next()
        .expect("plugin MCP server")
        .clone();
    (root, Arc::new(registry), McpPool::new(config), server)
}

#[tokio::test]
async fn computer_use_real_plugin_host_handshake_rejects_tamper_replay_and_late_keys() {
    // The bundled Computer Use plugin applies only to macOS hosts; elsewhere
    // there is no live plugin to start.
    if !cfg!(target_os = "macos") {
        return;
    }
    let _env = crate::test_support::lock_test_env();
    let (root, _registry, mut pool, server) = computer_use_test_fixture();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let connection = pool
        .get_or_connect(&server)
        .await
        .expect("actual reviewed Node plugin");
    let key = connection
        .decision_key
        .expect("first-message key from the host");
    let args = serde_json::json!({"scope":"foreground", "remember":false});
    let decision = crate::core::engine::HumanDecision::for_test("mcp_fixture_consent_allow", &args);
    let decode = |result: serde_json::Value| {
        serde_json::from_str::<serde_json::Value>(result["content"][0]["text"].as_str().unwrap())
            .unwrap()
    };
    let unsigned = decode(
        connection
            .call_tool_decided("consent_allow", args.clone(), 5, None)
            .await
            .unwrap(),
    );
    assert_eq!(
        unsigned["error"]["code"], "consent_needs_user",
        "{unsigned}"
    );
    let approved = decode(
        connection
            .call_tool_decided("consent_allow", args.clone(), 5, Some(&decision))
            .await
            .unwrap(),
    );
    assert_eq!(approved["ok"], true, "{approved}");

    // A later key notification must not replace the first connection key.
    let wrong_key = [8_u8; 32];
    connection.send(serde_json::json!({"jsonrpc":"2.0","method":COMPUTER_USE_HOST_KEYS_METHOD,"params":{"decision_key":hex_encode(&wrong_key)}})).await.unwrap();
    let fresh = attest_decision(&key, "consent_revoke", &args).unwrap();
    for (tool, arguments, tag) in [
        (
            "consent_revoke",
            args.clone(),
            attest_decision(&wrong_key, "consent_revoke", &args).unwrap(),
        ),
        (
            "consent_revoke",
            serde_json::json!({"scope":"foreground","remember":true}),
            fresh.clone(),
        ),
        ("consent_allow", args.clone(), fresh.clone()),
    ] {
        let rejected = decode(connection.call_method("tools/call", serde_json::json!({"name":tool,"arguments":arguments,"_meta":{COMPUTER_USE_DECISION_META:tag}}), 5).await.unwrap());
        assert_eq!(
            rejected["error"]["code"], "consent_needs_user",
            "{rejected}"
        );
    }
    let exact = serde_json::json!({"name":"consent_revoke","arguments":args,"_meta":{COMPUTER_USE_DECISION_META:fresh}});
    let allowed = decode(
        connection
            .call_method("tools/call", exact.clone(), 5)
            .await
            .unwrap(),
    );
    assert_eq!(
        allowed["ok"], true,
        "the original key remains authoritative: {allowed}"
    );
    let replay = decode(
        connection
            .call_method("tools/call", exact, 5)
            .await
            .unwrap(),
    );
    assert_eq!(replay["error"]["code"], "consent_needs_user", "{replay}");
    pool.shutdown_all().await;
}

#[tokio::test]
async fn discover_all_rejects_combined_item_budget_without_publishing_any_family() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let tools = (0..MAX_MCP_CATALOG_ITEMS)
        .map(|i| serde_json::json!({"name": format!("tool_{i}"), "inputSchema": {}}))
        .collect::<Vec<_>>();
    let transport = ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses: VecDeque::from([
            json_frame(serde_json::json!({"jsonrpc":"2.0", "id":1, "result":{"tools":tools}})),
            json_frame(
                serde_json::json!({"jsonrpc":"2.0", "id":2, "result":{"resources":[{"uri":"file:///over-limit", "name":"over-limit"}]}}),
            ),
        ]),
    };
    let mut conn = test_connection(Box::new(transport));
    conn.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: true,
        prompts: true,
    });
    conn.tools = vec![
        serde_json::from_value(serde_json::json!({"name":"previous", "inputSchema":{}})).unwrap(),
    ];
    conn.resources = vec![
        serde_json::from_value(serde_json::json!({"uri":"file:///previous", "name":"previous"}))
            .unwrap(),
    ];
    conn.resource_templates = vec![
        serde_json::from_value(
            serde_json::json!({"uriTemplate":"file:///{key}", "name":"previous"}),
        )
        .unwrap(),
    ];
    conn.prompts = vec![serde_json::from_value(serde_json::json!({"name":"previous"})).unwrap()];

    let error = conn
        .discover_all()
        .await
        .expect_err("combined count must refuse");
    assert!(
        error
            .to_string()
            .contains("resources/list exceeded the 4096-item")
    );
    assert_eq!(conn.tools.len(), 1);
    assert_eq!(conn.tools[0].name, "previous");
    assert_eq!(conn.resources[0].uri, "file:///previous");
    assert_eq!(conn.resource_templates[0].name, "previous");
    assert_eq!(conn.prompts[0].name, "previous");
    assert_eq!(
        sent.lock().unwrap().len(),
        2,
        "poisoned budget must prevent subsequent RPCs"
    );
}

#[tokio::test]
async fn discover_all_rejects_combined_page_budget_without_publishing() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut responses = VecDeque::new();
    for page in 1..=MAX_MCP_CATALOG_PAGES {
        let mut result = serde_json::json!({"tools":[]});
        if page < MAX_MCP_CATALOG_PAGES {
            result["nextCursor"] = serde_json::json!(format!("page_{page}"));
        }
        responses.push_back(json_frame(
            serde_json::json!({"jsonrpc":"2.0", "id":page, "result":result}),
        ));
    }
    responses.push_back(json_frame(serde_json::json!({"jsonrpc":"2.0", "id":MAX_MCP_CATALOG_PAGES+1, "result":{"resources":[]}})));
    let mut conn = test_connection(Box::new(ScriptedValueTransport {
        sent: Arc::clone(&sent),
        responses,
    }));
    conn.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: true,
        prompts: false,
    });
    conn.tools = vec![
        serde_json::from_value(serde_json::json!({"name":"previous", "inputSchema":{}})).unwrap(),
    ];
    let error = conn
        .discover_all()
        .await
        .expect_err("page budget must span families");
    assert!(
        error
            .to_string()
            .contains("resources/list exceeded the 64-page")
    );
    assert_eq!(conn.tools[0].name, "previous");
    assert_eq!(sent.lock().unwrap().len(), MAX_MCP_CATALOG_PAGES + 1);
}

#[test]
fn catalog_cursor_refusal_is_scoped_by_family_and_latched() {
    let mut budget = McpCatalogBudget::new();
    let page = serde_json::json!({"nextCursor":"shared-value"});
    budget.observe_page("tools/list", &page, 0).unwrap();
    budget.observe_page("resources/list", &page, 0).unwrap();
    let error = budget.observe_page("tools/list", &page, 0).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("tools/list repeated pagination cursor")
    );
    let error = budget
        .observe_page("prompts/list", &serde_json::json!({}), 0)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("tools/list repeated pagination cursor")
    );
}

#[test]
fn catalog_byte_budget_cannot_reset_between_families() {
    let mut budget = McpCatalogBudget::new();
    // Put the real shared counter at the boundary without a 32MiB allocation.
    let page = serde_json::json!({});
    budget.bytes = MAX_MCP_CATALOG_BYTES - serde_json::to_vec(&page).unwrap().len();
    budget.observe_page("tools/list", &page, 0).unwrap();
    let error = budget.observe_page("resources/list", &page, 0).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("resources/list exceeded the 33554432-byte aggregate")
    );
    assert!(budget.ensure_available().is_err());
}

#[tokio::test]
async fn discover_all_success_replaces_unavailable_old_families() {
    let mut conn = test_connection(Box::new(ScriptedValueTransport {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: VecDeque::from([json_frame(
            serde_json::json!({"jsonrpc":"2.0", "id":1, "result":{"tools":[]}}),
        )]),
    }));
    conn.server_capabilities = Some(McpServerCapabilities {
        tools: true,
        resources: false,
        prompts: false,
    });
    conn.resources = vec![
        serde_json::from_value(serde_json::json!({"uri":"file:///old", "name":"old"})).unwrap(),
    ];
    conn.prompts = vec![serde_json::from_value(serde_json::json!({"name":"old"})).unwrap()];
    conn.discover_all().await.unwrap();
    assert!(conn.tools.is_empty());
    assert!(conn.resources.is_empty());
    assert!(conn.prompts.is_empty());
}

/// The selected Host and Rust default share the same real Rust OAuth store,
/// refresh endpoint and connection recovery. Only loopback fixture credentials
/// exist; no provider/browser or token bytes cross the extension RPC wire.
#[tokio::test(flavor = "current_thread")]
async fn http_backend_parity_reactive_refresh_reuses_exact_facade_id_once() {
    let _env = crate::test_support::lock_test_env();
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;
    let node = crate::extension_host::tests::node_for_tests("HTTP OAuth Host parity")
        .expect("Host parity requires Node");
    for backend in [McpBackend::Rust, McpBackend::Host] {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(true);
        let manager = Arc::new(crate::extension_host::ExtensionHostManager::new(
            crate::extension_host::ExtensionHostOptions {
                root: Some(dir.path().to_path_buf()),
                node_override: Some(node.clone()),
                ..Default::default()
            },
        ));
        let _manager = crate::extension_host::TestManagerGuard::install(Arc::clone(&manager));
        let mock = OAuthMcpMock::spawn().await;
        let url = mock.url();
        // The local expiry trusts this token; only the explicit peer 401 may
        // force refresh. A token-expiry fixture would not exercise this arm.
        seed_oauth_tokens(
            "wikiserver",
            &url,
            "cw-unaccepted-but-unexpired",
            "rt-fresh",
            Some(millis_from_now(3_600_000)),
        );
        let mut pool = McpPool::new(McpConfig {
            servers: [("wikiserver".into(), mock_oauth_server_config(mock.addr))].into(),
            ..Default::default()
        })
        .with_backend(backend);
        assert!(pool.connect_all().await.is_empty(), "{backend:?}");
        assert_eq!(mock.token_requests.load(AtomicOrdering::SeqCst), 1);
        let frames = mock.frames.lock().unwrap().clone();
        let initializes: Vec<_> = frames
            .iter()
            .filter(|frame| frame["method"] == "initialize")
            .collect();
        assert_eq!(initializes.len(), 2);
        assert_eq!(
            initializes[0], initializes[1],
            "same admitted frame, exact original Rust ID and params"
        );
        assert_eq!(initializes[0]["id"], "1");
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["method"] == "notifications/initialized")
                .count(),
            1
        );
        pool.call_tool("mcp_wikiserver_wiki_lookup", json!({}))
            .await
            .unwrap();
        assert_eq!(
            mock.frames
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| frame["method"] == "tools/call")
                .count(),
            1
        );
        pool.shutdown_all().await;
        manager.shutdown().await;
        mock.task.abort();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn shared_http_refresh_revalidates_authority_before_any_second_write() {
    let _env = crate::test_support::lock_test_env();
    let dir = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let _loopback = lock_mcp_loopback_tests().await;
    let mock = OAuthMcpMock::spawn().await;
    let config = mock_oauth_server_config(mock.addr);
    let url = mock.url();
    seed_oauth_tokens(
        "wikiserver",
        &url,
        "cw-unaccepted-but-unexpired",
        "rt-fresh",
        Some(millis_from_now(3_600_000)),
    );
    let runtime = oauth::McpOAuthRuntime::from_server_config(
        "wikiserver",
        &config,
        reqwest::header::HeaderMap::new(),
    )
    .await
    .unwrap();
    let client = super::http_client::McpHttpClient::new(
        &url,
        false,
        false,
        false,
        None,
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .unwrap()
    .with_mcp_auth(McpHttpAuth::from_config("wikiserver", &config, runtime));
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let error = client
        .send_mcp_request(
            client.post(&url).body(
                json!({"jsonrpc":"2.0","id":"original-rust-id","method":"initialize","params":{}})
                    .to_string(),
            ),
            true,
            false,
            true,
            || {
                // This is the actual post-refresh validation point, not a mock
                // success result: the token endpoint really answered first.
                if mock.token_requests.load(AtomicOrdering::SeqCst) != 0 {
                    checks.fetch_add(1, AtomicOrdering::SeqCst);
                    anyhow::bail!("fixture authority withdrawn during refresh");
                }
                Ok(())
            },
            |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("authority withdrawn"));
    assert_eq!(checks.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(mock.token_requests.load(AtomicOrdering::SeqCst), 1);
    let frames = mock.frames.lock().unwrap();
    assert_eq!(
        frames.len(),
        1,
        "no second MCP network write after withdrawal"
    );
    assert_eq!(frames[0]["id"], "original-rust-id");
    drop(frames);
    mock.task.abort();
}

#[test]
fn configured_mcp_search_matches_real_names_and_bounds_the_admitted_set() {
    let mut config = McpConfig::default();
    for index in 0..12 {
        config.servers.insert(
            format!("server{index:02}"),
            serde_json::from_value(json!({"command":"unused"})).unwrap(),
        );
    }
    let pool = McpPool::new(config);
    assert!(
        pool.configured_servers_for_search(".*", "regex", |_| true)
            .unwrap()
            .is_empty()
    );
    assert!(
        pool.configured_servers_for_search("unrelated search", "bm25", |_| true)
            .unwrap()
            .is_empty()
    );
    let names = pool
        .configured_servers_for_search("mcp_.*", "regex", |name| name >= "server04")
        .unwrap();
    assert_eq!(names.len(), 8);
    assert_eq!(names[0], "server04");
    assert_eq!(names[7], "server11");
    assert_eq!(
        pool.configured_servers_for_search("^mcp_server05_actual_method$", "regex", |_| true)
            .unwrap(),
        ["server05"]
    );
    assert!(
        pool.configured_servers_for_search("mcp_[", "regex", |_| true)
            .is_err()
    );
}

#[test]
fn configured_mcp_search_excludes_disabled_and_namespace_denied_servers() {
    let mut config = McpConfig::default();
    config.servers.insert(
        "enabled".into(),
        serde_json::from_value(json!({"command":"unused"})).unwrap(),
    );
    config.servers.insert(
        "disabled".into(),
        serde_json::from_value(json!({"command":"unused", "enabled":false})).unwrap(),
    );
    config.servers.insert(
        "denied".into(),
        serde_json::from_value(json!({"command":"unused"})).unwrap(),
    );
    let pool = McpPool::new(config).with_disallowed_tools(vec!["mcp_denied_*".into()]);
    assert_eq!(
        pool.configured_servers_for_search("mcp_.*", "regex", |_| true)
            .unwrap(),
        ["enabled"]
    );
    // The list named to the model passes the same gates, plus the turn's.
    assert_eq!(pool.discoverable_configured_servers(|_| true), ["enabled"]);
    assert!(
        pool.discoverable_configured_servers(|name| name != "enabled")
            .is_empty()
    );
}

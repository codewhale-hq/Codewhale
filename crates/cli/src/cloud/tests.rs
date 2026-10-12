use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use clap::Parser;
use codewhale_secrets::account::{
    ACCOUNT_SESSION_SCHEMA_VERSION, AccountSession as AuthSession,
    account_auth_slot as cloud_auth_slot,
};
use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
use serde_json::json;

use super::*;
use crate::{Cli, Commands};

struct FakeTransport {
    responses: Mutex<VecDeque<CloudResponse>>,
    requests: Mutex<Vec<CloudRequest>>,
}

impl FakeTransport {
    fn new(responses: Vec<CloudResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, Vec<CloudRequest>> {
        self.requests.lock().unwrap()
    }
}

impl CloudTransport for FakeTransport {
    fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("fake transport exhausted"))
    }
}

fn response(status: u16, body: serde_json::Value) -> CloudResponse {
    CloudResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        retry_after: None,
    }
}

fn response_retry_after(status: u16, body: serde_json::Value, seconds: u64) -> CloudResponse {
    CloudResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        retry_after: Some(seconds),
    }
}

fn sse_response(events: &[serde_json::Value]) -> CloudResponse {
    let body = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {}\nid: {}\n\n",
                event["type"].as_str().unwrap(),
                event,
                event["seq"].as_u64().unwrap()
            )
        })
        .collect::<String>()
        .into_bytes();
    CloudResponse {
        status: 200,
        body,
        retry_after: None,
    }
}

fn account(id: &str) -> serde_json::Value {
    json!({
        "user": {
            "id": id,
            "displayName": "Hunter",
            "email": "hunter@example.test",
            "plan": "free",
            "modelKeys": {}
        }
    })
}

/// A stand-in for `GET /api/model-providers`.
///
/// Deliberately includes ids the retired hardcoded enum never knew
/// (`modelstudio-coding-plan`) so a test failure means the CLI went back to a
/// compiled provider list.
fn catalog() -> serde_json::Value {
    json!({
        "providers": [
            {
                "id": "openai",
                "label": "OpenAI",
                "runtimeProvider": "openai",
                "availability": "account_key",
                "connectionAvailable": true
            },
            {
                "id": "anthropic",
                "label": "Anthropic",
                "runtimeProvider": "anthropic",
                "availability": "account_key",
                "connectionAvailable": true
            },
            {
                "id": "xiaomi",
                "label": "Xiaomi MiMo",
                "runtimeProvider": "xiaomi-mimo",
                "availability": "account_key",
                "connectionAvailable": true
            },
            {
                "id": "modelstudio-coding-plan",
                "label": "Alibaba Model Studio Coding Plan",
                "runtimeProvider": "modelstudio-coding-plan",
                "availability": "account_key",
                "connectionAvailable": true
            }
        ]
    })
}

fn auth(access: &str, refresh: &str, account_id: &str) -> AuthBundle {
    AuthBundle {
        token_type: "Bearer".to_string(),
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        session: Some(AuthSession {
            id: "session-1".to_string(),
            provider: "github".to_string(),
            expires_at: String::new(),
            refresh_expires_at: String::new(),
            ..AuthSession::default()
        }),
        user: Some(CloudUser {
            id: account_id.to_string(),
            display_name: "Hunter".to_string(),
            email: "hunter@example.test".to_string(),
            ..CloudUser::default()
        }),
    }
}

fn auth_json(access: &str, refresh: &str, account_id: &str) -> serde_json::Value {
    serde_json::to_value(auth(access, refresh, account_id)).unwrap()
}

fn test_secrets() -> (Secrets, Arc<InMemoryKeyringStore>) {
    let store = Arc::new(InMemoryKeyringStore::new());
    (Secrets::new(store.clone()), store)
}

fn test_config() -> (tempfile::TempDir, ConfigStore) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let config = ConfigStore::load(Some(path)).unwrap();
    (temp, config)
}

fn command(argv: &[&str]) -> CloudCommand {
    let cli = Cli::try_parse_from(argv).unwrap();
    let Some(Commands::Account(args)) = cli.command else {
        panic!("expected account command");
    };
    args.command
}

fn computer(id: &str, status: &str) -> serde_json::Value {
    json!({
        "id": id,
        "ownerId": "acct-123",
        "name": "Account pilot",
        "region": "us-west",
        "status": status,
        "startQueueReason": if status == "queued" { "active_limit" } else { "" }
    })
}

fn agent(id: &str, name: &str) -> serde_json::Value {
    json!({ "id": id, "name": name, "status": "active", "projectId": "project-codewhale", "revision": 1 })
}

fn agent_thread(id: &str, agent_id: &str, title: &str) -> serde_json::Value {
    json!({
        "id": id,
        "agentId": agent_id,
        "projectId": "project-codewhale",
        "title": title,
        "model": "deepseek-flash",
        "modelProvider": "deepseek",
        "modelProviderId": "",
        "kind": "conversation",
        "archivedAt": ""
    })
}

fn route_catalog() -> serde_json::Value {
    json!({
        "providers": [{
            "id": "deepseek",
            "label": "DeepSeek",
            "runtimeProvider": "deepseek",
            "availability": "account_key",
            "connectionAvailable": true,
            "models": ["deepseek-v4-pro", "deepseek-flash"]
        }]
    })
}

#[test]
fn parses_cloud_command_matrix_and_rejects_inline_keys() {
    assert!(matches!(
        command(&["codewhale", "account", "status"]),
        CloudCommand::Status
    ));
    assert!(matches!(
        command(&["codewhale", "account", "projects", "list"]),
        CloudCommand::Projects(CloudProjectsArgs {
            command: CloudProjectsCommand::List { json: false }
        })
    ));
    assert!(matches!(
        command(&["codewhale", "account", "github", "bindings"]),
        CloudCommand::Github(CloudGithubArgs {
            command: CloudGithubCommand::Bindings { json: false }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "computers",
            "create",
            "Trial",
            "--computer-trial",
            "--eu-compute-opt-in"
        ]),
        CloudCommand::Computers(CloudComputersArgs {
            command: CloudComputersCommand::Create {
                computer_trial: true,
                eu_compute_opt_in: true,
                ..
            }
        })
    ));
    for lone_flag in ["--computer-trial", "--eu-compute-opt-in"] {
        assert!(
            Cli::try_parse_from([
                "codewhale",
                "account",
                "computers",
                "create",
                "Trial",
                lone_flag
            ])
            .is_err()
        );
    }
    assert!(
        Cli::try_parse_from(["codewhale", "account", "computers", "usage", "computer-1"]).is_err()
    );
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "projects",
            "create",
            "My Project",
            "--repo-binding-id",
            "github:acct-123:987:owner/repo",
            "--operation-key",
            "project-create-1",
        ]),
        CloudCommand::Projects(CloudProjectsArgs {
            command: CloudProjectsCommand::Create { .. }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "agents",
            "bind-project",
            "Whale",
            "project-codewhale"
        ]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::BindProject { .. }
        })
    ));
    assert!(matches!(
        command(&["codewhale", "cloud", "login", "--no-open"]),
        CloudCommand::Login(CloudLoginArgs { no_open: true, .. })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "cloud",
            "keys",
            "set",
            "xiaomi-mimo",
            "--from-local"
        ]),
        CloudCommand::Keys(CloudKeysArgs {
            command: CloudKeysCommand::Set(CloudKeySetArgs {
                from_local: true,
                ..
            })
        })
    ));
    // Provider ids are open strings validated against the account's catalog,
    // not a compiled enum: clap must not reject an id this CLI never heard of.
    assert!(matches!(
        command(&[
            "codewhale",
            "cloud",
            "keys",
            "set",
            "modelstudio-coding-plan"
        ]),
        CloudCommand::Keys(CloudKeysArgs {
            command: CloudKeysCommand::Set(CloudKeySetArgs { .. })
        })
    ));
    assert!(
        Cli::try_parse_from([
            "codewhale",
            "cloud",
            "keys",
            "set",
            "openai",
            "sk-unsafe-inline"
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "codewhale",
            "cloud",
            "keys",
            "set",
            "openai",
            "--from-local",
            "--api-key-stdin"
        ])
        .is_err()
    );
    assert!(reject_inline_api_key(None).is_ok());
    let error = reject_inline_api_key(Some("sk-never-render")).unwrap_err();
    assert!(error.to_string().contains("--api-key-stdin"));
    assert!(!error.to_string().contains("sk-never-render"));
}

#[test]
fn api_base_requires_https_or_literal_loopback_http() {
    assert_eq!(
        validate_api_base("https://api.codewhale.net/")
            .unwrap()
            .display,
        "https://api.codewhale.net"
    );
    assert!(validate_api_base("http://127.0.0.1:8787").is_ok());
    assert!(validate_api_base("http://[::1]:8787").is_ok());
    assert!(validate_api_base("http://api.codewhale.net").is_err());
    assert!(validate_api_base("https://user:secret@example.test").is_err());
    assert!(validate_api_base("https://example.test/prefix").is_err());
}

#[test]
fn verification_urls_are_pinned_to_the_app_or_loopback() {
    const CODE: &str = "ABCD-EFGH-JKLM";
    const API: &str = "https://api.codewhale.net";
    assert!(
        validate_verification_url("https://app.codewhale.net/cli/authorize", API, CODE, false,)
            .is_ok()
    );
    assert!(
        validate_verification_url(
            "https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM",
            API,
            CODE,
            true,
        )
        .is_ok()
    );
    for unsafe_url in [
        "https://attacker.example/cli/authorize",
        "https://user@app.codewhale.net/cli/authorize",
        "https://app.codewhale.net/cli/authorize#continue",
        "https://app.codewhale.net/cli/authorize/extra",
        "https://app.codewhale.net/cli/other/../authorize",
        "https://app.codewhale.net/cli/%61uthorize",
        "https://app.codewhale.net/cli/authorize?next=https%3A%2F%2Fattacker.example",
        "https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM&next=evil",
    ] {
        assert!(
            validate_verification_url(unsafe_url, API, CODE, unsafe_url.contains("user_code"))
                .is_err(),
            "accepted unsafe URL: {unsafe_url}"
        );
    }
    assert!(
        validate_verification_url(
            "http://localhost:3000/cli/authorize?user_code=ABCD-EFGH-JKLM",
            "http://127.0.0.1:8787",
            CODE,
            true,
        )
        .is_ok()
    );
    assert!(
        validate_verification_url(
            "https://staging-app.example/cli/authorize",
            "https://staging-api.example",
            CODE,
            false,
        )
        .is_err()
    );
}

#[test]
fn user_codes_and_key_inputs_match_the_server_contract() {
    assert!(validate_user_code("ABCD-EFGH-JKLM").is_ok());
    for invalid in [
        "CW-1234",
        "ABCI-EFGH-JKLM",
        "ABCO-EFGH-JKLM",
        "ABC1-EFGH-JKLM",
        "abcd-EFGH-JKLM",
        "ABCD_EFGH_JKLM",
    ] {
        assert!(validate_user_code(invalid).is_err(), "accepted {invalid}");
    }

    assert!(validate_device_code("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_ok());
    for invalid in [
        "too-short",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA!",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(validate_device_code(invalid).is_err(), "accepted {invalid}");
    }

    assert!(validate_api_key("1234567").is_err());
    assert!(validate_api_key("12345678").is_ok());
    assert!(validate_api_key(&"x".repeat(4096)).is_ok());
    assert!(validate_api_key(&"x".repeat(4097)).is_err());
    assert!(validate_api_key(&"é".repeat(4)).is_ok());
    assert!(validate_api_key("1234567\n8").is_err());
    assert_eq!(
        parse_key_input(format!("{}\n", "x".repeat(4096)).into_bytes()).unwrap(),
        "x".repeat(4096)
    );
    assert!(parse_key_input(vec![b'x'; MAX_API_KEY_STDIN_BYTES as usize + 1]).is_err());
    assert_eq!(
        validate_label("  Codewhale\tCLI  ").unwrap(),
        "Codewhale CLI"
    );
    assert!(validate_label(&"x".repeat(80)).is_ok());
    assert!(validate_label(&"x".repeat(81)).is_err());
}

#[test]
fn device_flow_handles_pending_then_authorized_without_printing_tokens() {
    for (no_open, browser_opens) in [(false, true), (false, false), (true, false)] {
        let (temp, mut config) = test_config();
        let _keep_temp = temp;
        let (secrets, _) = test_secrets();
        let transport = FakeTransport::new(vec![
            response(
                200,
                json!({
                    "deviceCode": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    "userCode": "ABCD-EFGH-JKLM",
                    "verificationUri": "https://app.codewhale.net/cli/authorize",
                    "verificationUriComplete": "https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM",
                    "expiresIn": 600,
                    "interval": 1
                }),
            ),
            response(202, json!({ "status": "authorization_pending" })),
            response(
                200,
                auth_json("access-never-print", "refresh-never-print", "acct-123"),
            ),
            response(200, account("acct-123")),
        ]);
        let mut output = Vec::new();
        let mut key_reader = |_| bail!("key reader should not be called");
        let mut opened = Vec::new();
        let mut opener = |url: String| {
            opened.push(url);
            browser_opens
        };
        let mut sleeper = |_| {};
        run_with(
            command(if no_open {
                &["codewhale", "cloud", "login", "--no-open"]
            } else {
                &["codewhale", "cloud", "login"]
            }),
            "work",
            "https://api.codewhale.net",
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output.lines().find(|line| line.starts_with("Open: ")),
            Some("Open: https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM")
        );
        assert!(output.contains("ABCD-EFGH-JKLM"));
        assert!(output.contains("Account ID: acct-123"));
        assert!(output.contains("Profile: work"));
        // No-brand invariant: login signs in the account; the internal
        // cloud-agent credential is never taught here.
        assert!(!output.to_lowercase().contains("daytona"), "{output}");
        assert!(!output.contains("set-slot"), "{output}");
        assert!(!output.contains("access-never-print"));
        assert!(!output.contains("refresh-never-print"));
        assert!(!output.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        if no_open {
            assert!(opened.is_empty());
        } else {
            assert_eq!(
                opened,
                ["https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM"]
            );
        }
        assert_eq!(
            output.contains("Browser could not be opened; use the URL and code above."),
            !no_open && !browser_opens
        );
        let requests = transport.requests();
        assert_eq!(requests[0].path, "/api/cli/device/start");
        assert_eq!(requests[1].path, "/api/cli/device/token");
        assert_eq!(requests[2].path, "/api/cli/device/token");
        assert_eq!(requests[3].path, "/api/me");
    }
}

fn login_responses() -> Vec<CloudResponse> {
    vec![
        response(
            200,
            json!({
                "deviceCode": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "userCode": "ABCD-EFGH-JKLM",
                "verificationUri": "https://app.codewhale.net/cli/authorize",
                "verificationUriComplete": "https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM",
                "expiresIn": 600,
                "interval": 1
            }),
        ),
        response(
            200,
            auth_json("access-never-print", "refresh-never-print", "acct-123"),
        ),
        response(200, account("acct-123")),
    ]
}

fn run_login(config: &mut ConfigStore, secrets: &Secrets) -> String {
    let transport = FakeTransport::new(login_responses());
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("key reader should not be called");
    let mut opener = |_: String| false;
    let mut sleeper = |_| {};
    run_with(
        command(&["codewhale", "cloud", "login", "--no-open"]),
        "work",
        "https://api.codewhale.net",
        config,
        secrets,
        secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn login_selects_managed_route_when_provider_is_default() {
    let (temp, mut config) = test_config();
    assert_eq!(config.config.provider, ProviderKind::default());
    let (secrets, _) = test_secrets();
    let output = run_login(&mut config, &secrets);
    assert!(
        output.contains("Using your Codewhale account route"),
        "{output}"
    );
    let saved = ConfigStore::load(Some(temp.path().join("config.toml"))).unwrap();
    assert_eq!(saved.config.provider, ProviderKind::Codewhale);
    assert_eq!(saved.config.model.as_deref(), Some("auto"));
}

#[test]
fn login_keeps_explicitly_configured_route() {
    let (temp, mut config) = test_config();
    config.config.provider = ProviderKind::Openai;
    config.save().unwrap();
    let (secrets, _) = test_secrets();
    let output = run_login(&mut config, &secrets);
    assert!(
        output.contains("Keeping your configured openai route."),
        "{output}"
    );
    let saved = ConfigStore::load(Some(temp.path().join("config.toml"))).unwrap();
    assert_eq!(saved.config.provider, ProviderKind::Openai);
    assert_eq!(saved.config.model, None);
}

#[test]
fn login_keeps_an_explicit_deepseek_route() {
    // DeepSeek is the default provider; choosing it explicitly must survive sign-in.
    let (temp, _) = test_config();
    std::fs::write(temp.path().join("config.toml"), "provider = \"deepseek\"\n").unwrap();
    let mut config = ConfigStore::load(Some(temp.path().join("config.toml"))).unwrap();
    let (secrets, _) = test_secrets();
    let output = run_login(&mut config, &secrets);
    assert!(
        output.contains("Keeping your configured deepseek route."),
        "{output}"
    );
    let saved = ConfigStore::load(Some(temp.path().join("config.toml"))).unwrap();
    assert_eq!(saved.config.provider, ProviderKind::Deepseek);
    assert_eq!(saved.config.model, None);
}

#[test]
fn login_keeps_the_default_route_when_it_has_a_local_key() {
    let (temp, mut config) = test_config();
    let (secrets, _) = test_secrets();
    secrets.set("deepseek", "sk-local-never-print").unwrap();
    let output = run_login(&mut config, &secrets);
    assert!(
        output.contains("Keeping your configured deepseek route."),
        "{output}"
    );
    let saved = ConfigStore::load(Some(temp.path().join("config.toml"))).unwrap();
    assert_eq!(saved.config.provider, ProviderKind::Deepseek);
}

#[test]
fn cloud_sessions_are_isolated_by_profile_and_api_origin() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![]);
    let default = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
    let work = CloudClient::new(&transport, &secrets, "work", "https://api.codewhale.net");
    let local = CloudClient::new(&transport, &secrets, "default", "http://127.0.0.1:8787");
    default
        .save_auth(auth("a-default", "r-default", "acct-default"))
        .unwrap();
    work.save_auth(auth("a-work", "r-work", "acct-work"))
        .unwrap();
    local
        .save_auth(auth("a-local", "r-local", "acct-local"))
        .unwrap();

    assert_eq!(
        default
            .load_auth()
            .unwrap()
            .unwrap()
            .bundle
            .user
            .unwrap()
            .id,
        "acct-default"
    );
    assert_eq!(
        work.load_auth().unwrap().unwrap().bundle.user.unwrap().id,
        "acct-work"
    );
    assert_eq!(
        local.load_auth().unwrap().unwrap().bundle.user.unwrap().id,
        "acct-local"
    );
}

#[test]
fn status_refreshes_once_on_unauthorized_and_never_displays_tokens() {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(401, json!({ "code": "access_token_expired" })),
        response(
            200,
            auth_json("access-new-secret", "refresh-new-secret", "acct-refresh"),
        ),
        response(200, account("acct-refresh")),
    ]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth(
            "access-old-secret",
            "refresh-old-secret",
            "acct-refresh",
        ))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    run_with(
        CloudCommand::Status,
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("acct-refresh"));
    for secret in [
        "access-old-secret",
        "refresh-old-secret",
        "access-new-secret",
        "refresh-new-secret",
    ] {
        assert!(!output.contains(secret));
    }
    let requests = transport.requests();
    assert_eq!(requests[0].path, "/api/me");
    assert_eq!(requests[1].path, "/api/auth/refresh");
    assert_eq!(requests[2].path, "/api/me");
}

#[test]
fn account_pull_refuses_to_claim_unimplemented_local_import() {
    let (temp, mut config) = test_config();
    let config_path = config.path().to_path_buf();
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![]);
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};

    let error = run_with(
        command(&["codewhale", "account", "pull"]),
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .expect_err("non-dry-run pull must fail until settings import exists");

    assert!(error.to_string().contains("import is not available"));
    assert!(error.to_string().contains("local config was not changed"));
    assert!(
        output.is_empty(),
        "a rejected pull must not print success text"
    );
    assert!(
        transport.requests().is_empty(),
        "a rejected pull needs no API call"
    );
    assert!(
        !config_path.exists(),
        "a rejected pull must not create config.toml"
    );
    drop(temp);
}

#[test]
fn account_pull_dry_run_is_truthful_and_read_only() {
    let (temp, mut config) = test_config();
    let config_path = config.path().to_path_buf();
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(200, account("acct-pull"))]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-pull"))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};

    run_with(
        command(&["codewhale", "account", "pull", "--dry-run"]),
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Account settings (pull --dry-run):"));
    assert!(output.contains("Account ID: acct-pull"));
    assert!(output.contains("remote settings import is not available"));
    assert!(output.contains("local config unchanged"));
    assert!(!output.contains("Pulled account document"));
    assert!(!output.contains("would hydrate"));
    assert!(!output.contains("access-secret"));
    assert!(!output.contains("refresh-secret"));
    assert!(!config_path.exists(), "dry-run must not create config.toml");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].method == HttpMethod::Get);
    assert_eq!(requests[0].path, "/api/me");
    drop(temp);
}

#[test]
fn non_terminal_refresh_responses_preserve_the_local_session() {
    for status in [403, 429, 500, 503] {
        let (secrets, _) = test_secrets();
        let transport = FakeTransport::new(vec![
            response(401, json!({ "code": "access_token_expired" })),
            response(status, json!({ "code": "temporarily_unavailable" })),
        ]);
        let client = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
        client
            .save_auth(auth(
                "access-old-secret",
                "refresh-still-valid",
                "acct-refresh",
            ))
            .unwrap();

        let error = client
            .me()
            .err()
            .expect("refresh response should fail the request")
            .to_string();
        assert!(error.contains(&format!("HTTP {status}")));
        assert_eq!(
            client
                .load_auth()
                .unwrap()
                .expect("retryable refresh failure must preserve the session")
                .bundle
                .refresh_token,
            "refresh-still-valid"
        );
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/api/me");
        assert_eq!(requests[1].path, "/api/auth/refresh");
    }
}

/// Revoke is defined as idempotent by the control plane (a repeat returns the
/// identical `revokedAt`), so a rate-limited revoke may be replayed — and the
/// server's own `Retry-After` decides how long CI waits, not a guess.
#[test]
fn a_rate_limited_revoke_waits_the_server_named_interval_and_replays() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response_retry_after(
            429,
            json!({
                "error": "rate_limited",
                "message": "slow down",
                "details": { "code": "rate_limited" }
            }),
            4,
        ),
        response(
            200,
            json!({ "ok": true, "apiKey": { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "revokedAt": "2026-02-02T00:00:00Z" } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
    client
        .save_auth(auth("access-secret", "refresh-secret", "acct-1"))
        .unwrap();
    let mut slept = Vec::new();
    let mut sleeper = |duration: Duration| slept.push(duration);
    let response = client
        .execute_authenticated_with_retry(
            HttpMethod::Delete,
            "/api/account/api-keys/3f2a9c1e4b7d8a0f5c6e2b91",
            None,
            machine::Retry::Idempotent,
            &mut sleeper,
        )
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(slept, vec![Duration::from_secs(4)]);
    assert_eq!(transport.requests().len(), 2);
}

/// The one POST in this surface mints a secret shown exactly once. A replay
/// that actually succeeded server-side would leave a key the caller can never
/// revoke by id, so `Retry::Never` must mean never — even on a 429.
#[test]
fn a_rate_limited_create_is_never_replayed() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response_retry_after(
        429,
        json!({
            "error": "rate_limited",
            "message": "slow down",
            "details": { "code": "rate_limited" }
        }),
        4,
    )]);
    let client = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
    client
        .save_auth(auth("access-secret", "refresh-secret", "acct-1"))
        .unwrap();
    let mut sleeper = |_: Duration| panic!("create must never sleep-and-retry");
    let response = client
        .execute_authenticated_with_retry(
            HttpMethod::Post,
            "/api/account/api-keys",
            Some(b"{}".to_vec()),
            machine::Retry::Never,
            &mut sleeper,
        )
        .unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn refresh_transport_failure_preserves_the_local_session() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        401,
        json!({ "code": "access_token_expired" }),
    )]);
    let client = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
    client
        .save_auth(auth(
            "access-old-secret",
            "refresh-still-valid",
            "acct-refresh",
        ))
        .unwrap();

    let error = client
        .me()
        .err()
        .expect("refresh transport should fail")
        .to_string();
    assert!(error.contains("fake transport exhausted"));
    assert_eq!(
        client
            .load_auth()
            .unwrap()
            .expect("transport failure must preserve the session")
            .bundle
            .refresh_token,
        "refresh-still-valid"
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/api/auth/refresh");
}

#[test]
fn terminal_refresh_auth_failures_clear_the_local_session() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(401, json!({ "code": "access_token_expired" })),
        response(401, json!({ "code": "invalid_refresh_token" })),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net");
    client
        .save_auth(auth(
            "access-old-secret",
            "refresh-terminal-secret",
            "acct-refresh",
        ))
        .unwrap();

    let error = client
        .me()
        .err()
        .expect("terminal refresh response should fail the request")
        .to_string();
    assert!(error.contains("session expired"));
    assert!(
        client.load_auth().unwrap().is_none(),
        "HTTP 401 must clear the terminal session"
    );
}

#[test]
fn set_list_and_remove_use_account_routes_without_secret_output() {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, _) = test_secrets();
    let list_account = json!({
        "user": {
            "id": "acct-keys",
            "displayName": "Hunter",
            "email": "hunter@example.test",
            "modelKeys": {
                "openai": { "configured": true, "label": "Laptop", "updatedAt": "now" }
            }
        }
    });
    let transport = FakeTransport::new(vec![
        // set: /api/me, catalog, PUT
        response(200, account("acct-keys")),
        response(200, catalog()),
        response(200, json!({ "ok": true })),
        // list: /api/me, catalog
        response(200, list_account),
        response(200, catalog()),
        // remove: /api/me, catalog, DELETE
        response(200, account("acct-keys")),
        response(200, catalog()),
        response(204, json!(null)),
    ]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-keys"))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| Ok("sk-provider-never-print".to_string());
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    for cmd in [
        command(&[
            "codewhale",
            "cloud",
            "keys",
            "set",
            "openai",
            "--api-key-stdin",
            "--label",
            "Laptop",
        ]),
        command(&["codewhale", "cloud", "keys", "list"]),
        command(&["codewhale", "cloud", "keys", "remove", "openai"]),
    ] {
        run_with(
            cmd,
            "default",
            "https://api.codewhale.net",
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();
    }
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("openai: set"));
    // Every catalog provider is listed, including ids the retired enum lacked.
    assert!(output.contains("modelstudio-coding-plan: not set"));
    assert!(output.contains("Alibaba Model Studio Coding Plan"));
    assert!(!output.contains("Laptop"));
    assert!(output.contains("Codewhale account acct-keys"));
    assert!(!output.contains("sk-provider-never-print"));
    assert!(!output.contains("access-secret"));
    assert!(!output.contains("refresh-secret"));

    let requests = transport.requests();
    let put = requests
        .iter()
        .find(|request| request.method == HttpMethod::Put)
        .unwrap();
    assert_eq!(put.path, "/api/model-keys/openai");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(put.body.as_ref().unwrap()).unwrap(),
        json!({ "key": "sk-provider-never-print", "label": "Laptop" })
    );
    assert!(requests.iter().any(|request| {
        request.method == HttpMethod::Delete && request.path == "/api/model-keys/openai"
    }));
}

#[test]
fn from_local_uses_config_without_printing_or_requiring_an_inline_key() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let mut config = ConfigStore::load(Some(path)).unwrap();
    config.config.providers.anthropic.api_key = Some("sk-local-upload-secret".to_string());
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(200, account("acct-local")),
        response(200, catalog()),
        response(200, json!({ "ok": true })),
    ]);
    CloudClient::new(&transport, &secrets, "work", "https://api.codewhale.net")
        .save_auth(auth("access", "refresh", "acct-local"))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("from-local must not prompt");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    run_with(
        command(&[
            "codewhale",
            "cloud",
            "keys",
            "set",
            "anthropic",
            "--from-local",
        ]),
        "work",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("acct-local"));
    assert!(!output.contains("sk-local-upload-secret"));
    let requests = transport.requests();
    let put = requests
        .iter()
        .find(|request| request.method == HttpMethod::Put)
        .unwrap();
    assert!(String::from_utf8_lossy(put.body.as_ref().unwrap()).contains("sk-local-upload-secret"));
}

#[test]
fn catalog_ids_map_to_local_providers_through_the_catalog_not_a_compiled_table() {
    // `xiaomi` is the control plane's route id; `xiaomi-mimo` is the runtime's.
    // The catalog states that mapping, so `--from-local` must read it from the
    // response rather than from a compiled slug table.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let mut config = ConfigStore::load(Some(path)).unwrap();
    config.config.providers.xiaomi_mimo.api_key = Some("sk-mimo-local".to_string());
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(200, account("acct-map")),
        response(200, catalog()),
        response(200, json!({ "ok": true })),
    ]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access", "refresh", "acct-map"))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("from-local must not prompt");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    run_with(
        command(&[
            "codewhale",
            "cloud",
            "keys",
            "set",
            "xiaomi",
            "--from-local",
        ]),
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("sk-mimo-local"), "{output}");
    let requests = transport.requests();
    let put = requests
        .iter()
        .find(|request| request.method == HttpMethod::Put)
        .expect("a PUT to the catalog route id");
    assert_eq!(put.path, "/api/model-keys/xiaomi");
    assert!(String::from_utf8_lossy(put.body.as_ref().unwrap()).contains("sk-mimo-local"));
}

#[test]
fn an_id_outside_the_account_catalog_is_refused_and_names_what_is_available() {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(200, account("acct-unknown")),
        response(200, catalog()),
    ]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access", "refresh", "acct-unknown"))
        .unwrap();
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("an unknown provider must not prompt for a key");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    let error = run_with(
        command(&["codewhale", "cloud", "keys", "remove", "not-a-provider"]),
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .expect_err("an id the account cannot connect must fail");
    let text = error.to_string();
    assert!(text.contains("modelstudio-coding-plan"), "{text}");
    assert!(
        !transport
            .requests()
            .iter()
            .any(|request| request.method == HttpMethod::Delete),
        "an unknown id must never reach a mutating route"
    );
}

#[test]
fn provider_ids_are_validated_before_they_can_reach_a_url_path() {
    validate_provider_id("modelstudio-coding-plan").unwrap();
    validate_provider_id("  deepseek  ").unwrap();
    for bad in [
        "",
        "-leading",
        "Upper",
        "has_underscore",
        "../escape",
        "with/slash",
        &"a".repeat(65),
    ] {
        assert!(
            validate_provider_id(bad).is_err(),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn from_local_uses_config_before_the_provider_secret_store() {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, store) = test_secrets();
    store.set("openai", "sk-secret-store").unwrap();

    assert_eq!(
        resolve_local_key(&config, &secrets, ProviderKind::Openai)
            .unwrap()
            .as_deref(),
        Some("sk-secret-store")
    );
    config.config.providers.openai.api_key = Some("sk-config-first".to_string());
    assert_eq!(
        resolve_local_key(&config, &secrets, ProviderKind::Openai)
            .unwrap()
            .as_deref(),
        Some("sk-config-first")
    );
}

#[test]
fn logout_recovers_from_a_corrupt_local_session_record() {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, store) = test_secrets();
    let slot = cloud_auth_slot("default", "https://api.codewhale.net");
    store.set(&slot, "not-json-and-not-a-token").unwrap();
    let transport = FakeTransport::new(vec![]);
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    run_with(
        CloudCommand::Logout,
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
    .unwrap();
    assert!(store.get(&slot).unwrap().is_none());
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("not-json-and-not-a-token")
    );
}

#[test]
fn logout_clears_obsolete_or_wrong_origin_session_records() {
    let canonical_api_base = "https://api.codewhale.net";
    for (case, schema_version, stored_api_base) in [
        (
            "obsolete schema",
            ACCOUNT_SESSION_SCHEMA_VERSION.saturating_add(1),
            canonical_api_base,
        ),
        (
            "wrong origin",
            ACCOUNT_SESSION_SCHEMA_VERSION,
            "https://other.codewhale.net",
        ),
    ] {
        let (secrets, store) = test_secrets();
        let slot = cloud_auth_slot("default", canonical_api_base);
        let raw = serde_json::to_string(&StoredCloudAuth {
            schema_version,
            api_base: stored_api_base.to_string(),
            bundle: auth("access-obsolete", "refresh-obsolete", "acct-obsolete"),
        })
        .unwrap();
        store.set(&slot, &raw).unwrap();
        let transport = FakeTransport::new(vec![]);
        let client = CloudClient::new(&transport, &secrets, "default", canonical_api_base);

        assert!(
            client.load_auth().unwrap().is_none(),
            "{case} must continue to load as signed out"
        );
        assert!(!client.logout().unwrap());
        assert!(
            store.get(&slot).unwrap().is_none(),
            "logout must scrub the {case} record"
        );
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn server_errors_never_echo_response_messages() {
    let error = response_error(&response(
        400,
        json!({
            "error": {
                "code": "invalid_api_key",
                "message": "The submitted key was sk-never-echo-this"
            }
        }),
    ))
    .to_string();
    assert!(error.contains("invalid_api_key"));
    assert!(!error.contains("sk-never-echo-this"));
}

#[test]
fn cloud_auth_slot_does_not_embed_profile_or_origin() {
    let slot = cloud_auth_slot("private-profile", "https://api.codewhale.net");
    assert!(!slot.contains("private-profile"));
    assert!(!slot.contains("api.codewhale.net"));
    assert_ne!(
        slot,
        cloud_auth_slot("other-profile", "https://api.codewhale.net")
    );
}

#[test]
fn fake_store_is_profile_safe() {
    let (_, store) = test_secrets();
    store.set("unrelated", "keep-me").unwrap();
    store.delete("missing").unwrap();
    assert_eq!(store.get("unrelated").unwrap().as_deref(), Some("keep-me"));
}

#[test]
fn account_login_timeout_fails_the_command() {
    // §2.3 / #5033 class: a timed-out device login printed the timeout yet the
    // process exited 0. Pin the contract at the run_with seam — the command
    // must return Err so run_cli maps it to ExitCode::FAILURE. Verified live
    // against a stub server: `error: Codewhale account login timed out` now
    // exits 1.
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let (secrets, _) = test_secrets();
    // Device start succeeds once; every token poll stays pending forever.
    struct PendingLogin;
    impl CloudTransport for PendingLogin {
        fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
            if request.path == "/api/cli/device/start" {
                return Ok(response(
                    200,
                    json!({
                        "deviceCode": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                        "userCode": "ABCD-EFGH-JKLM",
                        "verificationUri": "https://app.codewhale.net/cli/authorize",
                        "verificationUriComplete": "https://app.codewhale.net/cli/authorize?user_code=ABCD-EFGH-JKLM",
                        "expiresIn": 600,
                        "interval": 1
                    }),
                ));
            }
            Ok(response(202, json!({ "status": "authorization_pending" })))
        }
    }
    let pending = PendingLogin;
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("key reader should not be called");
    let mut opener = |_| true;
    // A real (short) sleep keeps the pending loop from busy-spinning while
    // still reaching the 1s client timeout quickly.
    let mut sleeper = |duration: std::time::Duration| {
        std::thread::sleep(duration.min(std::time::Duration::from_millis(50)))
    };
    let result = run_with(
        command(&[
            "codewhale",
            "cloud",
            "login",
            "--no-open",
            "--timeout-seconds",
            "1",
        ]),
        "default",
        "https://api.codewhale.net",
        &mut config,
        &secrets,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &pending,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    );
    let err = match result {
        Ok(()) => panic!("a timed-out login must return Err so the exit code is non-zero"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("login timed out"),
        "timeout error text: {err}"
    );
}

// ---------------------------------------------------------------------------
// Machine-token command surface, end to end through `run_with`.
// ---------------------------------------------------------------------------

const MACHINE_TOKEN: &str =
    "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_-xQRST";

fn machine_env() -> machine::MachineKeyEnv {
    machine::MachineKeyEnv::from_raw(Some(MACHINE_TOKEN))
}

/// Drive one `codewhale account …` invocation with a scripted transport.
fn run_account(
    argv: &[&str],
    machine: &machine::MachineKeyEnv,
    secrets: &Secrets,
    transport: &FakeTransport,
) -> (Result<()>, String) {
    let (temp, mut config) = test_config();
    let _keep_temp = temp;
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("key reader should not be called");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    let result = run_with(
        command(argv),
        "default",
        "https://api.codewhale.net",
        &mut config,
        secrets,
        secrets,
        machine,
        transport,
        &mut output,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    );
    (result, String::from_utf8(output).unwrap())
}

#[test]
fn whoami_with_a_machine_key_uses_the_key_route_and_never_the_session_route() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        200,
        json!({
            "account": { "id": "user_1", "displayName": "Hunter", "email": "h@example.test",
                         "region": "us-west", "plan": "free" },
            "apiKey": { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "name": "github-actions",
                        "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
                        "scopes": ["account:read", "agent:run"],
                        "createdAt": "2026-01-01T00:00:00Z" },
            "agent": { "configured": true, "modelProvider": "deepseek" }
        }),
    )]);
    let (result, output) = run_account(
        &["codewhale", "account", "whoami"],
        &machine_env(),
        &secrets,
        &transport,
    );
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/account/api-key/whoami");
    // Exactly one credential on the wire, and it is the machine key.
    assert_eq!(requests[0].bearer.as_deref(), Some(MACHINE_TOKEN));
    assert!(output.contains("user_1"), "{output}");
    assert!(
        output.contains("cwc_key_3f2a9c1e4b7d8a0f5c6e2b91"),
        "{output}"
    );
    assert!(
        !output.contains(&MACHINE_TOKEN[32..]),
        "secret half leaked: {output}"
    );
}

/// The load-bearing failure mode: a machine credential that fails must not
/// quietly become a human one, or CI runs as the wrong identity.
#[test]
fn a_rejected_machine_key_never_falls_back_to_the_stored_session() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        401,
        json!({
            "error": "unauthorized",
            "message": "invalid key",
            "details": { "code": "api_key_invalid" }
        }),
    )]);
    // A perfectly good interactive session exists alongside the bad key.
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, output) = run_account(
        &["codewhale", "account", "whoami"],
        &machine_env(),
        &secrets,
        &transport,
    );
    let err = result.expect_err("an invalid machine key must fail the command");
    assert_eq!(
        transport.requests().len(),
        1,
        "there must be no second attempt"
    );
    assert!(output.is_empty(), "nothing should be printed: {output}");
    let machine_error = err
        .downcast_ref::<machine::MachineError>()
        .expect("the failure must carry a class");
    assert_eq!(machine_error.exit_code, machine::EXIT_AUTH);
    assert!(err.to_string().contains("is not valid"), "{err}");
    // The human session is untouched: a bad key is not a reason to log anyone out.
    assert!(
        CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
            .load_auth()
            .unwrap()
            .is_some()
    );
}

#[test]
fn managing_keys_with_only_a_machine_key_is_refused_before_anything_is_sent() {
    let (secrets, _) = test_secrets();
    for argv in [
        vec!["codewhale", "account", "api-keys", "list"],
        vec!["codewhale", "account", "api-keys", "create", "--name", "ci"],
        vec![
            "codewhale",
            "account",
            "api-keys",
            "revoke",
            "3f2a9c1e4b7d8a0f5c6e2b91",
        ],
    ] {
        let transport = FakeTransport::new(Vec::new());
        let (result, output) = run_account(&argv, &machine_env(), &secrets, &transport);
        let err = result.expect_err("a key cannot manage keys");
        assert!(
            transport.requests().is_empty(),
            "{argv:?} put the key on the wire"
        );
        assert!(output.is_empty(), "{argv:?}: {output}");
        assert!(
            err.to_string()
                .contains("Managing API keys needs an interactive login."),
            "{argv:?}: {err}"
        );
    }
}

#[test]
fn create_use_saves_the_key_only_in_the_local_codewhale_slot() {
    let (secrets, keyring) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        201,
        json!({
            "apiKey": { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "name": "laptop",
                        "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
                        "scopes": ["account:read", "agent:run", "models:infer"],
                        "createdAt": "2026-01-01T00:00:00Z", "expiresAt": null },
            "secret": MACHINE_TOKEN
        }),
    )]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, output) = run_account(
        &[
            "codewhale",
            "account",
            "api-keys",
            "create",
            "--name",
            "laptop",
            "--scope",
            "models:infer",
            "--use",
        ],
        &machine::MachineKeyEnv::default(),
        &secrets,
        &transport,
    );
    result.unwrap();
    // The secret is still printed exactly once, and the local save is stated.
    assert_eq!(output.matches(MACHINE_TOKEN).count(), 1, "{output}");
    assert!(
        output.contains("local `codewhale` provider credential"),
        "{output}"
    );
    assert_eq!(
        keyring.get("codewhale").unwrap().as_deref(),
        Some(MACHINE_TOKEN),
        "--use must write the codewhale provider slot"
    );
    // Only one request: --use is a local write, never an upload.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value =
        serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["scopes"], json!(["models:infer"]), "{body}");
}

#[test]
fn creating_a_key_prints_the_secret_exactly_once_and_saves_it_nowhere() {
    let (secrets, keyring) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        201,
        json!({
            "apiKey": { "id": "3f2a9c1e4b7d8a0f5c6e2b91", "name": "github-actions",
                        "displayPrefix": "cwc_key_3f2a9c1e4b7d8a0f5c6e2b91",
                        "scopes": ["account:read", "agent:run"],
                        "createdAt": "2026-01-01T00:00:00Z", "expiresAt": null },
            "secret": MACHINE_TOKEN
        }),
    )]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, output) = run_account(
        &[
            "codewhale",
            "account",
            "api-keys",
            "create",
            "--name",
            "github-actions",
            "--expires-in-days",
            "90",
        ],
        &machine::MachineKeyEnv::default(),
        &secrets,
        &transport,
    );
    result.unwrap();
    assert_eq!(output.matches(MACHINE_TOKEN).count(), 1, "{output}");
    assert!(
        output.contains("ONLY TIME YOU WILL SEE THIS SECRET"),
        "{output}"
    );

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/account/api-keys");
    let body: serde_json::Value =
        serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["name"], "github-actions");
    assert_eq!(body["expiresInDays"], 90);
    // Scopes omitted means every scope, stated explicitly so a key carries
    // exactly what this CLI's help promised.
    assert_eq!(
        body["scopes"],
        json!(["account:read", "agent:run", "models:infer"]),
        "{body}"
    );

    // The plaintext exists in one response and nowhere else, ever: creating a
    // key must not write it into the session record on its way past.
    let stored = keyring
        .get(&cloud_auth_slot("default", "https://api.codewhale.net"))
        .unwrap()
        .expect("the session record is still there");
    assert!(
        !stored.contains(MACHINE_TOKEN),
        "the secret reached storage"
    );
}

#[test]
fn a_bad_key_name_is_rejected_locally_without_a_round_trip() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(Vec::new());
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, _) = run_account(
        &[
            "codewhale",
            "account",
            "api-keys",
            "create",
            "--name",
            "bad*name",
        ],
        &machine::MachineKeyEnv::default(),
        &secrets,
        &transport,
    );
    let err = result.expect_err("`*` is outside the server's name pattern");
    assert!(transport.requests().is_empty());
    assert!(
        err.to_string().contains("only letters, digits, spaces"),
        "{err}"
    );
}

#[test]
fn the_agent_precondition_surfaces_the_409_with_its_own_exit_class() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(
        409,
        json!({
            "error": "conflict",
            "message": "no agent model",
            "details": { "code": "account_agent_model_unconfigured" }
        }),
    )]);
    let (result, _) = run_account(
        &["codewhale", "account", "agent"],
        &machine_env(),
        &secrets,
        &transport,
    );
    let err = result.expect_err("machine work needs a model");
    let machine_error = err.downcast_ref::<machine::MachineError>().unwrap();
    // A configuration problem, not a credential problem — and CI must be able
    // to tell them apart from the exit code alone.
    assert_eq!(machine_error.exit_code, machine::EXIT_AGENT_UNCONFIGURED);
    assert_ne!(machine_error.exit_code, machine::EXIT_AUTH);
    assert_eq!(transport.requests().len(), 1, "409 must not be retried");
    assert!(
        err.to_string().contains("codewhale account keys set"),
        "{err}"
    );
}

#[test]
fn the_agent_command_has_no_session_fallback() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(Vec::new());
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, _) = run_account(
        &["codewhale", "account", "agent"],
        &machine::MachineKeyEnv::default(),
        &secrets,
        &transport,
    );
    let err = result.expect_err("the agent route is machine-key-only");
    assert!(transport.requests().is_empty());
    assert!(err.to_string().contains("CODEWHALE_API_KEY"), "{err}");
}

#[test]
fn whoami_without_a_machine_key_still_reports_the_interactive_session() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![response(200, account("acct-human"))]);
    CloudClient::new(&transport, &secrets, "default", "https://api.codewhale.net")
        .save_auth(auth("access-secret", "refresh-secret", "acct-human"))
        .unwrap();
    let (result, output) = run_account(
        &["codewhale", "account", "whoami"],
        &machine::MachineKeyEnv::default(),
        &secrets,
        &transport,
    );
    result.unwrap();
    assert_eq!(transport.requests()[0].path, "/api/me");
    assert!(output.contains("acct-human"), "{output}");
}

#[test]
fn the_machine_token_surface_is_a_different_noun_from_the_provider_vault() {
    // `account keys` is the BYOK provider vault; `account api-keys` is the
    // machine tokens. Merging them would let one typo revoke the wrong thing.
    assert!(matches!(
        command(&["codewhale", "account", "keys", "list"]),
        CloudCommand::Keys(_)
    ));
    assert!(matches!(
        command(&["codewhale", "account", "api-keys", "list"]),
        CloudCommand::ApiKeys(_)
    ));
}

struct ConcurrentSessionTransport {
    inner: FakeTransport,
    trigger: &'static str,
    hold_writes: bool,
    writes: std::sync::mpsc::Sender<()>,
    done: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl CloudTransport for ConcurrentSessionTransport {
    fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
        if request.path == self.trigger {
            self.writes.send(()).unwrap();
            if self.hold_writes {
                assert!(
                    matches!(
                        self.done
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_millis(100)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ),
                    "account writer bypassed lifecycle transaction"
                );
            } else {
                self.done
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }
        self.inner.execute(request)
    }
}

#[test]
fn delayed_account_responses_never_replace_or_clear_a_new_sign_in() {
    for (operation, status) in [
        ("refresh", 200),
        ("refresh", 401),
        ("logout", 200),
        ("me", 200),
    ] {
        let (secrets, _) = test_secrets();
        let owner = AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE);
        owner
            .save(auth("old-access", "old-refresh", "old-account"))
            .unwrap();
        let (write_tx, write_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            write_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            owner
                .save(auth("new-access", "new-refresh", "new-account"))
                .unwrap();
            done_tx.send(()).unwrap();
        });
        let (trigger, responses) = match operation {
            "refresh" => (
                "/api/auth/refresh",
                vec![
                    response(401, json!({})),
                    response(
                        status,
                        auth_json("rotated-access", "rotated-refresh", "old-account"),
                    ),
                ],
            ),
            "logout" => ("/api/auth/logout", vec![response(status, json!({}))]),
            "me" => ("/api/me", vec![response(status, account("old-account"))]),
            _ => unreachable!(),
        };
        let transport = ConcurrentSessionTransport {
            inner: FakeTransport::new(responses),
            trigger,
            hold_writes: operation != "me",
            writes: write_tx,
            done: Mutex::new(done_rx),
        };
        let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
        if operation == "logout" {
            assert!(client.logout().unwrap());
        } else {
            assert!(client.me().is_err());
        }
        writer.join().unwrap();
        let current = client.load_auth().unwrap().unwrap();
        assert_eq!(current.bundle.access_token, "new-access");
        assert_eq!(current.bundle.refresh_token, "new-refresh");
        assert_eq!(current.bundle.user.unwrap().id, "new-account");
        assert_eq!(
            transport.inner.requests().len(),
            if operation == "refresh" {
                if status == 200 { 3 } else { 2 }
            } else {
                1
            }
        );
    }
}

#[test]
fn renewed_credentials_survive_retry_transport_failure() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(401, json!({})),
        response(
            200,
            auth_json("renewed-access", "renewed-refresh", "account"),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    client
        .save_auth(auth("old-access", "old-refresh", "account"))
        .unwrap();
    assert!(client.me().is_err());
    let current = client.load_auth().unwrap().unwrap();
    assert_eq!(current.bundle.access_token, "renewed-access");
    assert_eq!(current.bundle.refresh_token, "renewed-refresh");
}

#[test]
fn concurrent_clients_spend_refresh_token_only_once() {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    struct Transport {
        first_reads: AtomicUsize,
        refreshes: AtomicUsize,
        barrier: Barrier,
    }
    impl CloudTransport for Transport {
        fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
            if request.path == "/api/auth/refresh" {
                self.refreshes.fetch_add(1, Ordering::SeqCst);
                return Ok(response(
                    200,
                    auth_json("new-access", "new-refresh", "account"),
                ));
            }
            if self.first_reads.fetch_add(1, Ordering::SeqCst) < 2 {
                self.barrier.wait();
                Ok(response(401, json!({})))
            } else {
                Ok(response(200, account("account")))
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let secrets = Secrets::new(Arc::new(codewhale_secrets::FileKeyringStore::new(
        dir.path().join("secrets.json"),
    )));
    let transport = Transport {
        first_reads: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
        barrier: Barrier::new(2),
    };
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    client
        .save_auth(auth("old-access", "old-refresh", "account"))
        .unwrap();
    let successes = std::thread::scope(|scope| {
        let a = scope
            .spawn(|| CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE).me());
        let b = scope
            .spawn(|| CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE).me());
        [a.join().unwrap(), b.join().unwrap()]
            .into_iter()
            .filter(Result::is_ok)
            .count()
    });
    assert_eq!(successes, 1);
    assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        client.load_auth().unwrap().unwrap().bundle.refresh_token,
        "new-refresh"
    );
}
#[test]
fn logout_preserves_custody_until_server_confirms_revocation_or_dead_session() {
    for status in [
        None,
        Some(429),
        Some(500),
        Some(503),
        Some(200),
        Some(204),
        Some(401),
        Some(403),
    ] {
        let (secrets, _) = test_secrets();
        let responses = status
            .map(|code| vec![response(code, json!({}))])
            .unwrap_or_default();
        let transport = FakeTransport::new(responses);
        let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
        client
            .save_auth(auth("access-revoke", "refresh-revoke", "account"))
            .unwrap();
        let result = client.logout();
        if status.is_some_and(|s| (200..300).contains(&s) || matches!(s, 401 | 403)) {
            assert!(result.is_ok());
            assert!(client.load_auth().unwrap().is_none());
        } else {
            assert!(result.is_err());
            assert_eq!(
                client.load_auth().unwrap().unwrap().bundle.refresh_token,
                "refresh-revoke"
            );
        }
    }
}

#[test]
fn account_computers_use_the_same_account_api_and_report_queued_starts() {
    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    let (_temp, mut config) = test_config();
    let (secrets, _) = test_secrets();
    let store = AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE);
    store
        .save(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut queued = computer(ID, "suspended");
    queued["startQueueReason"] = json!("active_limit");
    let transport = FakeTransport::new(vec![
        response(200, json!({"computers": [computer(ID, "suspended")]})),
        response(200, json!({"computer": computer(ID, "suspended")})),
        response(200, json!({"computer": computer(ID, "suspended")})),
        response(202, json!({"computer": queued, "queued": true})),
        response(200, json!({"computer": computer(ID, "suspended")})),
        response(200, json!({"deleted": true, "computerId": ID})),
    ]);
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    let commands = [
        vec!["codewhale", "account", "computers", "list"],
        vec![
            "codewhale",
            "account",
            "computers",
            "create",
            "Account pilot",
        ],
        vec!["codewhale", "account", "computers", "show", ID],
        vec!["codewhale", "account", "computers", "start", ID],
        vec!["codewhale", "account", "computers", "pause", ID],
        vec![
            "codewhale",
            "account",
            "computers",
            "delete",
            ID,
            "--discard-files",
        ],
    ];
    for argv in commands {
        run_with(
            command(&argv),
            "default",
            DEFAULT_API_BASE,
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();
    }
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/api/computers",
            "/api/computers",
            "/api/computers/123e4567-e89b-42d3-a456-426614174000",
            "/api/computers/123e4567-e89b-42d3-a456-426614174000/start",
            "/api/computers/123e4567-e89b-42d3-a456-426614174000/pause",
            "/api/computers/123e4567-e89b-42d3-a456-426614174000",
        ]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.bearer.as_deref() == Some("access-secret"))
    );
    assert!(requests[0].method == HttpMethod::Get);
    assert!(requests[1].method == HttpMethod::Post);
    assert!(requests[5].method == HttpMethod::Delete);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"name":"Account pilot"})
    );
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Account ID: acct-123"));
    assert!(output.contains("Saved Computer identity; compute is allocated when you start it."));
    assert!(output.contains("Computer start queued."));
    assert!(output.contains("Reason: active_limit"));
    assert!(output.contains(&format!("Deleted Computer {ID}.")));
    assert!(!output.contains("access-secret"));
}

#[test]
fn account_computers_refuse_unsafe_ids_machine_keys_and_unconfirmed_delete() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    client
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    assert!(client.computer("../other").is_err());
    assert!(client.computer_action("id/other", "start").is_err());
    assert!(client.delete_computer("id?other").is_err());
    assert!(client.create_computer("un\nsafe", false, false).is_err());
    assert!(client.create_computer("Trial", true, false).is_err());
    assert!(client.create_computer("Trial", false, true).is_err());
    assert!(client.computer_usage("../other").is_err());
    let error = run_computers(
        CloudComputersCommand::List { json: false },
        &client,
        &machine::MachineKeyEnv::from_raw(Some("machine-key-present")),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interactive Codewhale account login")
    );
    assert!(transport.requests().is_empty());

    let denied = FakeTransport::new(vec![response(
        200,
        json!({"deleted": false, "computerId": "123e4567-e89b-42d3-a456-426614174000"}),
    )]);
    let client = CloudClient::new(&denied, &secrets, "default", DEFAULT_API_BASE);
    assert!(
        client
            .delete_computer("123e4567-e89b-42d3-a456-426614174000")
            .is_err()
    );
}

#[test]
fn account_computers_json_preserves_server_metering_and_entitlement() {
    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    let (_temp, mut config) = test_config();
    let (secrets, _) = test_secrets();
    let account = AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE);
    account
        .save(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut record = computer(ID, "running");
    record["allowanceMultiplier"] = json!(1);
    record["allowance"] = json!({
        "meter": "compute_cu", "usedCu": 2.5, "includedCu": 20,
        "remainingCu": 17.5, "state": "ok"
    });
    record["futureMeterField"] = json!({"value": 7});
    let listing = json!({
        "computers": [record.clone()],
        "entitlement": {"planId": "test", "seatActive": true}
    });
    let shown = json!({"computer": record});
    let transport = FakeTransport::new(vec![
        response(200, listing.clone()),
        response(200, shown.clone()),
    ]);
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    for (argv, expected) in [
        (
            vec!["codewhale", "account", "computers", "list", "--json"],
            listing,
        ),
        (
            vec!["codewhale", "account", "computers", "show", ID, "--json"],
            shown,
        ),
    ] {
        let mut output = Vec::new();
        run_with(
            command(&argv),
            "default",
            DEFAULT_API_BASE,
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
            expected
        );
    }
}

#[test]
fn account_computers_boat_trial_and_usage_send_explicit_consent_and_read_receipts() {
    const ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    let (_temp, mut config) = test_config();
    let (secrets, _) = test_secrets();
    AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE)
        .save(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let usage = json!({
        "computerId": ID,
        "provider": "boat",
        "meter": "provider_billable_seconds",
        "trialLimitSeconds": 7200,
        "usedSeconds": 360,
        "remainingSeconds": 6840,
        "customerChargeDollars": 0,
        "receipts": [{"operationId": "operation-1", "seconds": 360}]
    });
    let transport = FakeTransport::new(vec![
        response(201, json!({"computer": computer(ID, "suspended")})),
        response(200, usage.clone()),
    ]);
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    for (index, argv) in [
        vec![
            "codewhale",
            "account",
            "computers",
            "create",
            "Trial",
            "--computer-trial",
            "--eu-compute-opt-in",
        ],
        vec!["codewhale", "account", "computers", "usage", ID, "--json"],
    ]
    .into_iter()
    .enumerate()
    {
        run_with(
            command(&argv),
            "default",
            DEFAULT_API_BASE,
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();
        if index == 0 {
            assert!(String::from_utf8_lossy(&output).contains("Saved Computer identity"));
            output.clear();
        }
    }
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[0].path, "/api/computers");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        json!({"name": "Trial", "provider": "boat", "boatEuComputeOptIn": true})
    );
    assert_eq!(requests[1].method, HttpMethod::Get);
    assert_eq!(requests[1].path, format!("/api/computers/{ID}/usage"));
    assert!(requests[1].body.is_none());
    assert!(
        requests
            .iter()
            .all(|request| request.bearer.as_deref() == Some("access-secret"))
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
        usage
    );
}

#[test]
fn account_agents_create_model_bound_thread_and_send_with_same_session() {
    let (_temp, mut config) = test_config();
    let (secrets, _) = test_secrets();
    AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE)
        .save(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let a = agent("agent-1", "Whale");
    let t = agent_thread("thread-1", "agent-1", "Main");
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [a.clone()] })),
        response(201, json!({ "agent": a.clone() })),
        response(200, json!({ "agents": [a.clone()] })),
        response(200, route_catalog()),
        response(201, json!({ "thread": t.clone(), "id": "thread-1" })),
        response(200, json!({ "agents": [a.clone()] })),
        response(200, json!([t.clone()])),
        response(200, json!({ "agents": [a] })),
        response(200, json!({ "thread": t })),
        response(
            202,
            json!({ "turn": { "id": "turn-1", "status": "pending" } }),
        ),
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!({ "thread": agent_thread("thread-1", "agent-1", "Main") }),
        ),
        sse_response(&[
            json!({ "type": "turn.started", "seq": 1, "turnId": "turn-1", "payload": { "status": "running" } }),
            json!({ "type": "assistant.delta", "seq": 2, "turnId": "turn-1", "payload": { "text": "Done.\n" } }),
            json!({ "type": "turn.completed", "seq": 3, "turnId": "turn-1", "payload": { "status": "completed" } }),
        ]),
    ]);
    let mut output = Vec::new();
    let mut key_reader = |_| bail!("unused");
    let mut opener = |_| true;
    let mut sleeper = |_| {};
    for argv in [
        vec!["codewhale", "account", "agents", "list"],
        vec![
            "codewhale",
            "account",
            "agents",
            "create",
            "Whale",
            "--project-id",
            "project-codewhale",
            "--operation-key",
            "create-1",
        ],
        vec![
            "codewhale",
            "account",
            "agents",
            "new-thread",
            "Whale",
            "--operation-key",
            "thread-1",
        ],
        vec!["codewhale", "account", "agents", "threads", "Whale"],
        vec![
            "codewhale",
            "account",
            "agents",
            "send",
            "Whale",
            "Build this",
            "--thread",
            "thread-1",
            "--billing-mode",
            "byok_external",
            "--operation-key",
            "message-1",
        ],
        vec![
            "codewhale",
            "account",
            "agents",
            "result",
            "Whale",
            "thread-1",
            "turn-1",
        ],
    ] {
        run_with(
            command(&argv),
            "default",
            DEFAULT_API_BASE,
            &mut config,
            &secrets,
            &secrets,
            &machine::MachineKeyEnv::default(),
            &transport,
            &mut output,
            &mut key_reader,
            &mut opener,
            &mut sleeper,
        )
        .unwrap();
    }
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/api/agents",
            "/api/agents",
            "/api/agents",
            "/api/model-providers",
            "/v1/threads",
            "/api/agents",
            "/v1/threads/summary?agentId=agent-1&limit=100",
            "/api/agents",
            "/v1/threads/thread-1",
            "/v1/threads/thread-1/turns",
            "/api/agents",
            "/v1/threads/thread-1",
            "/v1/threads/thread-1/events?since_seq=0",
        ]
    );
    // The model catalog is public: no credential rides along with that read.
    assert!(requests.iter().all(|request| {
        request.bearer.as_deref()
            == (request.path != "/api/model-providers").then_some("access-secret")
    }));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({ "name": "Whale", "projectId": "project-codewhale", "operationKey": "create-1" })
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[4].body.as_ref().unwrap()).unwrap(),
        json!({
            "title": "Main", "productMode": "chat", "mode": "chat",
            "agentId": "agent-1", "projectId": "project-codewhale",
            "modelProvider": "deepseek",
            "model": "deepseek-flash", "operationKey": "thread-1"
        })
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[9].body.as_ref().unwrap()).unwrap(),
        json!({
            "prompt": "Build this", "billingMode": "byok_external",
            "modelProvider": "deepseek", "modelProviderId": "",
            "model": "deepseek-flash", "requiresByok": true,
            "mode": "chat", "productMode": "chat", "operationKey": "message-1",
            "sourceMessageId": "message-1"
        })
    );
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Turn ID: turn-1"));
    assert!(output.contains("Status: pending"));
    assert!(output.contains("Status: completed"));
    assert!(output.contains("Answer:\nDone."));
    assert!(!output.contains("access-secret"));
}

#[test]
fn account_projects_list_and_agent_binding_use_the_saved_revision() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut unbound = agent("agent-1", "Whale");
    unbound["projectId"] = json!("");
    let bound = agent("agent-1", "Whale");
    let projects = json!({ "projects": [{ "id": "project-codewhale", "name": "Codewhale" }] });
    let transport = FakeTransport::new(vec![
        response(200, projects.clone()),
        response(200, json!({ "agents": [unbound] })),
        response(200, projects),
        response(200, json!({ "agent": bound })),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    run_projects(
        CloudProjectsCommand::List { json: false },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    run_agents(
        CloudAgentsCommand::BindProject {
            agent: "Whale".into(),
            project_id: "project-codewhale".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/api/projects",
            "/api/agents",
            "/api/projects",
            "/api/agents/agent-1"
        ]
    );
    assert!(requests[3].method == HttpMethod::Patch);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[3].body.as_ref().unwrap()).unwrap(),
        json!({ "projectId": "project-codewhale", "revision": 1 })
    );
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Codewhale — project-codewhale"));
    assert!(output.contains("Agent Whale is bound to Project project-codewhale."));
}

#[test]
fn account_github_binding_creates_a_project_from_the_same_repository() {
    let (secrets, _) = test_secrets();
    CloudClient::new(
        &FakeTransport::new(vec![]),
        &secrets,
        "default",
        DEFAULT_API_BASE,
    )
    .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
    .unwrap();
    let binding_id = "github:acct-123:987:owner/repo";
    let binding = json!({
        "id": binding_id,
        "provider": "github",
        "repo": "owner/repo",
        "status": "bound",
        "installationId": "987",
    });
    let transport = FakeTransport::new(vec![
        response(200, json!({ "bindings": [binding.clone()] })),
        response(200, json!({ "bindings": [binding] })),
        response(
            201,
            json!({ "project": {
            "id": "project-github", "name": "My Project",
            "defaultRepoProvider": "github", "defaultRepo": "owner/repo",
        } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    run_github(
        CloudGithubCommand::Bindings { json: false },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    run_projects(
        CloudProjectsCommand::Create {
            name: "My Project".into(),
            repo_binding_id: binding_id.into(),
            operation_key: "project-create-1".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].path, "/api/integrations/github/bindings");
    assert_eq!(requests[1].path, "/api/integrations/github/bindings");
    assert_eq!(requests[2].path, "/api/projects");
    assert_eq!(requests[2].method, HttpMethod::Post);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[2].body.as_ref().unwrap()).unwrap(),
        json!({
            "name": "My Project", "defaultMode": "chat", "chatFilesystem": "optional_scratch",
            "repoBindingId": binding_id, "operationKey": "project-create-1",
        })
    );
    assert!(
        requests
            .iter()
            .all(|request| request.bearer.as_deref() == Some("access-secret"))
    );
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("owner/repo — github:acct-123:987:owner/repo (bound)"));
    assert!(output.contains("GitHub repository: owner/repo"));
    assert!(output.contains("agents bind-project AGENT project-github"));
    assert!(!output.contains("access-secret"));
}

#[test]
fn account_github_project_setup_refuses_missing_or_inactive_bindings_before_write() {
    let (secrets, _) = test_secrets();
    CloudClient::new(
        &FakeTransport::new(vec![]),
        &secrets,
        "default",
        DEFAULT_API_BASE,
    )
    .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
    .unwrap();
    let command = |repo_binding_id: &str| CloudProjectsCommand::Create {
        name: "My Project".into(),
        repo_binding_id: repo_binding_id.into(),
        operation_key: "project-create-1".into(),
    };
    let missing = FakeTransport::new(vec![response(200, json!({ "bindings": [] }))]);
    let error = run_projects(
        command("github:missing:repo"),
        &CloudClient::new(&missing, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not connected to this account"));
    assert_eq!(missing.requests().len(), 1);

    let empty = FakeTransport::new(vec![response(200, json!({ "bindings": [] }))]);
    let mut output = Vec::new();
    run_github(
        CloudGithubCommand::Bindings { json: false },
        &CloudClient::new(&empty, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("No GitHub repositories are connected")
    );

    let revoked = FakeTransport::new(vec![response(
        200,
        json!({ "bindings": [{
        "id": "github:revoked:repo", "provider": "github", "repo": "owner/repo",
        "status": "revoked", "installationId": "987",
    }] }),
    )]);
    let error = run_projects(
        command("github:revoked:repo"),
        &CloudClient::new(&revoked, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("unavailable for a Project"));
    assert_eq!(revoked.requests().len(), 1);
}

#[test]
fn account_github_setup_reports_unattached_api_and_rejects_wrong_project_receipt() {
    let (secrets, _) = test_secrets();
    CloudClient::new(
        &FakeTransport::new(vec![]),
        &secrets,
        "default",
        DEFAULT_API_BASE,
    )
    .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
    .unwrap();
    let unavailable = FakeTransport::new(vec![response(
        503,
        json!({ "code": "control_plane_not_attached" }),
    )]);
    let error = run_github(
        CloudGithubCommand::Bindings { json: false },
        &CloudClient::new(&unavailable, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("GitHub repository bindings are unavailable")
    );

    let machine_only = FakeTransport::new(vec![]);
    let error = run_github(
        CloudGithubCommand::Bindings { json: false },
        &CloudClient::new(&machine_only, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::from_raw(Some("machine-key-present")),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interactive Codewhale account login")
    );
    assert!(machine_only.requests().is_empty());

    let binding_id = "github:acct-123:987:owner/repo";
    let mismatch = FakeTransport::new(vec![
        response(
            200,
            json!({ "bindings": [{
            "id": binding_id, "provider": "github", "repo": "owner/repo",
            "status": "bound", "installationId": "987",
        }] }),
        ),
        response(
            201,
            json!({ "project": {
            "id": "project-other", "name": "My Project",
            "defaultRepoProvider": "github", "defaultRepo": "someone/else",
        } }),
        ),
    ]);
    let error = run_projects(
        CloudProjectsCommand::Create {
            name: "My Project".into(),
            repo_binding_id: binding_id.into(),
            operation_key: "project-create-1".into(),
        },
        &CloudClient::new(&mismatch, &secrets, "default", DEFAULT_API_BASE),
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("different GitHub repository"));
    assert_eq!(mismatch.requests().len(), 2);
}

#[test]
fn account_agent_binding_refuses_an_unavailable_project_before_writing() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(200, json!({ "projects": [] })),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::BindProject {
            agent: "Whale".into(),
            project_id: "project-unknown".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not available on this account"));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn account_agent_send_refuses_closed_billing_modes_before_network() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let thread: AgentThread =
        serde_json::from_value(agent_thread("thread-1", "agent-1", "Main")).unwrap();
    for mode in ["membership_included", "managed_wallet"] {
        let error = client
            .send_agent_turn(&thread, "Build this", mode, "message-1")
            .err()
            .expect("billing mode should fail");
        assert!(
            error
                .to_string()
                .contains("unavailable under the current launch policy")
        );
    }
    assert!(transport.requests().is_empty());
}

#[test]
fn account_agent_send_uses_its_only_active_conversation_regardless_of_title() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!([agent_thread("thread-1", "agent-1", "Whale Trial · main")]),
        ),
        response(
            202,
            json!({ "turn": { "id": "turn-1", "status": "pending" } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Continue".into(),
            thread: None,
            billing_mode: "byok_external".into(),
            operation_key: "message-1".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].path, "/v1/threads/thread-1/turns");
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("Turn ID: turn-1")
    );
}

#[test]
fn account_agent_work_records_one_idempotent_request_without_allocating_compute() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!({
                "intent": "actionable",
                "work": { "id": "run-1", "agentId": "agent-1", "status": "queued", "objective": "Fix the build" },
                "queuedWork": []
            }),
        ),
        response(
            200,
            json!({ "run": { "id": "run-1", "state": "queued", "title": "Fix the build" } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    run_agents(
        CloudAgentsCommand::Work {
            agent: "Whale".into(),
            objective: "Fix the build".into(),
            message_id: "work-request-1".into(),
            context_file: None,
            file_refs: Vec::new(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    run_agents(
        CloudAgentsCommand::WorkStatus { id: "run-1".into() },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/api/agents",
            "/api/agents/agent-1/messages",
            "/api/runs/run-1"
        ]
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({ "messageId": "work-request-1", "text": "Fix the build" })
    );
    let shown = String::from_utf8(output).unwrap();
    assert!(shown.contains("Work ID: run-1"));
    assert!(shown.contains("Work is recorded, not started."));
    assert!(shown.contains("work-quote run-1 --operation-key"));
}

#[test]
fn account_agent_send_allows_an_unbound_agent_conversation() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut unbound_agent = agent("agent-1", "Whale");
    unbound_agent["projectId"] = json!("");
    let mut default_project_thread = agent_thread("thread-1", "agent-1", "Whale Trial · main");
    default_project_thread["projectId"] = json!("project-general");
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [unbound_agent] })),
        response(200, json!([default_project_thread])),
        response(
            202,
            json!({ "turn": { "id": "turn-1", "status": "pending" } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Continue".into(),
            thread: None,
            billing_mode: "byok_external".into(),
            operation_key: "message-1".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap();
    assert_eq!(transport.requests()[2].path, "/v1/threads/thread-1/turns");
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("Turn ID: turn-1")
    );
}

#[test]
fn account_agent_send_refuses_a_conversation_from_a_previous_project() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut rebound = agent("agent-1", "Whale");
    rebound["projectId"] = json!("project-new");
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [rebound] })),
        response(
            200,
            json!({ "thread": agent_thread("thread-1", "agent-1", "Main") }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Continue".into(),
            thread: Some("thread-1".into()),
            billing_mode: "byok_external".into(),
            operation_key: "message-1".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("different Project"));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn account_agents_new_thread_omits_an_unbound_project_and_preserves_a_bound_one() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    let client_session = CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE);
    client_session
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let new_thread = || CloudAgentsCommand::NewThread {
        agent: "Whale".into(),
        title: "Main".into(),
        provider: "deepseek".into(),
        model: "deepseek-flash".into(),
        operation_key: "thread-project-1".into(),
    };

    let mut unbound_agent = agent("agent-1", "Whale");
    unbound_agent["projectId"] = json!("");
    let mut default_project_thread = agent_thread("thread-1", "agent-1", "Main");
    default_project_thread["projectId"] = json!("project-general");
    let unbound = FakeTransport::new(vec![
        response(200, json!({ "agents": [unbound_agent] })),
        response(200, route_catalog()),
        response(201, json!({ "thread": default_project_thread })),
    ]);
    let unbound_client = CloudClient::new(&unbound, &secrets, "default", DEFAULT_API_BASE);
    let mut unbound_output = Vec::new();
    run_agents(
        new_thread(),
        &unbound_client,
        &machine::MachineKeyEnv::default(),
        &mut unbound_output,
    )
    .unwrap();
    assert!(
        String::from_utf8(unbound_output)
            .unwrap()
            .contains("ID: thread-1")
    );
    assert_eq!(unbound.requests().len(), 3);
    let unbound_body: serde_json::Value =
        serde_json::from_slice(unbound.requests()[2].body.as_ref().unwrap()).unwrap();
    assert!(unbound_body.get("projectId").is_none());

    let mut foreign_thread = agent_thread("thread-1", "agent-1", "Main");
    foreign_thread["projectId"] = json!("project-other");
    let foreign = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(200, route_catalog()),
        response(201, json!({ "thread": foreign_thread })),
    ]);
    let foreign_client = CloudClient::new(&foreign, &secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    let error = run_agents(
        new_thread(),
        &foreign_client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap_err();
    assert!(error.to_string().contains("different Project"));
    assert!(output.is_empty());
    let requests = foreign.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[2].body.as_ref().unwrap()).unwrap()["projectId"],
        "project-codewhale"
    );
}

#[test]
fn account_agents_require_explicit_thread_when_recent_page_may_be_incomplete() {
    let (secrets, _) = test_secrets();
    let auth_transport = FakeTransport::new(vec![]);
    CloudClient::new(&auth_transport, &secrets, "default", DEFAULT_API_BASE)
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let page = (0..100)
        .map(|index| agent_thread(&format!("thread-{index}"), "agent-1", "Main"))
        .collect::<Vec<_>>();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(200, json!(page)),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Do work".into(),
            thread: None,
            billing_mode: "byok_external".into(),
            operation_key: "message-100".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("pass --thread"));
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(
        transport.requests()[1].path,
        "/v1/threads/summary?agentId=agent-1&limit=100"
    );
}

#[test]
fn account_agents_result_replays_only_the_selected_turn_after_cursor() {
    assert_eq!(validate_turn_id("turn:reply@1").unwrap(), "turn:reply@1");
    assert!(validate_turn_id("turn/other").is_err());
    let events = sse_response(&[
        json!({ "type": "assistant.delta", "seq": 11, "turnId": "other-turn", "payload": { "text": "Other answer" } }),
        json!({ "type": "assistant.delta", "seq": 12, "turnId": "turn-1", "payload": { "text": "Selected " } }),
        json!({ "type": "assistant.delta", "seq": 13, "turnId": "turn-1", "payload": { "text": "answer" } }),
        json!({ "type": "turn.completed", "seq": 14, "turnId": "turn-1", "payload": { "status": "completed" } }),
    ]);
    let result = parse_agent_turn_events(&events.body, "turn-1", 10).unwrap();
    assert_eq!(result.answer, "Selected answer");
    assert_eq!(result.status, "completed");
    assert_eq!(result.last_seq, 14);
    assert!(result.seen_turn);
    assert!(parse_agent_turn_events(&events.body, "turn-1", 12).is_err());
}

#[test]
fn account_agents_refuse_foreign_threads_ambiguous_main_and_machine_keys() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::List { json: false },
        &client,
        &machine::MachineKeyEnv::from_raw(Some("machine-key-present")),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interactive Codewhale account login")
    );
    assert!(transport.requests().is_empty());
    assert!(client.create_agent("Whale", None, "invalid/key").is_err());
    assert!(transport.requests().is_empty());

    client
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let foreign = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!({ "thread": agent_thread("thread-2", "agent-2", "Main") }),
        ),
    ]);
    let foreign_client = CloudClient::new(&foreign, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Do work".into(),
            thread: Some("thread-2".into()),
            billing_mode: "byok_external".into(),
            operation_key: "message-2".into(),
        },
        &foreign_client,
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not belong"));
    assert_eq!(foreign.requests().len(), 2);

    let ambiguous = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!([
                agent_thread("thread-1", "agent-1", "Main"),
                agent_thread("thread-2", "agent-1", "Main")
            ]),
        ),
    ]);
    let ambiguous_client = CloudClient::new(&ambiguous, &secrets, "default", DEFAULT_API_BASE);
    let error = run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Do work".into(),
            thread: None,
            billing_mode: "byok_external".into(),
            operation_key: "message-3".into(),
        },
        &ambiguous_client,
        &machine::MachineKeyEnv::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("several Main conversations"));
    assert_eq!(ambiguous.requests().len(), 2);
}

#[test]
fn account_agents_do_not_claim_a_turn_when_runtime_is_unattached() {
    let (secrets, _) = test_secrets();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!({ "thread": agent_thread("thread-1", "agent-1", "Main") }),
        ),
        response(
            409,
            json!({ "error": { "code": "chat_runtime_not_attached" } }),
        ),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    client
        .save_auth(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    let mut output = Vec::new();
    let error = run_agents(
        CloudAgentsCommand::Send {
            agent: "Whale".into(),
            prompt: "Do work".into(),
            thread: Some("thread-1".into()),
            billing_mode: "byok_external".into(),
            operation_key: "message-4".into(),
        },
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    )
    .unwrap_err();
    assert!(error.to_string().contains("chat_runtime_not_attached"));
    assert!(output.is_empty());
    assert_eq!(transport.requests().len(), 3);
}

// ---- Account Work journey (work.rs) -------------------------------------

const WORK_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

enum Scripted {
    Reply(CloudResponse),
    Unreachable,
}

/// A transport that can also lose the connection, which `FakeTransport`
/// cannot: a lost reply is the case mutating commands must report honestly.
struct ScriptedTransport {
    steps: Mutex<VecDeque<Scripted>>,
    requests: Mutex<Vec<CloudRequest>>,
}

impl ScriptedTransport {
    fn new(steps: Vec<Scripted>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, Vec<CloudRequest>> {
        self.requests.lock().unwrap()
    }
}

impl CloudTransport for ScriptedTransport {
    fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
        self.requests.lock().unwrap().push(request);
        match self.steps.lock().unwrap().pop_front() {
            Some(Scripted::Reply(reply)) => Ok(reply),
            Some(Scripted::Unreachable) => Err(CloudTransportError::new(
                "could not reach the Codewhale service",
                std::io::Error::other("connection reset"),
            )
            .into()),
            None => Err(anyhow!("scripted transport exhausted")),
        }
    }
}

fn reply(status: u16, body: serde_json::Value) -> Scripted {
    Scripted::Reply(response(status, body))
}

fn signed_in() -> Secrets {
    let (secrets, _) = test_secrets();
    AccountSessionStore::new(secrets.clone(), Some("default"), DEFAULT_API_BASE)
        .save(auth("access-secret", "refresh-secret", "acct-123"))
        .unwrap();
    secrets
}

/// Run one agents subcommand and return its result with everything it printed.
fn run_agent_command(
    transport: &impl CloudTransport,
    secrets: &Secrets,
    argv: &[&str],
) -> (Result<()>, String) {
    let CloudCommand::Agents(agents) = command(argv) else {
        panic!("expected an agents command");
    };
    let client = CloudClient::new(transport, secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    let result = run_agents(
        agents.command,
        &client,
        &machine::MachineKeyEnv::default(),
        &mut output,
    );
    (result, String::from_utf8(output).unwrap())
}

fn run_github_command(
    transport: &impl CloudTransport,
    secrets: &Secrets,
    machine: &machine::MachineKeyEnv,
    argv: &[&str],
) -> (Result<()>, String) {
    let CloudCommand::Github(github) = command(argv) else {
        panic!("expected a github command");
    };
    let client = CloudClient::new(transport, secrets, "default", DEFAULT_API_BASE);
    let mut output = Vec::new();
    let result = run_github(github.command, &client, machine, &mut output);
    (result, String::from_utf8(output).unwrap())
}

fn chain(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

fn body_of(request: &CloudRequest) -> serde_json::Value {
    serde_json::from_slice(request.body.as_ref().expect("request body")).unwrap()
}

fn work_message(intent: &str, extra: serde_json::Value, work: serde_json::Value) -> CloudResponse {
    let mut body = json!({ "intent": intent, "work": work });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    response(200, body)
}

fn work_item(status: &str) -> serde_json::Value {
    json!({ "id": "run-1", "agentId": "agent-1", "status": status, "objective": "Fix the build" })
}

fn agents_step() -> Scripted {
    reply(200, json!({ "agents": [agent("agent-1", "Whale")] }))
}

fn work_command(message_id: &str) -> Vec<String> {
    [
        "codewhale",
        "account",
        "agents",
        "work",
        "Whale",
        "Fix the build",
        "--message-id",
        message_id,
    ]
    .map(String::from)
    .to_vec()
}

fn run_work(transport: &impl CloudTransport, secrets: &Secrets) -> (Result<()>, String) {
    let argv = work_command("work-1");
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    run_agent_command(transport, secrets, &argv)
}

fn work_run(state: &str) -> serde_json::Value {
    json!({ "run": {
        "id": WORK_ID, "agentId": "agent-1", "projectId": "project-codewhale",
        "title": "Fix the build", "state": state, "repo": "octo-org/app", "repoProvider": "github"
    } })
}

fn projects_step() -> Scripted {
    reply(
        200,
        json!({ "projects": [{
            "id": "project-codewhale", "name": "Codewhale",
            "defaultRepoProvider": "github", "defaultRepo": "octo-org/app"
        }] }),
    )
}

fn work_offer() -> serde_json::Value {
    json!({
        "mode": "trial", "sku": "task-small", "region": "eu", "minSeconds": 60,
        "maxSeconds": 300, "model": { "provider": "deepseek", "model": "deepseek-flash" },
        "funding": "trial_credit", "computeRemainingSeconds": null, "emptyWorkspace": false
    })
}

fn work_quote() -> serde_json::Value {
    json!({
        "runner": {},
        "quote": { "sku": "task-small", "target": "eu", "pricingStatus": "provider_trial" },
        "disclosure": {
            "funding": "trial_credit", "customerCreditsChargedUsd": 0,
            "providerCostEstimateUsd": 0.0207, "sandboxTargetRegion": "eu",
            "euPlacementConsentRequired": true,
            "modelInference": { "billing": "byok_external" },
            "computerTime": { "estimatedSeconds": 300 }
        },
        "confirmation": {
            "token": "tok.abc-123_DEF", "expiresAt": "2026-09-30T12:00:00.000Z", "workRunId": WORK_ID
        },
        "confirmCopy": {
            "title": "Run this five-minute trial Work?",
            "body": "Trial credit covers its computer time.\u{1b}[31m",
            "confirmLabel": "Start trial Work"
        }
    })
}

/// POST /api/cloud-sessions replies `{ "session": {...} }`.
fn cloud_session() -> serde_json::Value {
    json!({ "session": {
        "id": format!("session_{WORK_ID}"),
        "run": { "id": WORK_ID, "state": "planning" },
        "attempt": { "status": "accepted" },
        "sandbox": { "status": "running" },
        "sandboxTargetRegion": "eu",
        "quote": { "customerCreditsChargedUsd": 0 },
        "initialTurn": { "status": "queued" }
    } })
}

const QUOTE_ARGV: [&str; 7] = [
    "codewhale",
    "account",
    "agents",
    "work-quote",
    WORK_ID,
    "--operation-key",
    "launch-1",
];
const LAUNCH_ARGV: [&str; 12] = [
    "codewhale",
    "account",
    "agents",
    "work-launch",
    WORK_ID,
    "--operation-key",
    "launch-1",
    "--confirmation",
    "tok.abc-123_DEF",
    "--seconds",
    "300",
    "--confirm-eu-compute",
];

#[test]
fn work_journey_commands_parse_and_are_documented_in_help() {
    assert!(matches!(
        command(&["codewhale", "account", "agents", "work-cancel", "run-1"]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkCancel {
                queue: None,
                reason: None,
                ..
            }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "agents",
            "work-cancel",
            "run-1",
            "--queue",
            "park",
            "--reason",
            "wrong repo"
        ]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkCancel {
                queue: Some(WorkQueueChoice::Park),
                reason: Some(_),
                ..
            }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "agents",
            "work-cancel",
            "run-1",
            "--queue",
            "discard"
        ]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkCancel {
                queue: Some(WorkQueueChoice::Discard),
                ..
            }
        })
    ));
    assert!(
        Cli::try_parse_from([
            "codewhale",
            "account",
            "agents",
            "work-cancel",
            "run-1",
            "--queue",
            "keep"
        ])
        .is_err()
    );
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "agents",
            "work-result",
            "run-1",
            "--json"
        ]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkResult { json: true, .. }
        })
    ));
    assert!(matches!(
        command(&QUOTE_ARGV),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkQuote { .. }
        })
    ));
    assert!(matches!(
        command(&LAUNCH_ARGV),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkLaunch {
                confirm_eu_compute: true,
                ..
            }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "agents",
            "work-launch",
            WORK_ID,
            "--operation-key",
            "k",
            "--confirmation",
            "t"
        ]),
        CloudCommand::Agents(CloudAgentsArgs {
            command: CloudAgentsCommand::WorkLaunch {
                confirm_eu_compute: false,
                ..
            }
        })
    ));
    for missing_key in [
        vec!["codewhale", "account", "agents", "work-quote", WORK_ID],
        vec![
            "codewhale",
            "account",
            "agents",
            "work-launch",
            WORK_ID,
            "--confirmation",
            "t",
        ],
        vec![
            "codewhale",
            "account",
            "agents",
            "work-launch",
            WORK_ID,
            "--operation-key",
            "k",
        ],
    ] {
        assert!(Cli::try_parse_from(missing_key).is_err());
    }
    assert!(matches!(
        command(&["codewhale", "account", "github", "bind", "octo-org/app"]),
        CloudCommand::Github(CloudGithubArgs {
            command: CloudGithubCommand::Bind {
                installation_id: None,
                ..
            }
        })
    ));
    assert!(matches!(
        command(&[
            "codewhale",
            "account",
            "github",
            "bind",
            "octo-org/app",
            "--installation-id",
            "987"
        ]),
        CloudCommand::Github(CloudGithubArgs {
            command: CloudGithubCommand::Bind {
                installation_id: Some(_),
                ..
            }
        })
    ));

    let help = |argv: &[&str]| Cli::try_parse_from(argv).unwrap_err().to_string();
    let agents_help = help(&["codewhale", "account", "agents", "--help"]);
    for name in [
        "work-cancel",
        "work-result",
        "work-quote",
        "work-launch",
        "work-status",
    ] {
        assert!(agents_help.contains(name), "agents --help must list {name}");
    }
    assert!(
        help(&["codewhale", "account", "agents", "work-launch", "--help"])
            .contains("--confirm-eu-compute")
    );
    assert!(help(&["codewhale", "account", "agents", "work-cancel", "--help"]).contains("--queue"));
    assert!(help(&["codewhale", "account", "github", "--help"]).contains("bind"));
}

#[test]
fn work_reports_what_the_agent_actually_did_with_the_message() {
    // A control verb acts on existing Work: it must say which action was
    // applied and never claim new Work was recorded.
    let secrets = signed_in();
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        work_message(
            "control",
            json!({ "reason": "control_verb:stop", "controlAction": "stop", "confident": true }),
            work_item("canceled"),
        ),
    ]);
    let (result, output) = run_work(&transport, &secrets);
    result.unwrap();
    assert!(output.contains("Intent: control"));
    assert!(output.contains("Applied control action: stop"));
    assert!(output.contains("Reason: control_verb:stop"));
    assert!(output.contains("Status: canceled"));
    assert!(!output.contains("Work is recorded"));
    assert!(!output.contains("work-quote"));

    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        work_message(
            "correction",
            json!({ "reason": "correction_marker", "confident": true }),
            work_item("running"),
        ),
    ]);
    let (result, output) = run_work(&transport, &secrets);
    result.unwrap();
    assert!(output.contains("edited the objective of active Work"));
    assert!(output.contains("No new Work was created"));
    assert!(!output.contains("Work is recorded"));

    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        work_message(
            "informational",
            json!({ "reason": "no_action_signal", "confident": false, "suggestion": "the build is red" }),
            json!(null),
        ),
    ]);
    let (result, output) = run_work(&transport, &secrets);
    result.unwrap();
    assert!(output.contains("No Work was created and nothing was changed."));
    assert!(output.contains("Suggestion: the build is red"));
    assert!(output.contains("not confident"));
    assert!(!output.contains("Work is recorded"));

    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        work_message(
            "actionable",
            json!({ "reason": "imperative_verb", "confident": true }),
            work_item("queued"),
        ),
    ]);
    let (result, output) = run_work(&transport, &secrets);
    result.unwrap();
    assert!(output.contains("Work is recorded, not started."));
    assert!(output.contains("work-quote run-1 --operation-key"));
    assert!(output.contains("Message ID: work-1"));

    // A control action with no Work record cannot be confirmed, so it fails.
    let transport = FakeTransport::new(vec![
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        work_message("control", json!({ "controlAction": "stop" }), json!(null)),
    ]);
    let (result, output) = run_work(&transport, &secrets);
    assert!(chain(&result.unwrap_err()).contains("returned no Work record"));
    assert!(output.is_empty());
}

#[test]
fn work_transport_and_server_failures_point_at_the_same_message_id() {
    let secrets = signed_in();
    for lost in [
        vec![agents_step(), Scripted::Unreachable],
        vec![
            agents_step(),
            reply(503, json!({ "code": "runtime_unavailable" })),
        ],
        // A truncated 2xx: the service acted, so the same hint applies.
        vec![
            agents_step(),
            Scripted::Reply(CloudResponse {
                status: 201,
                body: b"{\"intent\":".to_vec(),
                retry_after: None,
            }),
        ],
    ] {
        let transport = ScriptedTransport::new(lost);
        let (result, output) = run_work(&transport, &secrets);
        let message = chain(&result.unwrap_err());
        assert!(
            message.contains("may or may not have reached Codewhale"),
            "{message}"
        );
        assert!(message.contains("--message-id work-1"), "{message}");
        assert!(
            message.contains("never creates a second Work for the same instruction"),
            "{message}"
        );
        assert!(output.is_empty());
        assert!(!message.contains("access-secret") && !message.contains("refresh-secret"));
    }
    // A definitive refusal is not an unknown outcome and gives no replay hint.
    let transport = ScriptedTransport::new(vec![
        agents_step(),
        reply(422, json!({ "code": "conversation_message_id_required" })),
    ]);
    let (result, _) = run_work(&transport, &secrets);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("conversation_message_id_required"));
    assert!(!message.contains("may or may not"));
}

#[test]
fn work_cancel_sends_choices_and_reports_each_outcome_honestly() {
    let secrets = signed_in();
    let argv = [
        "codewhale",
        "account",
        "agents",
        "work-cancel",
        "run-1",
        "--queue",
        "park",
        "--reason",
        "wrong repo",
    ];
    let transport = FakeTransport::new(vec![response(
        200,
        json!({
            "run": { "id": "run-1", "state": "canceled" },
            "command": { "type": "run.control", "action": "cancel" },
            "promptQueue": { "queuedCount": 0 }
        }),
    )]);
    let (result, output) = run_agent_command(&transport, &secrets, &argv);
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[0].path, "/api/runs/run-1/cancel");
    assert_eq!(
        body_of(&requests[0]),
        json!({ "reason": "wrong repo", "queue": "park" })
    );
    assert_eq!(requests[0].bearer.as_deref(), Some("access-secret"));
    assert!(output.contains("Status: canceled"));
    assert!(output.contains("Work canceled."));
    assert!(output.contains("Queued prompts: parked (recorded)."));
    assert!(!output.contains("access-secret"));
    drop(requests);

    // The runtime has been asked to stop but has not confirmed.
    let transport = FakeTransport::new(vec![response(
        200,
        json!({ "command": { "type": "run.control" }, "queued": { "seq": 4 } }),
    )]);
    let (result, output) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-cancel", "run-1"],
    );
    result.unwrap();
    assert!(output.contains("Cancel requested. The runtime has not confirmed the stop yet."));
    assert!(!output.contains("Work canceled."));
    assert_eq!(body_of(&transport.requests()[0]), json!({}));

    // Already final is success, and says where to read the outcome.
    let transport = FakeTransport::new(vec![response(
        409,
        json!({ "code": "run_control_terminal", "message": "Run run-1 is already completed." }),
    )]);
    let (result, output) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-cancel", "run-1"],
    );
    result.unwrap();
    assert!(output.contains("already final"));
    assert!(output.contains("work-result run-1"));

    // Queued prompts need an explicit choice; nothing was cancelled.
    let transport = FakeTransport::new(vec![response(
        422,
        json!({ "code": "run_prompt_queue_choice_required" }),
    )]);
    let (result, _) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-cancel", "run-1"],
    );
    let message = chain(&result.unwrap_err());
    assert!(
        message.contains("--queue discard or --queue park"),
        "{message}"
    );
    assert!(message.contains("was not cancelled"));

    // A different definitive refusal is reported without an unknown-outcome claim.
    let transport = FakeTransport::new(vec![response(
        409,
        json!({ "code": "run_control_transition_invalid" }),
    )]);
    let (result, _) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-cancel", "run-1"],
    );
    let message = chain(&result.unwrap_err());
    assert!(message.contains("run_control_transition_invalid"));
    assert!(!message.contains("unknown"));
}

#[test]
fn work_cancel_with_a_lost_reply_is_an_unknown_outcome() {
    let secrets = signed_in();
    for lost in [
        Scripted::Unreachable,
        reply(502, json!({ "code": "bad_gateway" })),
        Scripted::Reply(CloudResponse {
            status: 200,
            body: b"not json".to_vec(),
            retry_after: None,
        }),
    ] {
        let transport = ScriptedTransport::new(vec![lost]);
        let (result, output) = run_agent_command(
            &transport,
            &secrets,
            &["codewhale", "account", "agents", "work-cancel", "run-1"],
        );
        let message = chain(&result.unwrap_err());
        assert!(message.contains("cancel outcome is unknown"), "{message}");
        assert!(message.contains("work-status run-1"), "{message}");
        assert!(output.is_empty());
    }
    // A bad id or reason never leaves the machine.
    let transport = ScriptedTransport::new(vec![]);
    let (result, _) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-cancel", "../x"],
    );
    result.unwrap_err();
    let (result, _) = run_agent_command(
        &transport,
        &secrets,
        &[
            "codewhale",
            "account",
            "agents",
            "work-cancel",
            "run-1",
            "--reason",
            "bad\u{7}bell",
        ],
    );
    result.unwrap_err();
    assert!(transport.requests().is_empty());
}

fn result_envelope() -> serde_json::Value {
    json!({ "result": {
        "run": { "id": "run-1", "title": "Fix the\u{1b}[31m build", "state": "completed", "workspaceId": "ws-1", "projectId": "p-1" },
        "status": "ready",
        "summary": { "text": "Fixed the failing test.\nSecond line", "source": "run.result" },
        "repository": {
            "name": "octo-org/app", "branch": "codewhale/run-1", "revision": "0123456789abcdef",
            "pullRequest": { "url": "https://github.com/octo-org/app/pull/42", "number": 42, "state": "draft" }
        },
        "changes": { "files": [], "fileCount": 2, "additions": 10, "deletions": 3, "evidence": "recorded" },
        "checks": [
            { "name": "cargo test", "status": "passed", "passed": true },
            { "name": "lint", "status": "failed", "passed": false }
        ],
        "findings": [{ "id": "f1", "severity": "low", "title": "Style nit" }],
        "artifacts": [{ "id": "a1", "name": "patch.diff", "status": "stored", "contentType": "text/x-diff", "size": 512 }],
        "readiness": { "prReady": false, "blockers": ["checks_failed"] },
        "nextAction": { "kind": "open_github", "label": "Open in GitHub", "url": "https://github.com/octo-org/app/pull/42" },
        "modelRoute": { "provider": "deepseek", "model": "deepseek-flash" },
        "boatUsage": {
            "providerSeconds": 212, "providerListPriceDollars": 0.00106,
            "customerCreditsChargedUsd": 0, "funding": "provider_trial", "running": false,
            "cleanupConfirmed": true
        },
        "receipt": { "eventsThroughSeq": 57, "source": "run.result" }
    } })
}

fn attempts_envelope() -> serde_json::Value {
    json!({
        "workId": "run-1", "attemptCount": 2,
        "attempts": [
            { "id": "attempt_a", "sequence": 1, "kind": "launch", "status": "failed", "errorCode": "boat_task_stop_unconfirmed" },
            { "id": "attempt_b", "sequence": 2, "kind": "recovery", "status": "settled", "errorCode": "" }
        ]
    })
}

#[test]
fn work_result_summarises_evidence_pr_route_and_boat_usage() {
    let secrets = signed_in();
    let transport = FakeTransport::new(vec![
        response(200, result_envelope()),
        response(200, attempts_envelope()),
    ]);
    let (result, output) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-result", "run-1"],
    );
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests[0].path, "/api/runs/run-1/result");
    assert_eq!(requests[1].path, "/api/runs/run-1/attempts");
    assert!(
        requests
            .iter()
            .all(|request| request.method == HttpMethod::Get)
    );
    for expected in [
        "State: completed",
        "Result: ready",
        "Objective: Fix the[31m build",
        "Summary: Fixed the failing test.Second line",
        "Changes: 2 file(s), +10 -3 (recorded)",
        "Checks: 1 passed of 2",
        "not passed: lint (failed)",
        "Findings: 1",
        "low: Style nit",
        "patch.diff (stored, text/x-diff, 512 bytes)",
        "Branch: codewhale/run-1",
        "Draft PR: https://github.com/octo-org/app/pull/42",
        "Blockers: checks_failed",
        "Model route: deepseek/deepseek-flash",
        "Computer usage:",
        "Provider seconds: 212",
        "Provider list price: $0.0011",
        "Codewhale credits charged: $0",
        "Funding: provider_trial",
        "Provider VM: stopped",
        "Provider cleanup: confirmed",
        "Attempts: 2",
        "#1 launch failed (error: The cloud computer stopped but its state is unconfirmed)",
        "#2 recovery settled",
        "Evidence through event 57",
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in:\n{output}"
        );
    }
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains("access-secret") && !output.contains("refresh-secret"));
}

#[test]
fn work_result_never_shows_a_link_that_is_not_a_github_pull_request_and_survives_missing_parts() {
    let secrets = signed_in();
    for hostile in [
        "https://github.com/octo-org/app/pull/42/files",
        "https://github.com/other-org/app/pull/42",
        "https://evil.example/octo-org/app/pull/42",
        "javascript:alert(1)",
        "https://github.com/octo-org/app/pull/042",
        "https://github.com/octo-org/app/issues/42",
    ] {
        let mut envelope = result_envelope();
        envelope["result"]["repository"]["pullRequest"]["url"] = json!(hostile);
        let transport = FakeTransport::new(vec![
            response(200, envelope),
            response(503, json!({ "code": "work_attempt_lineage_unavailable" })),
        ]);
        let (result, output) = run_agent_command(
            &transport,
            &secrets,
            &["codewhale", "account", "agents", "work-result", "run-1"],
        );
        result.unwrap();
        assert!(!output.contains("Draft PR:"), "{hostile}");
        assert!(!output.contains(hostile), "{hostile}");
        assert!(output.contains("not a GitHub pull request"), "{hostile}");
        // Attempt lineage failing does not hide the result itself.
        assert!(output.contains("Attempts: unavailable"));
    }
    // A running Work with nothing recorded says it is not final and invents nothing.
    let transport = FakeTransport::new(vec![
        response(
            200,
            json!({ "result": {
                "run": { "id": "run-1", "state": "running" }, "status": "in_progress",
                "changes": { "evidence": "unavailable" }, "checks": [], "artifacts": []
            } }),
        ),
        response(200, json!({ "attempts": [] })),
    ]);
    let (result, output) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-result", "run-1"],
    );
    result.unwrap();
    assert!(output.contains("still running; nothing below is final"));
    assert!(output.contains("Changes: not verified (evidence: unavailable)"));
    assert!(output.contains("Checks: none recorded"));
    assert!(output.contains("Artifacts: none"));
    assert!(output.contains("Draft PR: none"));
    assert!(output.contains("Model route: not reported"));
    assert!(!output.contains("Computer usage:"));
    // Another run's record is refused.
    let transport = FakeTransport::new(vec![response(
        200,
        json!({ "result": { "run": { "id": "run-2", "state": "completed" } } }),
    )]);
    let (result, _) = run_agent_command(
        &transport,
        &secrets,
        &["codewhale", "account", "agents", "work-result", "run-1"],
    );
    assert!(chain(&result.unwrap_err()).contains("different Work result"));
}

#[test]
fn work_result_json_prints_the_raw_result_and_attempts() {
    let secrets = signed_in();
    let transport = FakeTransport::new(vec![
        response(200, result_envelope()),
        response(200, attempts_envelope()),
    ]);
    let (result, output) = run_agent_command(
        &transport,
        &secrets,
        &[
            "codewhale",
            "account",
            "agents",
            "work-result",
            "run-1",
            "--json",
        ],
    );
    result.unwrap();
    let printed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(printed["result"], result_envelope());
    assert_eq!(printed["attempts"], attempts_envelope());
    assert!(!output.contains("access-secret"));
}

#[test]
fn work_quote_builds_the_c5_request_from_served_records_and_prints_the_disclosure() {
    let secrets = signed_in();
    let transport = FakeTransport::new(vec![
        response(200, work_run("queued")),
        response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
        response(
            200,
            json!({ "projects": [{
            "id": "project-codewhale", "name": "Codewhale",
            "defaultRepoProvider": "github", "defaultRepo": "octo-org/app"
        }] }),
        ),
        response(200, work_offer()),
        response(200, work_quote()),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            format!("/api/runs/{WORK_ID}").as_str(),
            "/api/agents",
            "/api/projects",
            "/api/sandbox/work-offer",
            "/api/sandbox/launch-quote"
        ]
    );
    assert_eq!(requests[4].method, HttpMethod::Post);
    assert_eq!(
        body_of(&requests[4]),
        json!({
            "workRunId": WORK_ID, "agentId": "agent-1", "projectId": "project-codewhale",
            "repo": "octo-org/app", "provider": "github", "prompt": "Fix the build",
            "runnerKind": "hosted", "sandboxSku": "task-small", "estimatedSeconds": 300,
            "modelProvider": "deepseek", "model": "deepseek-flash",
            "billingMode": "byok_external", "computeRegion": "eu",
            "sandboxTargetRegion": "eu", "crossRegionSandboxOptIn": true,
            "operationKey": "launch-1"
        })
    );
    for expected in [
        "Nothing has started and nothing is charged.",
        "Repository: octo-org/app",
        "deepseek/deepseek-flash",
        "small cloud computer in the EU, up to 300 seconds",
        "Funding: trial credit; Codewhale credits charged: $0",
        "Provider cost estimate: $0.0207",
        "EU placement",
        "Run this five-minute trial Work?",
        "Trial credit covers its computer time.[31m",
        "Confirmation: tok.abc-123_DEF",
        "Operation key: launch-1",
        &format!(
            "work-launch {WORK_ID} --operation-key launch-1 --confirmation tok.abc-123_DEF --confirm-eu-compute"
        ),
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in:\n{output}"
        );
    }
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains("access-secret") && !output.contains("refresh-secret"));
}

#[test]
fn work_quote_refuses_before_quoting_anything_it_cannot_launch_or_would_charge_for() {
    let secrets = signed_in();
    // Not queued: only the Work is read.
    let transport = FakeTransport::new(vec![response(200, work_run("running"))]);
    let (result, _) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
    assert!(chain(&result.unwrap_err()).contains("only queued Work can be quoted"));
    assert_eq!(transport.requests().len(), 1);

    // The Agent moved to another Project after the Work was created.
    let mut moved = agent("agent-1", "Whale");
    moved["projectId"] = json!("project-other");
    let transport = FakeTransport::new(vec![
        response(200, work_run("queued")),
        response(200, json!({ "agents": [moved] })),
    ]);
    let (result, _) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
    assert!(chain(&result.unwrap_err()).contains("no longer bound to this Work's Project"));

    // A non-GitHub Work cannot use cloud Work.
    let mut cnb = work_run("queued");
    cnb["run"]["repoProvider"] = json!("cnb");
    let transport = FakeTransport::new(vec![response(200, cnb)]);
    let (result, _) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
    assert!(chain(&result.unwrap_err()).contains("GitHub repositories only"));

    // A quote that would charge Codewhale credits (or is not the EU trial)
    // yields no confirmation and no launch command.
    for tamper in [
        |quote: &mut serde_json::Value| {
            quote["disclosure"]["customerCreditsChargedUsd"] = json!(0.4)
        },
        |quote: &mut serde_json::Value| quote["disclosure"]["funding"] = json!("membership"),
        |quote: &mut serde_json::Value| quote["quote"]["sku"] = json!("task-large"),
        |quote: &mut serde_json::Value| {
            quote["disclosure"]["sandboxTargetRegion"] = json!("us-west")
        },
        |quote: &mut serde_json::Value| {
            quote["disclosure"]["computerTime"]["estimatedSeconds"] = json!(301)
        },
        |quote: &mut serde_json::Value| quote["disclosure"]["computerTime"] = json!({}),
        |quote: &mut serde_json::Value| {
            quote["disclosure"]["modelInference"]["billing"] = json!("managed_wallet")
        },
    ] {
        let mut quote = work_quote();
        tamper(&mut quote);
        let transport = FakeTransport::new(vec![
            response(200, work_run("queued")),
            response(200, json!({ "agents": [agent("agent-1", "Whale")] })),
            response(
                200,
                json!({ "projects": [{
                "id": "project-codewhale", "name": "Codewhale",
                "defaultRepoProvider": "github", "defaultRepo": "octo-org/app"
            }] }),
            ),
            response(200, work_offer()),
            response(200, quote),
        ]);
        let (result, output) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
        assert!(chain(&result.unwrap_err()).contains("Refusing to print a confirmation"));
        assert!(!output.contains("tok.abc-123_DEF"));
    }

    // Server refusals keep their code and gain the next step.
    let transport = ScriptedTransport::new(vec![
        reply(200, work_run("queued")),
        agents_step(),
        projects_step(),
        reply(200, work_offer()),
        reply(404, json!({ "code": "work_vm_trial_unavailable" })),
    ]);
    let (result, _) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("work_vm_trial_unavailable"));
    assert!(message.contains("not available for this account"));

    // The printed retry command must not carry remote shell metacharacters.
    for token in ["tok.$(command)", "tok.`command`", "tok.abc;command"] {
        let mut quote = work_quote();
        quote["confirmation"]["token"] = json!(token);
        let transport = ScriptedTransport::new(vec![
            reply(200, work_run("queued")),
            agents_step(),
            projects_step(),
            reply(200, work_offer()),
            reply(200, quote),
        ]);
        let (result, output) = run_agent_command(&transport, &secrets, &QUOTE_ARGV);
        assert!(chain(&result.unwrap_err()).contains("unusable launch confirmation"));
        assert!(!output.contains(token));
    }
}

#[test]
fn work_confirmation_stdin_is_exposed_without_a_token_in_argv() {
    let parsed = command(&[
        "codewhale",
        "account",
        "agents",
        "work-launch",
        WORK_ID,
        "--operation-key",
        "launch-stdin",
        "--confirmation",
        "-",
        "--confirm-eu-compute",
    ]);
    assert!(matches!(parsed, CloudCommand::Agents(CloudAgentsArgs {
        command: CloudAgentsCommand::WorkLaunch { confirmation, confirm_eu_compute: true, .. }
    }) if confirmation == "-"));
    let help = Cli::try_parse_from(["codewhale", "account", "agents", "work-launch", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("bounded piped stdin"));
}

#[test]
fn work_confirmation_stdin_accepts_a_single_proof_with_optional_line_ending() {
    for input in [
        "consent.abc-123_DEF",
        "consent.abc-123_DEF\n",
        "consent.abc-123_DEF\r\n",
    ] {
        assert_eq!(
            work::read_confirmation(input.as_bytes()).unwrap(),
            "consent.abc-123_DEF"
        );
    }
    let maximum = "A".repeat(4096);
    assert_eq!(
        work::read_confirmation(format!("{maximum}\r\n").as_bytes()).unwrap(),
        maximum
    );
}

#[test]
fn work_confirmation_stdin_rejects_untrusted_input_without_echo_and_bounds_reads() {
    for input in [
        "",
        "consent SECRET",
        "consent.$(SECRET)",
        "consent.SECRET\nsecond",
        "consent.SECRET\n\n",
        "consent.SECRET\r",
    ] {
        let error = chain(&work::read_confirmation(input.as_bytes()).unwrap_err());
        assert!(!error.contains("SECRET"));
    }
    assert!(
        work::read_confirmation(&[0xff][..])
            .unwrap_err()
            .to_string()
            .contains("UTF-8")
    );
    let mut huge = std::io::Cursor::new(vec![b'A'; 1_000_000]);
    let error = chain(&work::read_confirmation(&mut huge).unwrap_err());
    assert!(error.contains("too long"));
    assert_eq!(
        huge.position(),
        4099,
        "read is bounded even if the pipe contains arbitrarily much input"
    );
}

#[test]
fn work_confirmation_stdin_proof_reaches_only_the_confirmed_request() {
    let secrets = signed_in();
    let transport = ScriptedTransport::new(vec![
        reply(200, work_run("queued")),
        agents_step(),
        projects_step(),
        reply(201, cloud_session()),
    ]);
    let client = CloudClient::new(&transport, &secrets, "default", DEFAULT_API_BASE);
    let proof = work::read_confirmation(&b"consent.private-proof\n"[..]).unwrap();
    let mut output = Vec::new();
    work::launch(
        &client,
        &mut output,
        WORK_ID,
        "launch-stdin",
        &proof,
        true,
        Some(300),
        None,
    )
    .unwrap();
    assert_eq!(
        body_of(&transport.requests()[3])["launchQuoteConfirmation"],
        proof
    );
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains(&proof));
    assert!(!output.contains("access-secret") && !output.contains("refresh-secret"));
}

#[test]
fn work_launch_requires_eu_consent_and_sends_the_confirmed_c5_request() {
    let secrets = signed_in();
    // No consent flag: nothing is read or sent.
    let transport = ScriptedTransport::new(vec![]);
    let without_consent = &LAUNCH_ARGV[..LAUNCH_ARGV.len() - 1];
    let (result, output) = run_agent_command(&transport, &secrets, without_consent);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("--confirm-eu-compute"));
    assert!(message.contains("Nothing was sent"));
    assert!(output.is_empty());
    assert!(transport.requests().is_empty());

    for token in ["tok.$(command)", "tok.`command`", "tok.abc;command"] {
        let mut unsafe_token = LAUNCH_ARGV.to_vec();
        unsafe_token[8] = token;
        let (result, _) = run_agent_command(&transport, &secrets, &unsafe_token);
        assert!(chain(&result.unwrap_err()).contains("Confirmation must be"));
        assert!(transport.requests().is_empty());
    }

    // A malformed confirmation is refused locally too.
    let mut spaced = LAUNCH_ARGV.to_vec();
    spaced[8] = "tok en";
    let (result, _) = run_agent_command(&transport, &secrets, &spaced);
    result.unwrap_err();
    assert!(transport.requests().is_empty());

    let transport = ScriptedTransport::new(vec![
        reply(200, work_run("queued")),
        agents_step(),
        projects_step(),
        reply(201, cloud_session()),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests[3].path, "/api/cloud-sessions");
    assert_eq!(requests[3].method, HttpMethod::Post);
    assert_eq!(
        body_of(&requests[3]),
        json!({
            "workRunId": WORK_ID, "agentId": "agent-1", "projectId": "project-codewhale",
            "repo": "octo-org/app", "provider": "github", "prompt": "Fix the build",
            "runnerKind": "hosted", "sandboxSku": "task-small", "estimatedSeconds": 300,
            "modelProvider": "deepseek", "model": "deepseek-flash",
            "billingMode": "byok_external", "computeRegion": "eu",
            "sandboxTargetRegion": "eu", "crossRegionSandboxOptIn": true,
            "operationKey": "launch-1",
            "launchQuoteConfirmation": "tok.abc-123_DEF",
            "customerEuPlacementConsent": true
        })
    );
    for expected in [
        "Work launched on a cloud computer.",
        &format!("Work ID: {WORK_ID}"),
        "Status: planning",
        "Computer: running",
        "Compute region: eu",
        "Codewhale credits charged: $0",
        "Attempt: accepted",
        "First turn: queued (not complete yet)",
        "Operation key: launch-1",
        &format!("work-cancel {WORK_ID}"),
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in:\n{output}"
        );
    }
    assert!(!output.contains("access-secret") && !output.contains("refresh-secret"));
}

#[test]
fn work_launch_is_replay_safe_after_the_work_has_already_started() {
    // Once the first launch was accepted the Work is no longer queued. The same
    // command must still reach the operation ledger (which replays the receipt)
    // instead of dying on a local "not queued" check.
    let secrets = signed_in();
    let transport = ScriptedTransport::new(vec![
        reply(200, work_run("running")),
        reply(200, cloud_session()),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/api/cloud-sessions");
    assert_eq!(body_of(&requests[1])["operationKey"], "launch-1");
    assert!(output.contains("Work launched"));
}

#[test]
fn work_launch_reports_unknown_and_refused_outcomes_without_guessing() {
    let secrets = signed_in();
    let steps = |last: Scripted| vec![reply(200, work_run("running")), last];
    for lost in [
        Scripted::Unreachable,
        reply(504, json!({ "code": "gateway_timeout" })),
        reply(409, json!({ "code": "boat_task_outcome_unknown" })),
        reply(409, json!({ "code": "boat_task_receipt_invalid" })),
        reply(
            409,
            json!({ "code": "provider_receipt_conflict", "reconciliationRequired": true }),
        ),
        Scripted::Reply(CloudResponse {
            status: 201,
            body: b"{".to_vec(),
            retry_after: None,
        }),
    ] {
        let transport = ScriptedTransport::new(steps(lost));
        let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
        let message = chain(&result.unwrap_err());
        assert!(message.contains("launch outcome is unknown"), "{message}");
        assert!(
            message.contains("a computer may already be running"),
            "{message}"
        );
        assert!(
            message.contains(&format!("work-status {WORK_ID}")),
            "{message}"
        );
        assert!(
            message.contains(&format!("work-cancel {WORK_ID}")),
            "{message}"
        );
        assert!(
            message.contains("same --operation-key and --confirmation"),
            "{message}"
        );
        assert!(output.is_empty());
        assert!(!message.contains("tok.abc-123_DEF") && !message.contains("access-secret"));
    }
    let transport = ScriptedTransport::new(steps(reply(
        409,
        json!({ "code": "boat_task_replay_expired" }),
    )));
    let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("launch outcome is unknown"));
    assert!(message.contains("operator reconciliation is required"));
    assert!(message.contains("Do not submit a new operation key"));
    assert!(!message.contains("re-run this exact command"));
    assert!(output.is_empty());

    let transport =
        ScriptedTransport::new(steps(reply(409, json!({ "code": "launch_in_progress" }))));
    let (result, _) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("still being recorded"), "{message}");
    assert!(!message.contains("outcome is unknown"));

    let transport = ScriptedTransport::new(steps(reply(
        422,
        json!({ "code": "hosted_launch_quote_expired" }),
    )));
    let (result, _) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("hosted_launch_quote_expired"));
    assert!(message.contains("`work-quote` again with the same --operation-key"));
    assert!(!message.contains("outcome is unknown"));

    let transport = ScriptedTransport::new(steps(reply(
        409,
        json!({ "code": "launch_operation_mismatch" }),
    )));
    let (result, _) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("Check `work-status` and `work-result`"));
    assert!(message.contains("never replace the key to retry an unknown launch"));

    // A reply about another Work is never presented as this launch.
    let mut other = cloud_session();
    other["session"]["run"]["id"] = json!("22222222-2222-4222-8222-222222222222");
    let transport = ScriptedTransport::new(steps(reply(201, other)));
    let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
    assert!(chain(&result.unwrap_err()).contains("launched a different Work"));
    assert!(!output.contains("Work launched"));

    // A 2xx without the `session` document (or with a non-object one) means the
    // service acted but the client cannot say how: an unknown outcome, never a
    // success and never "a different Work".
    for missing in [
        json!({}),
        json!({ "session": "nope" }),
        cloud_session()["session"].clone(),
    ] {
        let transport = ScriptedTransport::new(steps(reply(201, missing)));
        let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
        let message = chain(&result.unwrap_err());
        assert!(message.contains("launch outcome is unknown"), "{message}");
        assert!(message.contains("without a session"), "{message}");
        assert!(!message.contains("different Work"), "{message}");
        assert!(!output.contains("Work launched"));
    }

    for missing in [
        json!({ "session": {} }),
        json!({ "session": { "run": {} } }),
    ] {
        let transport = ScriptedTransport::new(steps(reply(201, missing)));
        let (result, output) = run_agent_command(&transport, &secrets, &LAUNCH_ARGV);
        let message = chain(&result.unwrap_err());
        assert!(message.contains("launch outcome is unknown"), "{message}");
        assert!(message.contains("without a Work ID"), "{message}");
        assert!(!message.contains("different Work"), "{message}");
        assert!(!output.contains("Work launched"));
    }
}

fn binding(id: &str, repo: &str, installation: &str, status: &str) -> serde_json::Value {
    json!({ "id": id, "provider": "github", "repo": repo, "status": status, "installationId": installation })
}

#[test]
fn github_bind_connects_a_repository_through_the_known_installation() {
    let secrets = signed_in();
    let transport = ScriptedTransport::new(vec![
        reply(
            200,
            json!({ "bindings": [binding("github:acct-123:987:octo-org/other", "octo-org/other", "987", "connected")] }),
        ),
        reply(
            201,
            json!({ "binding": binding("github:acct-123:987:octo-org/app", "octo-org/app", "987", "connected") }),
        ),
    ]);
    let (result, output) = run_github_command(
        &transport,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &["codewhale", "account", "github", "bind", "octo-org/app"],
    );
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(requests[0].path, "/api/integrations/github/bindings");
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert_eq!(requests[1].path, "/api/integrations/github/bindings");
    assert_eq!(
        body_of(&requests[1]),
        json!({ "installationId": "987", "repo": "octo-org/app" })
    );
    assert!(
        output.contains("Connected octo-org/app: github:acct-123:987:octo-org/app (connected)")
    );
    assert!(output.contains("--repo-binding-id github:acct-123:987:octo-org/app"));

    // An explicit installation ID is used as given.
    let transport = ScriptedTransport::new(vec![
        reply(200, json!({ "bindings": [] })),
        reply(
            200,
            json!({ "binding": binding("github:acct-123:55:octo-org/app", "octo-org/app", "55", "connected") }),
        ),
    ]);
    let (result, _) = run_github_command(
        &transport,
        &secrets,
        &machine::MachineKeyEnv::default(),
        &[
            "codewhale",
            "account",
            "github",
            "bind",
            "octo-org/app",
            "--installation-id",
            "55",
        ],
    );
    result.unwrap();
    assert_eq!(body_of(&transport.requests()[1])["installationId"], "55");
}

#[test]
fn github_bind_refuses_ambiguity_bad_input_and_lost_replies() {
    let secrets = signed_in();
    let plain = machine::MachineKeyEnv::default();
    let bind = ["codewhale", "account", "github", "bind", "octo-org/app"];

    // Already connected: no write at all.
    let transport = ScriptedTransport::new(vec![reply(
        200,
        json!({ "bindings": [binding("github:acct-123:987:octo-org/app", "Octo-Org/App", "987", "connected")] }),
    )]);
    let (result, output) = run_github_command(&transport, &secrets, &plain, &bind);
    result.unwrap();
    assert!(output.contains("is already connected"));
    assert_eq!(transport.requests().len(), 1);

    // Several installations: the user must choose.
    let transport = ScriptedTransport::new(vec![reply(
        200,
        json!({ "bindings": [
            binding("b1", "octo-org/one", "111", "connected"),
            binding("b2", "octo-org/two", "222", "connected")
        ] }),
    )]);
    let (result, _) = run_github_command(&transport, &secrets, &plain, &bind);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("several GitHub App installations (111, 222)"));
    assert_eq!(transport.requests().len(), 1);

    // No installation known at all.
    let transport = ScriptedTransport::new(vec![reply(200, json!({ "bindings": [] }))]);
    let (result, _) = run_github_command(&transport, &secrets, &plain, &bind);
    assert!(chain(&result.unwrap_err()).contains("--installation-id"));

    // Malformed repository and installation fail before any request.
    let transport = ScriptedTransport::new(vec![]);
    for argv in [
        vec!["codewhale", "account", "github", "bind", "octo-org"],
        vec![
            "codewhale",
            "account",
            "github",
            "bind",
            "octo-org/app/extra",
        ],
        vec!["codewhale", "account", "github", "bind", "../app"],
        vec![
            "codewhale",
            "account",
            "github",
            "bind",
            "octo-org/app",
            "--installation-id",
            "12x",
        ],
    ] {
        run_github_command(&transport, &secrets, &plain, &argv)
            .0
            .unwrap_err();
    }
    assert!(transport.requests().is_empty());

    // The server refusing (not the owner, no repository access) keeps its code.
    let transport = ScriptedTransport::new(vec![
        reply(
            200,
            json!({ "bindings": [binding("b1", "octo-org/one", "111", "connected")] }),
        ),
        reply(
            403,
            json!({ "code": "github_repository_user_access_denied" }),
        ),
    ]);
    let (result, _) = run_github_command(&transport, &secrets, &plain, &bind);
    let message = chain(&result.unwrap_err());
    assert!(message.contains("github_repository_user_access_denied"));
    assert!(!message.contains("outcome is unknown"));

    // A lost reply says to check the list before repeating.
    for last in [
        Scripted::Unreachable,
        reply(
            503,
            json!({ "code": "github_repository_bindings_unavailable" }),
        ),
    ] {
        let transport = ScriptedTransport::new(vec![
            reply(
                200,
                json!({ "bindings": [binding("b1", "octo-org/one", "111", "connected")] }),
            ),
            last,
        ]);
        let (result, _) = run_github_command(&transport, &secrets, &plain, &bind);
        let message = chain(&result.unwrap_err());
        assert!(message.contains("bind outcome is unknown"));
        assert!(message.contains("account github bindings"));
    }

    // A binding for a different repository is not accepted as this one.
    let transport = ScriptedTransport::new(vec![
        reply(
            200,
            json!({ "bindings": [binding("b1", "octo-org/one", "111", "connected")] }),
        ),
        reply(
            201,
            json!({ "binding": binding("b9", "octo-org/elsewhere", "111", "connected") }),
        ),
    ]);
    let (result, output) = run_github_command(&transport, &secrets, &plain, &bind);
    assert!(chain(&result.unwrap_err()).contains("different repository"));
    assert!(!output.contains("Connected"));

    // A machine key never binds repositories.
    let transport = ScriptedTransport::new(vec![]);
    let (result, _) = run_github_command(&transport, &secrets, &machine_env(), &bind);
    assert!(chain(&result.unwrap_err()).contains("interactive Codewhale account login"));
    assert!(transport.requests().is_empty());
}

#[test]
fn new_thread_refuses_a_route_the_live_catalog_does_not_list() {
    let secrets = signed_in();
    let argv = |provider: &'static str, model: &'static str| {
        vec![
            "codewhale",
            "account",
            "agents",
            "new-thread",
            "Whale",
            "--provider",
            provider,
            "--model",
            model,
            "--operation-key",
            "thread-1",
        ]
    };
    for (provider, model, expect) in [
        (
            "deepseek",
            "deepseek-v9",
            "catalog lists these models for `deepseek`: deepseek-v4-pro, deepseek-flash",
        ),
        (
            "openai",
            "gpt-x",
            "is not a hosted provider in the catalog. Known providers: deepseek",
        ),
    ] {
        let transport = ScriptedTransport::new(vec![agents_step(), reply(200, route_catalog())]);
        let (result, output) = run_agent_command(&transport, &secrets, &argv(provider, model));
        let message = chain(&result.unwrap_err());
        assert!(
            message.contains("live Codewhale model catalog"),
            "{message}"
        );
        assert!(message.contains("GET /api/model-providers"), "{message}");
        assert!(message.contains(expect), "{message}");
        assert!(output.is_empty());
        // Only reads happened: no conversation was created.
        let requests = transport.requests();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            ["/api/agents", "/api/model-providers"]
        );
        assert!(
            requests
                .iter()
                .all(|request| request.method == HttpMethod::Get)
        );
    }
    // A runtime-only row is not a hosted route even when it lists the model.
    let mut catalog = route_catalog();
    catalog["providers"][0]["connectionAvailable"] = json!(false);
    let transport = ScriptedTransport::new(vec![agents_step(), reply(200, catalog)]);
    let (result, _) = run_agent_command(&transport, &secrets, &argv("deepseek", "deepseek-flash"));
    result.unwrap_err();
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn new_thread_sends_the_canonical_route_id_when_given_a_runtime_alias() {
    // `xiaomi-mimo` is the runtime name of the catalog row `xiaomi`; the server
    // stores `xiaomi`, so the preflight must resolve to it before creating.
    let secrets = signed_in();
    let argv = [
        "codewhale",
        "account",
        "agents",
        "new-thread",
        "Whale",
        "--provider",
        "xiaomi-mimo",
        "--model",
        "mimo-v2",
        "--operation-key",
        "thread-1",
    ];
    let catalog = json!({ "providers": [{
        "id": "xiaomi",
        "runtimeProvider": "xiaomi-mimo",
        "connectionAvailable": true,
        "models": ["mimo-v2"]
    }] });
    let mut created = agent_thread("thread-1", "agent-1", "Main");
    created["modelProvider"] = json!("xiaomi");
    created["model"] = json!("mimo-v2");
    let transport = ScriptedTransport::new(vec![
        agents_step(),
        reply(200, catalog),
        reply(201, json!({ "thread": created })),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &argv);
    result.unwrap();
    let requests = transport.requests();
    assert_eq!(body_of(&requests[2])["modelProvider"], "xiaomi");
    assert!(output.contains("Model: xiaomi/mimo-v2"), "{output}");
}

#[test]
fn new_thread_asserts_the_created_route_equals_the_requested_one() {
    let secrets = signed_in();
    let argv = [
        "codewhale",
        "account",
        "agents",
        "new-thread",
        "Whale",
        "--operation-key",
        "thread-1",
    ];
    let mut swapped = agent_thread("thread-1", "agent-1", "Main");
    swapped["model"] = json!("deepseek-v4-pro");
    let transport = ScriptedTransport::new(vec![
        agents_step(),
        reply(200, route_catalog()),
        reply(201, json!({ "thread": swapped })),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &argv);
    let message = chain(&result.unwrap_err());
    assert!(
        message
            .contains("deepseek/deepseek-v4-pro instead of the requested deepseek/deepseek-flash"),
        "{message}"
    );
    assert!(message.contains("Do not send to it"));
    assert!(output.is_empty());

    let mut other_provider = agent_thread("thread-1", "agent-1", "Main");
    other_provider["modelProvider"] = json!("openrouter");
    let transport = ScriptedTransport::new(vec![
        agents_step(),
        reply(200, route_catalog()),
        reply(201, json!({ "thread": other_provider })),
    ]);
    let (result, _) = run_agent_command(&transport, &secrets, &argv);
    assert!(chain(&result.unwrap_err()).contains("instead of the requested"));

    // The defaults are the DeepSeek V4.1 Flash route and are accepted when honored.
    let transport = ScriptedTransport::new(vec![
        agents_step(),
        reply(200, route_catalog()),
        reply(
            201,
            json!({ "thread": agent_thread("thread-1", "agent-1", "Main") }),
        ),
    ]);
    let (result, output) = run_agent_command(&transport, &secrets, &argv);
    result.unwrap();
    assert!(output.contains("Model: deepseek/deepseek-flash"));
}

#[test]
fn account_errors_stay_typed_so_callers_can_tell_refusals_from_unknown_outcomes() {
    let refusal = response_error(&response(
        422,
        json!({ "code": "boat_work_trial_seconds_invalid" }),
    ));
    assert_eq!(
        refusal.to_string(),
        "Codewhale account request failed (HTTP 422)"
    );
    assert!(!outcome_unknown(&refusal));
    for status in [500, 502, 503, 504, 408] {
        assert!(
            outcome_unknown(&response_error(&response(status, json!({})))),
            "{status}"
        );
    }
    for status in [400, 401, 404, 409, 422, 429] {
        assert!(
            !outcome_unknown(&response_error(&response(status, json!({})))),
            "{status}"
        );
    }
    let lost: anyhow::Error = CloudTransportError::new(
        "could not reach the Codewhale service",
        std::io::Error::other("reset"),
    )
    .into();
    assert!(outcome_unknown(&lost));
    // Context layers do not hide the typed cause.
    assert!(outcome_unknown(&lost.context("while cancelling")));
    assert!(!outcome_unknown(&anyhow!("Not signed in")));
    // A hostile code is dropped, never echoed.
    let hostile = response_error(&response(422, json!({ "code": "bad code\u{1b}[31m" })));
    assert_eq!(
        hostile.to_string(),
        "Codewhale account request failed (HTTP 422)"
    );
}

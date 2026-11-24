//! Exercise the public CLI boundary: the runtime reloads configuration after
//! dispatch, so testing the outer store alone cannot catch lost overrides.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = r#"
provider = "deepseek"
default_text_model = "deepseek-v4-flash"
sandbox_mode = "workspace-write"
approval_policy = "never"
telemetry = false

[profiles.review]
provider = "openrouter"
default_text_model = "profile-model"
sandbox_mode = "danger-full-access"
approval_policy = "untrusted"
"#;

struct Fixture {
    root: TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new().unwrap();
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Self {
            root,
            config: config_path,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(
            std::env::var_os("CARGO_BIN_EXE_codewhale")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_codewhale"))),
        );
        command
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("USERPROFILE", self.root.path().join("home"))
            .env("CODEWHALE_HOME", self.root.path().join("state"))
            .env("CODEWHALE_SECRET_BACKEND", "file")
            .env("CODEWHALE_TELEMETRY", "0")
            .stdin(Stdio::null())
            .arg("--config")
            .arg(&self.config)
            .arg("--no-project-config")
            .args(args);
        // Doctor may inspect rustc. Keep rustup's initialization outside the
        // sealed fixture, while withholding all provider/account variables.
        for name in ["PATH", "RUSTUP_HOME", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run public codewhale")
    }

    fn doctor(&self, args: &[&str]) -> Value {
        let output = self
            .command(args)
            .args(["doctor", "--json"])
            .output()
            .unwrap();
        success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn unchanged(&self) {
        assert_eq!(fs::read_to_string(&self.config).unwrap(), CONFIG);
        assert!(
            !self.root.path().join("state").exists(),
            "diagnostic must not create state"
        );
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn route(report: &Value) -> (&str, &str) {
    let route = &report["setup"]["provider_model"];
    (
        route["provider"]["id"].as_str().unwrap(),
        route["model"]["resolved"].as_str().unwrap(),
    )
}

fn posture<'a>(report: &'a Value, key: &str) -> &'a Value {
    &report["setup"]["runtime_posture"][key]["value"]
}

#[test]
fn runtime_set_reaches_the_public_runtime_and_does_not_persist() {
    let fixture = Fixture::new(CONFIG);
    let baseline = fixture.doctor(&[]);
    assert_eq!(route(&baseline), ("deepseek", "deepseek-v4-flash"));
    assert_eq!(posture(&baseline, "sandbox_mode"), "workspace-write");
    let report = fixture.doctor(&[
        "--set",
        "sandbox_mode=read-only",
        "--set",
        "approval_policy=on-request",
        "--set",
        "model=deepseek-v4-pro",
        "--set",
        "telemetry=false",
    ]);
    assert_eq!(route(&report), ("deepseek", "deepseek-v4-pro"));
    assert_eq!(posture(&report, "sandbox_mode"), "read-only");
    assert_eq!(posture(&report, "approval_policy"), "on-request");
    assert_eq!(posture(&report, "telemetry"), false);
    let after = fixture.doctor(&[]);
    assert_eq!(route(&after), route(&baseline));
    assert_eq!(posture(&after, "sandbox_mode"), "workspace-write");
    fixture.unchanged();
}

#[test]
fn profile_defaults_survive_unrelated_overrides_and_yield_to_explicit_routes() {
    let fixture = Fixture::new(CONFIG);
    let report = fixture.doctor(&["--profile", "review", "--set", "sandbox_mode=read-only"]);
    assert_eq!(route(&report), ("openrouter", "profile-model"));
    assert_eq!(posture(&report, "approval_policy"), "untrusted");
    assert_eq!(posture(&report, "sandbox_mode"), "read-only");
    let report = fixture.doctor(&[
        "--profile",
        "review",
        "--set",
        "provider=deepseek",
        "--set",
        "default_text_model=deepseek-v4-pro",
    ]);
    assert_eq!(route(&report), ("deepseek", "deepseek-v4-pro"));
    assert_eq!(posture(&report, "sandbox_mode"), "danger-full-access");
    fixture.unchanged();
}

#[test]
fn dedicated_flags_win_in_either_order_and_repeated_aliases_use_the_last_value() {
    let fixture = Fixture::new(CONFIG);
    let flags = [
        "--provider",
        "deepseek",
        "--model",
        "deepseek-v4-flash",
        "--sandbox-mode",
        "workspace-write",
        "--approval-policy",
        "on-request",
    ];
    let overrides = [
        "--set",
        "provider=openrouter",
        "--set",
        "model=other-model",
        "--set",
        "sandbox_mode=read-only",
        "--set",
        "approval_policy=never",
    ];
    for args in [
        flags.iter().chain(&overrides).copied().collect::<Vec<_>>(),
        overrides.iter().chain(&flags).copied().collect::<Vec<_>>(),
    ] {
        let report = fixture.doctor(&args);
        assert_eq!(route(&report), ("deepseek", "deepseek-v4-flash"));
        assert_eq!(posture(&report, "sandbox_mode"), "workspace-write");
        assert_eq!(posture(&report, "approval_policy"), "on-request");
    }
    let report = fixture.doctor(&[
        "--set",
        "model=deepseek-v4-flash",
        "--set",
        "default_text_model=deepseek-v4-pro",
        "--set",
        "sandbox_mode=workspace-write",
        "--set",
        "sandbox_mode=read-only",
    ]);
    assert_eq!(route(&report), ("deepseek", "deepseek-v4-pro"));
    assert_eq!(posture(&report, "sandbox_mode"), "read-only");
    fixture.unchanged();
}

#[test]
fn managed_policy_keeps_authority_over_runtime_set_and_dedicated_flags() {
    let fixture = Fixture::new(CONFIG);
    let managed = fixture.root.path().join("managed.toml");
    fs::write(&managed, "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-flash\"\nsandbox_mode = \"read-only\"\napproval_policy = \"untrusted\"\n").unwrap();
    let output = fixture
        .command(&[
            "--profile",
            "review",
            "--set",
            "provider=openrouter",
            "--set",
            "model=other-model",
            "--set",
            "sandbox_mode=danger-full-access",
            "--approval-policy",
            "never",
        ])
        .env("CODEWHALE_MANAGED_CONFIG_PATH", &managed)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    success(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(route(&report), ("deepseek", "deepseek-v4-flash"));
    assert_eq!(posture(&report, "sandbox_mode"), "read-only");
    assert_eq!(posture(&report, "approval_policy"), "untrusted");
    fixture.unchanged();
}

#[test]
fn managed_requirements_reject_an_incompatible_temporary_sandbox() {
    let fixture = Fixture::new(CONFIG);
    let requirements = fixture.root.path().join("requirements.toml");
    fs::write(&requirements, "allowed_sandbox_modes = [\"read-only\"]\n").unwrap();
    let output = fixture
        .command(&[
            "--set",
            "sandbox_mode=danger-full-access",
            "doctor",
            "--json",
        ])
        .env("CODEWHALE_REQUIREMENTS_PATH", requirements)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["kind"], "config_validation");
    fixture.unchanged();
}

#[test]
fn unsupported_values_auth_and_legacy_transports_fail_before_config_or_secret_access() {
    const SENTINEL: &str = "synthetic-override-secret";
    let fixture = Fixture::new("invalid = [synthetic-config-secret\n");
    for key in [
        "api_key",
        "auth.mode",
        "base_url",
        "providers.openai.api_key",
        "not_a_key",
    ] {
        let output = fixture.run(&["--set", &format!("{key}={SENTINEL}"), "doctor", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported runtime --set key"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SENTINEL));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-config-secret"));
    }
    for args in [
        vec!["auth", "status"],
        vec!["auth", "print-api-key", "--provider", "deepseek"],
        vec!["app-server"],
        vec!["app-server", "--stdio"],
        vec!["app-server", "--socket"],
    ] {
        let output = fixture
            .command(&["--set", "sandbox_mode=read-only"])
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--set is not supported"),
            "{output:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-config-secret"));
    }
    for spec in [
        "missing-equals",
        "model=",
        "model=   ",
        "telemetry=maybe",
        "provider=bad/id",
    ] {
        let output = fixture.run(&["--set", spec, "doctor", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-config-secret"));
    }
    assert!(!fixture.root.path().join("state").exists());
}

#[test]
fn runtime_values_cannot_leak_through_a_command_that_saves_the_store() {
    let fixture = Fixture::new(CONFIG);
    let output = fixture.run(&[
        "--set",
        "sandbox_mode=read-only",
        "--set",
        "provider=openrouter",
        "--set",
        "model=temporary-model",
        "model",
        "set",
        "deepseek-v4-pro",
    ]);
    success(&output);
    let saved: Value = serde_json::to_value(
        toml::from_str::<toml::Value>(&fs::read_to_string(&fixture.config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["provider"], "deepseek");
    assert_eq!(saved["sandbox_mode"], "workspace-write");
    // Selected models persist in the canonical per-provider slot; root
    // default_text_model is legacy fallback only.
    assert_eq!(saved["providers"]["deepseek"]["model"], "deepseek-v4-pro");
    assert!(
        !fs::read_to_string(&fixture.config)
            .unwrap()
            .contains("temporary-model")
    );
}

#[test]
fn named_provider_and_model_reach_an_actual_exec_request_with_a_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let mock = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        assert_eq!(request.trim(), "POST /v1/chat/completions HTTP/1.1");
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let length = length.expect("request content length");
        assert!(length <= 1024 * 1024);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "id": "fixture", "object": "chat.completion.chunk", "model": "temporary-model",
                "choices": [{"index": 0, "delta": {"content": "LOCAL_SET_ACCEPTED"}, "finish_reason": "stop"}]
            })
        );
        write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        body
    });
    let config = format!(
        "{CONFIG}\n[providers.fixture_route]\nkind = \"openai-compatible\"\nbase_url = \"{endpoint}\"\nmodel = \"saved-model\"\napi_key = \"synthetic-fixture-key\"\n"
    );
    let fixture = Fixture::new(&config);
    let output = fixture.run(&[
        "--profile",
        "review",
        "--set",
        "provider=fixture_route",
        "--set",
        "model=temporary-model",
        "--set",
        "sandbox_mode=read-only",
        "exec",
        // Plain exec is a zero-tool one-shot (#6510); `--auto` opens the tool
        // surface so the read-only `--set` is proven to filter it.
        "--auto",
        "--max-turns",
        "1",
        "--output-format",
        "stream-json",
        "reply briefly",
    ]);
    success(&output);
    let request = mock.join().unwrap();
    assert_eq!(request["model"], "temporary-model");
    assert!(
        !request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "Bash")
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "content" && event["content"] == "LOCAL_SET_ACCEPTED"),
        "{events:?}"
    );
    assert_eq!(events.last().unwrap()["type"], "done");
    let receipt = events
        .iter()
        .find(|event| event["type"] == "metadata")
        .unwrap();
    assert_eq!(receipt["meta"]["provider_id"], "fixture_route");
    assert_eq!(receipt["meta"]["model"], "temporary-model");
    assert_eq!(fs::read_to_string(&fixture.config).unwrap(), config);
}

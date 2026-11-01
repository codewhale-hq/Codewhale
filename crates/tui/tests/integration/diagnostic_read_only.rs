//! Process-level regression coverage for read-only diagnostic commands.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use codewhale_secrets::{FileKeyringStore, KeyringStore};
use tempfile::TempDir;

#[test]
fn doctor_text_leaves_a_sealed_home_untouched() {
    let output = run_sealed_diagnostic(["doctor"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("codewhale Doctor"), "stdout:\n{stdout}");
}

#[test]
fn doctor_json_leaves_a_sealed_home_untouched() {
    let output = run_sealed_diagnostic(["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor --json must remain machine-readable: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert_eq!(report["api_connectivity"]["checked"], false);
}

#[test]
fn doctor_json_rejects_kimi_code_claude_alias_with_machine_readable_guidance() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let config = workspace.join("kimi-invalid.toml");
    let config_bytes = br#"provider = "moonshot"

[providers.moonshot]
api_key = "doctor-json-kimi-secret"
base_url = "https://api.kimi.com/coding/v1"
model = "k3[1m]"
"#;
    fs::write(&config, config_bytes).expect("write invalid Kimi config");

    let mut command = diagnostic_command(&workspace, &home);
    command
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--json",
        ])
        .env("CODEWHALE_HOME", &codewhale_home);
    let output = command.output().expect("run invalid Kimi doctor json");

    assert!(
        !output.status.success(),
        "invalid configuration must return nonzero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("invalid doctor --json output must remain machine-readable");
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], "config_validation");
    let message = report["error"]["message"]
        .as_str()
        .expect("config error message");
    assert!(message.contains("model = \"k3\""), "{message}");
    assert!(message.contains("context_window = 1048576"), "{message}");
    assert!(message.contains("plan includes 1M context"), "{message}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("doctor configuration validation failed; see JSON output"),
        "stderr must point only to the JSON envelope: {stderr}"
    );
    for stdout_only_detail in ["k3[1m]", "context_window", "plan includes 1M context"] {
        assert!(
            !stderr.contains(stdout_only_detail),
            "actionable validation details belong only in redacted stdout JSON: {stderr}"
        );
    }

    let all_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all_output.contains("doctor-json-kimi-secret"));
    assert_eq!(
        fs::read(&config).expect("read config after doctor"),
        config_bytes,
        "doctor must not rewrite an invalid config"
    );
    assert!(!home.exists(), "doctor must not create HOME state");
    assert!(
        !codewhale_home.exists(),
        "doctor must not create CODEWHALE_HOME state"
    );
}

#[test]
fn doctor_json_parse_failure_never_enters_provider_dispatch() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("owned provider listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let endpoint = listener.local_addr().expect("provider address");
    let config = workspace.join("malformed.toml");
    let config_bytes = format!(
        "provider = \"openai\"\n[providers.openai]\nbase_url = \"http://{endpoint}/v1\"\napi_key = \"doctor-malformed-secret\"\nmodel =\n"
    );
    fs::write(&config, &config_bytes).expect("write malformed source");
    let output = diagnostic_command(&workspace, &home)
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--json",
        ])
        .env("CODEWHALE_HOME", &codewhale_home)
        .output()
        .expect("run malformed doctor json");

    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable parse error");
    assert_eq!(report["error"]["kind"], "config_validation");
    let all_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all_output.contains("doctor-malformed-secret"));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a rejected diagnostic source must not contact its provider"
    );
    assert_eq!(
        fs::read(&config).expect("source after report"),
        config_bytes.as_bytes()
    );
    assert!(!home.exists(), "doctor must not create HOME state");
    assert!(
        !codewhale_home.exists(),
        "doctor must not create Codewhale state"
    );
}

#[test]
fn doctor_json_omits_untrusted_config_validation_details() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let config = workspace.join("untrusted-invalid.toml");
    let config_bytes = br#"provider = "doctor-untrusted-provider-secret"
api_key = "doctor-json-arbitrary-secret"
"#;
    fs::write(&config, config_bytes).expect("write invalid config");

    let mut command = diagnostic_command(&workspace, &home);
    command
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--json",
        ])
        .env("CODEWHALE_HOME", &codewhale_home);
    let output = command.output().expect("run invalid doctor json");

    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable doctor error");
    assert_eq!(report["error"]["kind"], "config_validation");
    // A plain value error names the key and the valid values, never the
    // rejected value itself (it may be a pasted secret).
    let message = report["error"]["message"].as_str().expect("message");
    assert!(
        message.starts_with("Invalid provider (value not shown): expected deepseek"),
        "{message}"
    );
    let fix = report["error"]["fix"].as_str().expect("fix");
    assert!(
        fix.starts_with("codewhale config set provider deepseek (if CODEWHALE_PROVIDER"),
        "{fix}"
    );
    let all_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all_output.contains("doctor-untrusted-provider-secret"));
    assert!(!all_output.contains("doctor-json-arbitrary-secret"));
    assert_eq!(
        fs::read(&config).expect("read config after doctor"),
        config_bytes
    );
    assert!(!home.exists());
    assert!(!codewhale_home.exists());
}

#[test]
fn doctor_json_reports_valid_kimi_code_k3_context_override_from_runtime_route() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let config = workspace.join("kimi-valid.toml");
    let config_bytes = br#"provider = "moonshot"

[providers.moonshot]
api_key = "doctor-json-valid-kimi-secret"
base_url = "https://api.kimi.com/coding/v1"
model = "k3"
context_window = 1048576
"#;
    fs::write(&config, config_bytes).expect("write valid Kimi config");

    let mut command = diagnostic_command(&workspace, &home);
    command
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--json",
        ])
        .env("CODEWHALE_HOME", &codewhale_home);
    let output = command.output().expect("run valid Kimi doctor json");

    assert!(
        output.status.success(),
        "valid Kimi doctor --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable doctor report");
    assert_eq!(report["route"]["model"], "k3");
    assert_eq!(report["route"]["context_window"]["tokens"], 1_048_576);
    assert_eq!(report["route"]["context_window"]["source"], "configured");
    assert!(report["route"]["route_error"].is_null());
    assert_eq!(report["capability"]["resolved_model"], "k3");
    assert_eq!(report["capability"]["context_window"], 1_048_576);
    assert_eq!(report["capability"]["context_window_source"], "configured");
    assert!(report["capability"]["route_error"].is_null());

    let all_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all_output.contains("doctor-json-valid-kimi-secret"));
    assert_eq!(
        fs::read(&config).expect("read config after doctor"),
        config_bytes,
        "doctor must not rewrite a valid config"
    );
    assert!(!home.exists(), "doctor must not create HOME state");
    assert!(
        !codewhale_home.exists(),
        "doctor must not create CODEWHALE_HOME state"
    );
}

#[test]
fn doctor_context_json_leaves_a_sealed_home_untouched() {
    let output = run_sealed_diagnostic(["doctor", "--context-json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor --context-json must remain machine-readable: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert!(
        report["entries"].is_array(),
        "doctor --context-json must emit a source map\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn setup_status_leaves_a_sealed_home_untouched() {
    let output = run_sealed_diagnostic(["setup", "--status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Codewhale Status"), "stdout:\n{stdout}");
}

#[test]
fn diagnostics_read_home_legacy_settings_without_migrating_them() {
    for args in [
        &["doctor"][..],
        &["doctor", "--json"][..],
        &["setup", "--status"][..],
    ] {
        let fixture = TempDir::new().expect("fixture root");
        let workspace = fixture.path().join("workspace");
        let home = fixture.path().join("home");
        let legacy = home.join(".deepseek").join("settings.toml");
        let primary_home = home.join(".codewhale");
        let legacy_bytes = b"default_mode = \"plan\"\nprefer_external_pdftotext = true\n";
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy directory");
        fs::write(&legacy, legacy_bytes).expect("legacy settings");

        let output = diagnostic_command(&workspace, &home)
            .args(args)
            .output()
            .expect("run diagnostic against legacy settings");
        assert!(
            output.status.success(),
            "diagnostic {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        match args {
            ["doctor"] => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    stdout.contains("default_mode=plan (settings)"),
                    "doctor must report the legacy default mode\nstdout:\n{stdout}"
                );
                assert!(
                    !stdout.contains("prefer_external_pdftotext"),
                    "doctor must not advertise the removed PDF preference\nstdout:\n{stdout}"
                );
            }
            ["doctor", "--json"] => {
                let report: serde_json::Value =
                    serde_json::from_slice(&output.stdout).expect("machine-readable doctor report");
                assert_eq!(
                    report["setup"]["runtime_posture"]["default_mode"]["value"],
                    "plan"
                );
                assert_eq!(
                    report["setup"]["runtime_posture"]["default_mode"]["source"],
                    "settings"
                );
            }
            ["setup", "--status"] => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    stdout.contains("default_mode: plan (settings)"),
                    "setup status must report the legacy default mode\nstdout:\n{stdout}"
                );
            }
            _ => unreachable!("fixed diagnostic command list"),
        }

        assert_eq!(
            fs::read(&legacy).expect("legacy settings after diagnostic"),
            legacy_bytes,
            "diagnostic {args:?} must not rewrite legacy settings"
        );
        assert!(
            !primary_home.exists(),
            "diagnostic {args:?} must not create a primary Codewhale home"
        );
    }
}

#[test]
fn doctor_json_does_not_inherit_an_ambient_legacy_secret_from_an_explicit_home() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let legacy = home.join(".deepseek").join("secrets").join("secrets.json");
    FileKeyringStore::new(&legacy)
        .set("deepseek", "synthetic-ambient-legacy-value")
        .expect("seed ambient legacy secret");
    let legacy_before = fs::read(&legacy).expect("read legacy secret before doctor");

    let mut command = Command::new(crate::binary::codewhale());
    command
        .current_dir(&workspace)
        .args(["doctor", "--json"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEWHALE_HOME", &codewhale_home)
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .env(
            "CODEWHALE_RELEASE_BASE_URL",
            "https://example.invalid/releases",
        )
        .env("DEEPSEEK_TUI_VERSION", env!("CARGO_PKG_VERSION"));
    preserve_host_rustup_home(&mut command);
    preserve_host_platform_runtime(&mut command);

    let output = command.output().expect("run isolated doctor --json");
    assert!(
        output.status.success(),
        "isolated doctor --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable doctor report");
    assert_eq!(
        report["api_key"]["source"], "secret_store_unprobed",
        "doctor must report only structural eligibility, not an ambient legacy secret from outside an explicit home"
    );
    assert_eq!(report["api_key"]["availability"], "not_probed");
    assert_eq!(
        fs::read(&legacy).expect("read legacy secret after doctor"),
        legacy_before,
        "doctor must not rewrite the ambient legacy secret"
    );
    assert!(
        !codewhale_home.exists(),
        "doctor must not create an isolated Codewhale home or secret store"
    );
}

#[test]
fn doctor_text_probe_uses_a_legacy_key_without_migrating_it() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    let legacy = home.join(".deepseek").join("secrets").join("secrets.json");
    let primary = home.join(".codewhale").join("secrets").join("secrets.json");
    FileKeyringStore::new(&legacy)
        .set("deepseek", "diagnostic-legacy-key")
        .expect("seed legacy secret");
    let legacy_before = fs::read(&legacy).expect("read legacy secret before doctor");
    let server = CompletionServer::start();
    let base_url = server.base_url();
    let config = workspace.join("doctor.toml");
    fs::write(
        &config,
        format!(
            "provider = \"deepseek\"\n[providers.deepseek]\nbase_url = \"{base_url}\"\nmodel = \"deepseek-chat\"\nauth_mode = \"api_key\"\n"
        ),
    )
    .expect("write doctor config");

    let output = diagnostic_command(&workspace, &home)
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--probe-local",
        ])
        .output()
        .expect("run doctor probe");
    assert!(
        output.status.success(),
        "doctor probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("API connection successful"),
        "stdout:\n{stdout}"
    );
    // The probe runs before the verdict, and the verdict reports its result.
    let verdict = stdout
        .find("Ready: the live deepseek API check passed")
        .unwrap_or_else(|| panic!("verdict must use the probe result\nstdout:\n{stdout}"));
    let connectivity = stdout
        .find("API Connectivity")
        .expect("connectivity section");
    assert!(verdict < connectivity, "stdout:\n{stdout}");
    let requests = server.received_requests();
    assert_eq!(
        requests.len(),
        1,
        "doctor must make one local probe request"
    );
    let authorization = requests[0]
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        authorization,
        Some("Bearer diagnostic-legacy-key"),
        "doctor probe must use the legacy credential without printing it"
    );
    assert!(
        !primary.exists(),
        "doctor's text connectivity probe must not create a migrated primary secret store"
    );
    assert_eq!(
        fs::read(&legacy).expect("read legacy secret after doctor"),
        legacy_before,
        "doctor must not rewrite the legacy secret store"
    );

    fs::write(&config, "").expect("write unconfigured route");
    let output = diagnostic_command(&workspace, &home)
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--probe-local",
        ])
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_DISABLE_CLOUD_FACTS", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run doctor with unconfigured route");
    assert!(output.status.success(), "unconfigured doctor probe failed");
    assert!(
        !primary.exists(),
        "credential inspection must not migrate the store"
    );
    assert_eq!(fs::read(&legacy).expect("legacy preserved"), legacy_before);
}

#[test]
fn doctor_json_reports_a_legacy_store_without_reading_or_migrating_it() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    let legacy = home.join(".deepseek").join("secrets").join("secrets.json");
    let primary = home.join(".codewhale").join("secrets").join("secrets.json");
    FileKeyringStore::new(&legacy)
        .set("xiaomi-mimo", "tp-diagnostic-legacy-key")
        .expect("seed legacy Xiaomi secret");
    let legacy_before = fs::read(&legacy).expect("read legacy secret before doctor");
    let config = workspace.join("doctor.toml");
    fs::write(
        &config,
        "provider = \"xiaomi-mimo\"\n[providers.xiaomi_mimo]\nmode = \"standard\"\n",
    )
    .expect("write doctor config");

    let output = diagnostic_command(&workspace, &home)
        .args([
            "--config",
            config.to_str().expect("config path"),
            "doctor",
            "--json",
        ])
        .output()
        .expect("run doctor json");
    assert!(
        output.status.success(),
        "doctor --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable doctor report");
    assert_eq!(report["api_key"]["source"], "secret_store_unprobed");
    assert_eq!(report["api_key"]["availability"], "not_probed");
    assert_eq!(
        report["route"]["auth"]["scheme"], "unknown",
        "ordinary JSON doctor must not read the legacy key prefix to refine the Xiaomi scheme"
    );
    assert_eq!(report["route"]["auth"]["source"], "secret_store_unprobed");
    assert_eq!(report["route"]["auth"]["availability"], "not_probed");
    assert!(
        !primary.exists(),
        "doctor --json must not migrate a legacy secret while classifying auth"
    );
    assert_eq!(
        fs::read(&legacy).expect("read legacy secret after doctor"),
        legacy_before,
        "doctor --json must not rewrite the legacy secret store"
    );
}

#[test]
fn setup_status_reports_a_legacy_store_without_reading_or_migrating_it() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    fs::create_dir_all(&workspace).expect("workspace");
    let legacy = home.join(".deepseek").join("secrets").join("secrets.json");
    let primary = home.join(".codewhale").join("secrets").join("secrets.json");
    FileKeyringStore::new(&legacy)
        .set("deepseek", "setup-status-legacy-key")
        .expect("seed legacy secret");
    let legacy_before = fs::read(&legacy).expect("read legacy secret before setup");

    let output = diagnostic_command(&workspace, &home)
        .args(["setup", "--status"])
        .output()
        .expect("run setup status");
    assert!(
        output.status.success(),
        "setup --status failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("api_key: secret store eligible (store not probed)"),
        "stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("credential availability: not_probed"),
        "stdout:\n{stdout}"
    );
    assert!(
        !primary.exists(),
        "setup --status must not create a migrated primary secret store"
    );
    assert_eq!(
        fs::read(&legacy).expect("read legacy secret after setup"),
        legacy_before,
        "setup --status must not rewrite the legacy secret store"
    );
}

#[test]
fn doctor_json_stash_honors_an_explicit_codewhale_home() {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("isolated-codewhale-home");
    fs::create_dir_all(&workspace).expect("workspace");
    let ambient_stash = home.join(".codewhale").join("composer_stash.jsonl");
    fs::create_dir_all(ambient_stash.parent().expect("ambient stash parent"))
        .expect("ambient stash parent");
    fs::write(
        &ambient_stash,
        r#"{"text":"ambient draft must not be inspected"}"#,
    )
    .expect("ambient stash");
    let ambient_before = fs::read(&ambient_stash).expect("read ambient stash before doctor");

    let mut command = diagnostic_command(&workspace, &home);
    command
        .args(["doctor", "--json"])
        .env("CODEWHALE_HOME", &codewhale_home);
    let output = command.output().expect("run isolated doctor json");
    assert!(
        output.status.success(),
        "isolated doctor --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine-readable doctor report");
    assert_eq!(
        report["storage"]["stash"]["path"],
        codewhale_home
            .join("composer_stash.jsonl")
            .display()
            .to_string()
    );
    assert_eq!(report["storage"]["stash"]["present"], false);
    assert_eq!(report["storage"]["stash"]["count"], 0);
    assert!(report["storage"]["stash"]["error"].is_null());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("ambient draft must not be inspected"),
        "doctor must not inspect an ambient stash outside explicit CODEWHALE_HOME"
    );
    assert_eq!(
        fs::read(&ambient_stash).expect("read ambient stash after doctor"),
        ambient_before,
        "doctor must not rewrite the ambient stash"
    );
    assert!(
        !codewhale_home.exists(),
        "a diagnostic must not create an explicit stash home"
    );
}

fn run_sealed_diagnostic<const N: usize>(args: [&str; N]) -> Output {
    let fixture = TempDir::new().expect("fixture root");
    let workspace = fixture.path().join("workspace");
    let sealed_home = fixture.path().join("sealed-home");
    let codewhale_home = fixture.path().join("sealed-codewhale-home");
    std::fs::create_dir_all(&workspace).expect("workspace");

    let mut command = Command::new(crate::binary::codewhale());
    command
        .current_dir(&workspace)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", &sealed_home)
        .env("USERPROFILE", &sealed_home)
        .env("CODEWHALE_HOME", &codewhale_home)
        .env("CODEWHALE_SECRET_BACKEND", "file")
        // Keep the text doctor command offline: the release crate treats this
        // as a pinned mirror version and does not issue a metadata request.
        .env(
            "CODEWHALE_RELEASE_BASE_URL",
            "https://example.invalid/releases",
        )
        .env("DEEPSEEK_TUI_VERSION", env!("CARGO_PKG_VERSION"));
    preserve_host_rustup_home(&mut command);
    preserve_host_platform_runtime(&mut command);

    let output = command.output().expect("run sealed diagnostic");
    assert!(
        output.status.success(),
        "diagnostic {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !sealed_home.exists(),
        "diagnostic {args:?} must not create a HOME tree at {}",
        sealed_home.display()
    );
    assert!(
        !codewhale_home.exists(),
        "diagnostic {args:?} must not create CODEWHALE_HOME or a secrets store at {}",
        codewhale_home.display()
    );
    output
}

fn diagnostic_command(workspace: &std::path::Path, home: &std::path::Path) -> Command {
    let mut command = Command::new(crate::binary::codewhale());
    command
        .current_dir(workspace)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEWHALE_SECRET_BACKEND", "file")
        .env(
            "CODEWHALE_RELEASE_BASE_URL",
            "https://example.invalid/releases",
        )
        .env("DEEPSEEK_TUI_VERSION", env!("CARGO_PKG_VERSION"));
    preserve_host_rustup_home(&mut command);
    preserve_host_platform_runtime(&mut command);
    command
}

struct CompletionServer {
    base_url: String,
    requests: Arc<Mutex<Vec<HeaderMap>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    owner: Option<thread::JoinHandle<()>>,
}

impl CompletionServer {
    fn start() -> Self {
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (shutdown, shutdown_receiver) = tokio::sync::oneshot::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = Arc::clone(&requests);
        let owner = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("local probe runtime");
            runtime.block_on(async move {
                let app = Router::new().route(
                    "/v1/chat/completions",
                    post(move |headers: HeaderMap, body: Bytes| {
                        let requests = Arc::clone(&server_requests);
                        async move {
                            // Extracting Bytes makes Axum drain the complete request body
                            // before replying. Preserve only headers for the credential
                            // assertion; the request payload itself is intentionally dropped.
                            drop(body);
                            requests
                                .lock()
                                .expect("local probe request lock")
                                .push(headers);
                            Json(serde_json::json!({
                                "id": "doctor",
                                "object": "chat.completion",
                                "created": 0,
                                "model": "deepseek-chat",
                                "choices": [{
                                    "index": 0,
                                    "message": {"role": "assistant", "content": "ok"},
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 1,
                                    "total_tokens": 2
                                }
                            }))
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await;
                let listener = listener.expect("bind local probe server");
                let address = listener.local_addr().expect("local probe address");
                ready_sender
                    .send(format!("http://{address}/v1"))
                    .expect("publish local probe address");
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_receiver.await;
                    })
                    .await
                    .expect("serve local probe request");
            });
        });
        let base_url = ready_receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("local probe server must start");
        Self {
            base_url,
            requests,
            shutdown: Some(shutdown),
            owner: Some(owner),
        }
    }

    fn base_url(&self) -> String {
        self.base_url.clone()
    }

    fn received_requests(&self) -> Vec<HeaderMap> {
        self.requests
            .lock()
            .expect("local probe request lock")
            .clone()
    }
}

impl Drop for CompletionServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(owner) = self.owner.take() {
            let result = owner.join();
            if !thread::panicking() {
                result.expect("stop local probe server");
            }
        }
    }
}

/// A rustup shim may initialize its own toolchain state below `$HOME` when
/// `doctor` asks `rustc --version`. Preserve an already-configured toolchain
/// root so this test isolates Codewhale's own state contract.
fn preserve_host_rustup_home(command: &mut Command) {
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".rustup"))
                .filter(|path| path.is_dir())
        });
    if let Some(rustup_home) = rustup_home {
        command.env("RUSTUP_HOME", rustup_home);
    }
}

/// `env_clear` is part of these tests' credential-isolation boundary, but a
/// Windows child still needs the non-secret OS root variables used to locate
/// platform networking components. Without them a reqwest client can fail its
/// loopback connection before the local fixture ever receives a request.
fn preserve_host_platform_runtime(_command: &mut Command) {
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            _command.env(name, value);
        }
    }
}

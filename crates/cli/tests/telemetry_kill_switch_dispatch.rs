//! The kill switch has to reach the in-process runtime that would emit.
//!
//! The single `codewhale` binary resolves dispatcher overrides, states the
//! telemetry floor in its environment, and then calls `codewhale_tui::run`.
//! That runtime re-resolves telemetry before it can arm. These tests drive the
//! real binary through the keyless `features list` command and use the local
//! dry-run sink as the end-to-end observable: an enabled positive control must
//! write session events, while a kill switch must create no telemetry state.

#![cfg(unix)]

use std::fs;
use std::process::Command;

use codewhale_config::{SetupState, TELEMETRY_NOTICE_VERSION};
use tempfile::TempDir;

/// `CODEWHALE_TELEMETRY=0` beats `--telemetry true`, end to end.
///
/// The positive control proves the runtime is enabled before the kill switch is
/// applied, so the zero-state assertion cannot pass vacuously.
#[test]
fn env_off_beats_cli_on_end_to_end() {
    // Positive control first: the flag reaches the in-process runtime and
    // arms its dry-run sink, so the assertion below is about the floor and not
    // about a command that never crossed the dispatch boundary.
    let on = dispatch_and_read_telemetry(None);
    let dry_run = on
        .dry_run
        .expect("current consent plus `--telemetry true` must write the dry-run sink");
    assert!(
        dry_run.contains("\"event\":\"session_start\"")
            && dry_run.contains("\"event\":\"session_end\""),
        "the real in-process runtime must record a complete session: {dry_run}"
    );

    let off = dispatch_and_read_telemetry(Some("0"));
    assert!(
        !off.telemetry_dir_exists && off.dry_run.is_none(),
        "`CODEWHALE_TELEMETRY=0` must beat `--telemetry true` before the runtime arms"
    );
}

/// A real endpoint must remain queued locally instead of becoming short-CLI
/// network latency. The next interactive session owns delivery.
#[test]
fn short_cli_exit_persists_without_network_delivery() {
    let evidence = dispatch_and_read_telemetry_with_endpoint(None, None, None, false);
    let pending = evidence
        .pending
        .expect("short CLI must seal its pending telemetry before process exit");
    assert!(
        pending.contains("\"event\":\"session_start\"")
            && pending.contains("\"event\":\"session_end\""),
        "the pending buffer must contain the complete short CLI session: {pending}"
    );
    assert!(
        evidence.dry_run.is_none(),
        "a configured endpoint must stay pending rather than use the dry-run sink"
    );
}

/// A value the resolver cannot parse resolves to off, rather than falling
/// through to the flag.
#[test]
fn an_unparseable_telemetry_env_value_keeps_the_in_process_runtime_off() {
    let evidence = dispatch_and_read_telemetry(Some("maybe"));
    assert!(
        !evidence.telemetry_dir_exists && evidence.dry_run.is_none(),
        "a typo in the kill switch must never arm the in-process runtime"
    );
}

/// An explicit durable enable uses the Settings transition to clear old no.
/// The optional versioned compatibility command still rejects old versions.
#[test]
fn config_set_true_reenables_a_historical_decline_through_settings() {
    let fixture = TempDir::new().expect("fixture root");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("codewhale-home");
    let workspace = fixture.path().join("workspace");
    for dir in [&home, &codewhale_home, &workspace] {
        fs::create_dir_all(dir).expect("create fixture dir");
    }

    let mut state = SetupState::default();
    state.record_telemetry_notice("1", false);
    let state_path = codewhale_home.join("setup_state.json");
    state
        .save_to(&state_path)
        .expect("write historical decline");

    let config_path = fixture.path().join("config.toml");
    fs::write(&config_path, "telemetry_endpoint = \"\"\n").expect("write config");

    let command = || {
        let mut command = Command::new(codewhale_binary());
        command
            .current_dir(&workspace)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("PATH"))
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("CODEWHALE_HOME", &codewhale_home)
            .env("CODEWHALE_SECRET_BACKEND", "file")
            .arg("--config")
            .arg(&config_path);
        command
    };
    let before = fs::read(&state_path).unwrap();
    let declined = command()
        .env("CODEWHALE_TELEMETRY", "true")
        .args(["--telemetry", "true", "features", "list"])
        .output()
        .expect("run with a historical sidecar-only decline");
    assert!(declined.status.success());
    assert!(!codewhale_home.join("telemetry").exists());
    assert_eq!(
        fs::read(&state_path).unwrap(),
        before,
        "default-on and run-scoped on must preserve the old explicit no"
    );
    let output = command()
        .args(["config", "set", "telemetry", "true"])
        .output()
        .expect("run config set");
    assert!(
        output.status.success(),
        "config set failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&config_path)
            .expect("read config")
            .contains("telemetry = true")
    );
    let state = SetupState::load_from(&state_path).expect("read setup state");
    assert!(state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_opted_out());

    let outdated = command()
        .args(["config", "telemetry", "--accept-notice", "3"])
        .output()
        .expect("attempt outdated consent");
    assert!(!outdated.status.success());
    assert!(
        !SetupState::load_from(&state_path)
            .expect("read enabled state")
            .telemetry_opted_out()
    );

    let accepted = command()
        .args([
            "config",
            "telemetry",
            "--accept-notice",
            TELEMETRY_NOTICE_VERSION,
        ])
        .output()
        .expect("accept current notice");
    assert!(
        accepted.status.success(),
        "current notice acceptance failed: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let state = SetupState::load_from(&state_path).expect("read accepted state");
    assert!(state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
    assert!(!state.telemetry_opted_out());
}

#[test]
fn missing_preference_defaults_on_without_inventing_acceptance() {
    for version in [None, Some("1"), Some("4")] {
        let evidence = dispatch_and_read_telemetry_with_endpoint(None, Some(""), version, false);
        let batch: serde_json::Value = serde_json::from_str(
            evidence
                .dry_run
                .as_ref()
                .expect("default-on writes dry run")
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(batch["schema_version"], 4);
        assert_eq!(batch["notice_version"], 6);
        assert!(batch.get("consent_version").is_none());
        let state = evidence.setup.expect("disclosure display marker");
        assert_eq!(
            state.telemetry_notice_shown_for.as_deref(),
            Some(TELEMETRY_NOTICE_VERSION)
        );
        assert_eq!(state.telemetry_notice_decided_for.as_deref(), version);
        assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
        assert!(evidence.stderr.contains("on by default"));
        assert!(evidence.stderr.contains("Codewhale and PostHog"));
        assert!(
            evidence
                .stderr
                .contains("codewhale config set telemetry false")
        );
    }
}

struct DispatchEvidence {
    telemetry_dir_exists: bool,
    dry_run: Option<String>,
    pending: Option<String>,
    setup: Option<SetupState>,
    stderr: String,
}

/// Run the real dispatcher into a keyless in-process command and report the
/// telemetry state it actually left behind.
fn dispatch_and_read_telemetry(telemetry_env: Option<&str>) -> DispatchEvidence {
    dispatch_and_read_telemetry_with_endpoint(
        telemetry_env,
        Some(""),
        Some(TELEMETRY_NOTICE_VERSION),
        true,
    )
}

fn dispatch_and_read_telemetry_with_endpoint(
    telemetry_env: Option<&str>,
    endpoint: Option<&str>,
    consent_version: Option<&str>,
    cli_on: bool,
) -> DispatchEvidence {
    let fixture = TempDir::new().expect("fixture root");
    let home = fixture.path().join("home");
    let codewhale_home = fixture.path().join("codewhale-home");
    let workspace = fixture.path().join("workspace");
    for dir in [&home, &codewhale_home, &workspace] {
        fs::create_dir_all(dir).expect("create fixture dir");
    }

    if let Some(version) = consent_version {
        let mut state = SetupState::default();
        state.record_telemetry_notice(version, true);
        state
            .save_to(&codewhale_home.join("setup_state.json"))
            .expect("write fixture consent");
    }

    let config_path = fixture.path().join("config.toml");
    let mut config = String::new();
    if let Some(endpoint) = endpoint {
        config.push_str(&format!("telemetry_endpoint = {endpoint:?}\n"));
    }
    fs::write(&config_path, config).expect("write config");

    let mut command = Command::new(codewhale_binary());
    command
        .current_dir(&workspace)
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
        .arg("--config")
        .arg(&config_path);
    if cli_on {
        command.args(["--telemetry", "true"]);
    }
    command.args(["features", "list"]);
    if let Some(value) = telemetry_env {
        command.env("CODEWHALE_TELEMETRY", value);
    }
    let output = command.output().expect("run codewhale dispatcher");
    assert!(
        output.status.success(),
        "the in-process feature command must succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("feature\tstage\tenabled"),
        "the real in-process feature command must have run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let telemetry_dir = codewhale_home.join("telemetry");
    let dry_run = match fs::read_to_string(telemetry_dir.join("dryrun.jsonl")) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read telemetry dry-run sink: {error}"),
    };
    let pending = match fs::read_to_string(telemetry_dir.join("buffer.jsonl")) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read telemetry pending buffer: {error}"),
    };
    DispatchEvidence {
        telemetry_dir_exists: telemetry_dir.exists(),
        dry_run,
        pending,
        setup: SetupState::load_from(&codewhale_home.join("setup_state.json")),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn codewhale_binary() -> &'static str {
    env!("CARGO_BIN_EXE_codewhale")
}

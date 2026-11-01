//! Process-level telemetry contract.
//!
//! Everything here drives the real `codewhale` binary inside a sealed
//! `HOME`/`CODEWHALE_HOME`, with a loopback recorder standing in for the
//! telemetry endpoint. The unit tests in `codewhale-telemetry` prove the
//! predicate; these prove that the *emitting process* consults it — which is
//! the thing v1 of this design got wrong, because `resolve_runtime_options`
//! had no non-test caller and so neither `telemetry = false` in the config file
//! nor `CODEWHALE_TELEMETRY=0` was ever read by a process that would have sent.
//!
//! Two disciplines make the zero-request assertions non-vacuous:
//!
//! 1. Short CLI tests require complete local sessions after an acknowledged
//!    flush, or an explicit bounded-writer timeout. Disabled runs create no
//!    telemetry state. Short commands do not wait for network delivery.
//! 2. Full `exec` tests prove the buffered events reach a live recorder while
//!    respecting the same persistent and run-scoped opt-outs.
//!
//! The recorder is `http://127.0.0.1:<port>` — loopback, which is the one place
//! `validate_endpoint` permits plaintext, and a packet that never leaves the
//! machine.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use codewhale_config::{SetupState, TELEMETRY_NOTICE_VERSION};
use futures_util::FutureExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use wait_timeout::ChildExt;
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Where the recorder listens for batches.
const TELEMETRY_PATH: &str = "/v1/telemetry";
/// Where the mock model listens.
const MODEL_PATH: &str = "/v1/chat/completions";
const TEST_MODEL: &str = "telemetry-contract-model";

/// Sentinels planted through real inputs. None of these may appear in a batch.
///
/// Deliberately low-entropy: `crates/engine/src/fleet/ledger.rs` notes that
/// realistic-looking tokens trip secret scanning at push time.
const SENTINEL_PROMPT: &str = "tc-prompt-sentinel-do-not-collect";
const SENTINEL_FILENAME: &str = "tc-workspace-sentinel-file.txt";
const SENTINEL_PROVIDER_TABLE: &str = "tc_custom_provider_sentinel";
const SENTINEL_MCP_SERVER: &str = "tc-mcp-server-sentinel";
const SENTINEL_API_KEY: &str = "tc-api-key-sentinel-not-a-real-key";
/// Planted by writing to `buffer.jsonl` directly, which is what any other
/// process running as this user can do.
const SENTINEL_INJECTED: &str = "tc-injected-sentinel-/Users/victim/secret-repo";
/// The key lives only in the child's environment, never in a file, so the
/// "absent from every written file" assertion means something.
const SENTINEL_API_KEY_ENV: &str = "TC_SENTINEL_API_KEY";

const EXEC_TIMEOUT: Duration = Duration::from_secs(90);

// ── Fixture ──────────────────────────────────────────────────────────────

struct Fixture {
    // The consolidated integration target runs tests in parallel. Each test in
    // this module launches the full Codewhale binary, and low-resource CI
    // runners can fail those children before they reach either loopback server.
    // These tests exercise telemetry contracts, not launch concurrency, so one
    // process fixture at a time keeps their non-vacuity assertions meaningful.
    _process_test_guard: MutexGuard<'static, ()>,
    _root: TempDir,
    home: PathBuf,
    codewhale_home: PathBuf,
    workspace: PathBuf,
    config_path: PathBuf,
    endpoint: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let process_test_guard = telemetry_process_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = TempDir::new().expect("fixture root");
        let home = root.path().join("home");
        let codewhale_home = root.path().join("codewhale-home");
        let workspace = root.path().join("workspace");
        for dir in [&home, &codewhale_home, &workspace] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        let config_path = root.path().join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        Self {
            _process_test_guard: process_test_guard,
            _root: root,
            home,
            codewhale_home,
            workspace,
            config_path,
            endpoint: None,
        }
    }

    /// Point this fixture at a loopback recorder.
    fn with_endpoint(mut self, base_url: &str) -> Self {
        self.endpoint = Some(format!("{base_url}{TELEMETRY_PATH}"));
        self
    }

    fn write_config(&self, body: &str) {
        std::fs::write(&self.config_path, body).expect("write config");
    }

    /// Record the answer a user would have given on a TTY.
    ///
    /// Explicit preferences are machine-scoped and preserve historical no
    /// across TTY and headless launches. Presentation records no preference.
    fn record_notice(&self, opt_in: bool) {
        let mut state = SetupState::default();
        state.record_telemetry_notice(TELEMETRY_NOTICE_VERSION, opt_in);
        state
            .save_to(&self.codewhale_home.join("setup_state.json"))
            .expect("write setup state");
    }

    fn setup_state_path(&self) -> PathBuf {
        self.codewhale_home.join("setup_state.json")
    }

    fn telemetry_root(&self) -> PathBuf {
        self.codewhale_home.join("telemetry")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(crate::binary::codewhale());
        command
            .current_dir(&self.workspace)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("PATH"))
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("CODEWHALE_HOME", &self.codewhale_home)
            .env("CODEWHALE_SECRET_BACKEND", "file")
            .env("CODEWHALE_MEMORY", "false")
            // A pinned mirror version keeps the release crate from issuing a
            // metadata request, so the only egress a test can observe is the
            // one this file is about.
            .env(
                "CODEWHALE_RELEASE_BASE_URL",
                "https://example.invalid/releases",
            )
            .env("DEEPSEEK_TUI_VERSION", env!("CARGO_PKG_VERSION"))
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        if let Some(endpoint) = &self.endpoint {
            command.env("CODEWHALE_TELEMETRY_ENDPOINT", endpoint);
        }
        command
    }

    /// The cheapest subcommand that still traverses the whole telemetry
    /// lifecycle: arm, `session_start`, dispatch, `session_end`, bounded local
    /// persistence. Verbose logging exposes the actual persistence outcome.
    /// The read-only `config telemetry` query traverses the shared command
    /// lifecycle without provider, MCP, or model execution. Structural Doctor
    /// deliberately has no telemetry lifecycle or persistence side effects.
    fn run_short_command(&self) -> Output {
        let mut command = self.command();
        command.args([
            "--verbose",
            "--config",
            self.config_path.to_str().expect("config path"),
            "config",
            "telemetry",
        ]);
        let output = command.output().expect("run codewhale config telemetry");
        assert!(
            output.status.success(),
            "config telemetry failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// Every regular file under the sealed roots, for leak scanning.
    fn written_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for base in [&self.home, &self.codewhale_home, &self.workspace] {
            collect_files(base, &mut out);
        }
        out.push(self.config_path.clone());
        out
    }
}

fn telemetry_process_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Mutex::default)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect_files(&path, out),
            Ok(kind) if kind.is_file() => out.push(path),
            _ => {}
        }
    }
}

// ── Recorder ─────────────────────────────────────────────────────────────

/// A loopback endpoint that accepts every batch and keeps the body.
///
/// The body is the point. The recorder `crates/tui/tests/diagnostic_read_only.rs`
/// copies deliberately drops it; a telemetry contract that cannot read what was
/// sent can only assert "something happened".
async fn start_recorder() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher(TELEMETRY_PATH))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// Every request the recorder saw, batch bodies included.
async fn recorded_batches(server: &MockServer) -> Vec<Value> {
    let requests = server
        .received_requests()
        .await
        .expect("the recorder must retain its request log");
    requests
        .iter()
        .filter(|request| request.url.path() == TELEMETRY_PATH)
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap_or_else(|error| {
                panic!(
                    "a telemetry batch must be JSON: {error}\nbody: {}",
                    String::from_utf8_lossy(&request.body)
                )
            })
        })
        .collect()
}

/// How many chat completions the mock model served.
///
/// The sentinel test's whole claim is that a prompt which *did* reach a model
/// did not reach a batch, so a run where the turn never happened would be
/// vacuous.
async fn model_request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("the recorder must retain its request log")
        .iter()
        .filter(|request| request.url.path() == MODEL_PATH)
        .count()
}

async fn assert_no_batches(server: &MockServer, why: &str) {
    let batches = recorded_batches(server).await;
    assert!(
        batches.is_empty(),
        "{why}: expected zero telemetry requests, recorded {}:\n{}",
        batches.len(),
        serde_json::to_string_pretty(&batches).unwrap_or_default()
    );
}

fn buffered_events(fixture: &Fixture) -> Vec<Value> {
    let path = fixture.telemetry_root().join("buffer.jsonl");
    let body = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "read locally buffered telemetry at {}: {error}",
            path.display()
        )
    });
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
                panic!("buffered telemetry must be JSON: {error}\nline: {line}")
            })
        })
        .collect()
}

async fn assert_short_cli_persistence_without_network(
    fixture: &Fixture,
    server: &MockServer,
    output: &Output,
    why: &str,
) {
    assert_no_batches(server, why).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let outcomes: Vec<_> = stderr
        .lines()
        .filter_map(|line| {
            line.split_once("telemetry local persistence outcome=")
                .map(|(_, outcome)| outcome)
        })
        .collect();
    assert_eq!(
        outcomes.len(),
        1,
        "{why}: require exactly one persistence outcome, not an inference from exit time: {stderr}"
    );
    match outcomes[0] {
        // The detached writer may still be queued when the bounded CLI exits.
        // Only the real timeout receipt excuses an incomplete local session.
        "TimedOut" => return,
        "Buffered" => {}
        outcome => panic!("{why}: unexpected local persistence outcome: {outcome}"),
    }
    let events = buffered_events(fixture);
    assert!(
        events.iter().any(|event| event["event"] == "session_start"),
        "{why}: the local buffer must carry the session it describes: {events:?}"
    );
    assert!(
        events.iter().any(|event| event["event"] == "session_end"),
        "{why}: local persistence must carry session_end: {events:?}"
    );
}

// ── Short CLI persistence is bounded and observable ──────────────────────

/// An acknowledged short CLI flush must preserve a complete session. A slow
/// local writer must report its deadline without waiting for the endpoint.
#[tokio::test(flavor = "current_thread")]
async fn current_explicit_consent_buffers_one_complete_session_without_network() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());

    fixture.write_config("telemetry = true\n");
    fixture.record_notice(true);
    let output = fixture.run_short_command();

    assert_short_cli_persistence_without_network(
        &fixture,
        &server,
        &output,
        "current explicit consent",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn default_on_reports_local_persistence_without_network() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());

    let output = fixture.run_short_command();

    assert_short_cli_persistence_without_network(
        &fixture,
        &server,
        &output,
        "the documented default",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_short_cli_sessions_require_an_explicit_timeout() {
    use std::os::unix::process::ExitStatusExt;

    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    std::fs::create_dir_all(fixture.telemetry_root()).expect("create local buffer root");
    std::fs::write(
        fixture.telemetry_root().join("buffer.jsonl"),
        "{\"event\":\"session_start\",\"source\":\"unknown\"}\n",
    )
    .expect("write incomplete session");

    // A successful process exit, an acknowledged but incomplete append, or an
    // unrecognized outcome must not be relabelled as an allowed deadline.
    for diagnostic in [
        "",
        "info telemetry local persistence outcome=Buffered\n",
        "info telemetry local persistence outcome=Dropped\n",
        "info telemetry local persistence outcome=TimedOutLater\n",
        "info telemetry local persistence outcome=TimedOut\ninfo telemetry local persistence outcome=Buffered\n",
    ] {
        let output = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: diagnostic.as_bytes().to_vec(),
        };
        let rejected = std::panic::AssertUnwindSafe(assert_short_cli_persistence_without_network(
            &fixture,
            &server,
            &output,
            "unexplained missing session_end",
        ))
        .catch_unwind()
        .await;
        assert!(
            rejected.is_err(),
            "accepted invalid receipt: {diagnostic:?}"
        );
    }

    let timed_out = Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: Vec::new(),
        stderr: b"info telemetry local persistence outcome=TimedOut\n".to_vec(),
    };
    assert_short_cli_persistence_without_network(
        &fixture,
        &server,
        &timed_out,
        "explicit bounded writer timeout",
    )
    .await;
}

// ── Off is real ──────────────────────────────────────────────────────────

/// The only test that proves the emitting process reads the config *file*.
///
/// No environment variable is set here on purpose. `CODEWHALE_TELEMETRY=0` and
/// the config key travel different paths, and v1 of this design shipped a
/// kill switch that only the env half ever reached.
#[tokio::test(flavor = "current_thread")]
async fn config_file_only_opt_out_sends_zero_requests() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = false\n");
    fixture.record_notice(true);

    let output = fixture.run_short_command();
    assert!(output.status.success());

    assert_no_batches(&server, "`telemetry = false` in the config file").await;
    assert!(
        !fixture.telemetry_root().exists(),
        "a fresh config-file opt-out must create no telemetry state"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn telemetry_disabled_by_env_sends_zero_requests() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = true\n");
    fixture.record_notice(true);

    let mut command = fixture.command();
    command
        .env("CODEWHALE_TELEMETRY", "0")
        .args([
            "--config",
            fixture.config_path.to_str().expect("config path"),
            "doctor",
        ])
        .output()
        .expect("run codewhale doctor");

    assert_no_batches(&server, "`CODEWHALE_TELEMETRY=0`").await;
    assert!(
        !fixture.telemetry_root().exists(),
        "a fresh run-scoped opt-out must create no telemetry state"
    );
}

/// An unparseable env value fails **closed**, rather than falling through to
/// the config file's `true`.
#[tokio::test(flavor = "current_thread")]
async fn an_unparseable_telemetry_env_value_sends_zero_requests() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = true\n");
    fixture.record_notice(true);

    fixture
        .command()
        .env("CODEWHALE_TELEMETRY", "maybe")
        .args([
            "--config",
            fixture.config_path.to_str().expect("config path"),
            "doctor",
        ])
        .output()
        .expect("run codewhale doctor");

    assert_no_batches(&server, "`CODEWHALE_TELEMETRY=maybe`").await;
    assert!(
        !fixture.telemetry_root().exists(),
        "a fresh forced-off run must create no telemetry state"
    );
}

/// A fresh headless run defaults on and records presentation, not acceptance.
#[tokio::test(flavor = "current_thread")]
async fn telemetry_defaults_on_without_notice_buffers_a_complete_session() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    // Deliberately no `record_notice`.

    let output = fixture.run_short_command();

    assert_short_cli_persistence_without_network(&fixture, &server, &output, "default-on usage")
        .await;
    let state = SetupState::load_from(&fixture.setup_state_path()).expect("shown state");
    assert_eq!(
        state.telemetry_notice_shown_for.as_deref(),
        Some(TELEMETRY_NOTICE_VERSION)
    );
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
}

/// A previous acceptance remains enabled under the default-on policy.
#[tokio::test(flavor = "current_thread")]
async fn a_stale_accepted_notice_remains_on_without_synthesizing_current_acceptance() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = true\n");
    let mut state = SetupState::default();
    state.record_telemetry_notice("0", true);
    state
        .save_to(&fixture.setup_state_path())
        .expect("write setup state");

    let output = fixture.run_short_command();

    assert_short_cli_persistence_without_network(&fixture, &server, &output, "default-on usage")
        .await;
    let state = SetupState::load_from(&fixture.setup_state_path()).expect("shown state");
    assert_eq!(
        state.telemetry_notice_shown_for.as_deref(),
        Some(TELEMETRY_NOTICE_VERSION)
    );
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
}

// ── Nothing survives a disable ───────────────────────────────────────────

/// A human's "off" wipes: tombstone first, data truncated, lock file left in
/// place, identity removed.
#[tokio::test(flavor = "current_thread")]
async fn disabling_after_buffering_wipes_and_sends_nothing() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    let root = fixture.telemetry_root();
    seed_consenting_home(&root);
    fixture.write_config("telemetry = false\n");
    fixture.record_notice(true);

    fixture.run_short_command();

    assert_no_batches(&server, "an explicit opt-out with a populated buffer").await;
    assert!(
        root.join("disabled").exists(),
        "the tombstone is written first and never removed"
    );
    assert_eq!(
        std::fs::read(root.join("buffer.jsonl")).expect("buffer survives as an empty file"),
        Vec::<u8>::new(),
        "buffered events must be truncated, not sent"
    );
    assert!(
        root.join("buffer.jsonl.lock").exists(),
        "the lock file is never unlinked: replacing it would leave appenders \
         and compactors holding different inodes"
    );
    assert!(
        !root.join("install_id.json").exists(),
        "the install identity must not survive an opt-out"
    );
}

/// A non-persistent forced-off result must preserve an existing identity and
/// unflushed buffer.
#[tokio::test(flavor = "current_thread")]
async fn forced_off_run_preserves_a_consenting_users_state() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    let root = fixture.telemetry_root();
    seed_consenting_home(&root);
    let before = snapshot(&root);
    let mut command = fixture.command();
    command
        .env("CODEWHALE_TELEMETRY", "not-a-bool")
        .args([
            "--config",
            fixture.config_path.to_str().expect("config path"),
            "doctor",
        ])
        .output()
        .expect("run codewhale doctor");

    assert_no_batches(&server, "a forced-off run").await;
    assert_eq!(
        snapshot(&root),
        before,
        "a forced-off run must leave a consenting user's telemetry state byte-identical"
    );
}

/// The documented one-command kill switch stops collection and destroys
/// nothing.
///
/// `CODEWHALE_TELEMETRY=0` used to resolve as an *answer*, so it took the
/// destructive opt-out branch: an agent harness that set it for one command
/// deleted the install id and truncated the dry-run records of the person who
/// owns the machine, and the "permanent" tombstone it left was cleared by the
/// user's very next ordinary run. Off for the run, and only for the run.
#[tokio::test(flavor = "current_thread")]
async fn a_run_scoped_kill_switch_preserves_a_consenting_users_state() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    let root = fixture.telemetry_root();
    seed_consenting_home(&root);
    let before = snapshot(&root);
    fixture.write_config("telemetry = true\n");
    fixture.record_notice(true);

    for value in ["0", "off", "false"] {
        fixture
            .command()
            .env("CODEWHALE_TELEMETRY", value)
            .args([
                "--config",
                fixture.config_path.to_str().expect("config path"),
                "doctor",
            ])
            .output()
            .expect("run codewhale doctor");

        assert_no_batches(&server, "a run-scoped kill switch").await;
        assert!(
            !root.join("disabled").exists(),
            "`CODEWHALE_TELEMETRY={value}` tombstoned a machine nobody opted out"
        );
        assert_eq!(
            snapshot(&root),
            before,
            "`CODEWHALE_TELEMETRY={value}` touched a consenting user's telemetry state"
        );
    }

    // And the persistent switch still is the destructive one, on the same
    // home, so the two are not merely both no-ops here.
    fixture.write_config("telemetry = false\n");
    fixture.run_short_command();
    assert!(
        root.join("disabled").exists(),
        "the config-file opt-out must still wipe and tombstone"
    );
    assert!(!root.join("install_id.json").exists());
}

/// A run that was never permitted to collect creates no directory at all —
/// which is also what makes the process panic hook, installed before the
/// command line is even parsed, write nothing for a disabled user.
#[tokio::test(flavor = "current_thread")]
async fn a_disabled_run_creates_no_telemetry_directory() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = false\n");
    fixture.record_notice(false);

    fixture.run_short_command();

    assert_no_batches(&server, "a declined run").await;
    assert!(
        !fixture.telemetry_root().exists(),
        "nothing may be created for a user who declined on a fresh home"
    );
}

// ── The notice is never answered by silence ──────────────────────────────

/// Deferral is not a decision. `--skip-onboarding` records and prints no notice
/// decision, while the non-interactive run still follows the documented
/// default — unlike the constitution checkpoint, which persists a `Deferred`
/// completion.
#[tokio::test(flavor = "current_thread")]
async fn skip_onboarding_writes_no_telemetry_decision() {
    let server = start_recorder().await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    fixture.write_config("telemetry = true\n");

    let mut command = fixture.command();
    let output = command
        .args([
            "--verbose",
            "--config",
            fixture.config_path.to_str().expect("config path"),
            "--skip-onboarding",
            "config",
            "telemetry",
        ])
        .output()
        .expect("run short telemetry config command");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for stream in [&stdout, &stderr] {
        assert!(
            !stream.contains("keep telemetry off"),
            "the notice must not be rendered on a path that cannot answer it"
        );
    }

    if let Some(state) = SetupState::load_from(&fixture.setup_state_path()) {
        assert_eq!(
            state.telemetry_notice_decided_for, None,
            "skip-onboarding must leave the telemetry decision unset"
        );
    }
    assert_short_cli_persistence_without_network(
        &fixture,
        &server,
        &output,
        "`--skip-onboarding` follows the default",
    )
    .await;
    let state = SetupState::load_from(&fixture.setup_state_path()).expect("shown state");
    assert_eq!(
        state.telemetry_notice_shown_for.as_deref(),
        Some(TELEMETRY_NOTICE_VERSION)
    );
    assert!(!state.telemetry_accepted(TELEMETRY_NOTICE_VERSION));
}

// ── Payload red lines, through a real turn ───────────────────────────────

/// Five sentinel classes planted through real inputs — the prompt, a workspace
/// filename, a custom `[providers.<name>]` table key, an MCP server name, and
/// the API key — asserted absent from every recorded batch.
///
/// The API key is held to the stricter standard the harness at
/// `crates/tui/tests/verifiers_harness_contract.rs` applies: absent from stdout,
/// stderr, and every file under the sealed roots as well. The other four
/// legitimately appear in files the product owns — a prompt is in the session
/// transcript, a provider table key is in the config the user wrote — so the
/// claim about them is precisely that they never reach a *batch*.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_contains_no_planted_sentinel() {
    let server = start_recorder().await;
    mount_model(&server, Duration::ZERO).await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    plant_sentinels(&fixture, &server.uri());

    let output = run_exec(&fixture, SENTINEL_PROMPT);
    assert_exec_succeeded(&output, "sentinel payload run");

    assert!(
        model_request_count(&server).await > 0,
        "the sentinel prompt must actually have reached a model, or this test \
         proves nothing\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let batches = recorded_batches(&server).await;
    assert!(
        !batches.is_empty(),
        "this test is only meaningful against a batch that was actually sent"
    );
    for batch in &batches {
        assert_eq!(batch["schema_version"], 4);
        assert_eq!(batch["notice_version"], 6);
        assert!(batch.get("consent_version").is_none());
    }
    let shown = SetupState::load_from(&fixture.setup_state_path()).expect("disclosure marker");
    assert_eq!(shown.telemetry_notice_decided_for, None);
    assert!(!shown.telemetry_opt_in);
    let serialized = serde_json::to_string(&batches).expect("serialize batches");
    for sentinel in [
        SENTINEL_PROMPT,
        SENTINEL_FILENAME,
        SENTINEL_PROVIDER_TABLE,
        SENTINEL_MCP_SERVER,
        SENTINEL_API_KEY,
    ] {
        assert!(
            !serialized.contains(sentinel),
            "sentinel `{sentinel}` reached a telemetry batch:\n{serialized}"
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(SENTINEL_API_KEY),
        "the API key leaked into stdout"
    );
    assert!(
        !stderr.contains(SENTINEL_API_KEY),
        "the API key leaked into stderr"
    );
    for file in fixture.written_files() {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        assert!(
            !String::from_utf8_lossy(&bytes).contains(SENTINEL_API_KEY),
            "the API key leaked into {}",
            file.display()
        );
    }
}

/// The buffer file is an **untrusted input**, and this is the test that says so.
///
/// Every bound in `codewhale-telemetry`'s schema is a property of how a payload
/// is *built*: closed enums, `u32`s, `ProviderKind::as_str()`,
/// `reduce_panic_site`. None of that survives the round trip, because `flush`
/// re-reads `buffer.jsonl` and deserializes it — and `$CODEWHALE_HOME` is a
/// predictable path that anything running as the user can append to. The
/// realistic writer is not an intruder: it is a `Bash` tool call this very
/// session made on the model's behalf, or an MCP server, or a hook. Without a
/// drain-path re-check, telemetry is a confused deputy that POSTs whatever that
/// writer chooses to the configured endpoint, under the user's install id, past
/// every egress control the user has on the provider route.
///
/// The injection happens **after** the session has armed, on purpose: `init`
/// truncates the buffer, so a pre-arming plant proves nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_buffer_line_never_reaches_a_batch() {
    let server = start_recorder().await;
    // A slow first token holds the session open long enough to append.
    mount_model(&server, Duration::from_secs(5)).await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    plant_sentinels(&fixture, &server.uri());

    let mut command = exec_command(&fixture, "hello");
    let mut child = command.spawn().expect("spawn codewhale exec");
    let stdout = read_in_background(child.stdout.take().expect("stdout pipe"));
    let stderr = read_in_background(child.stderr.take().expect("stderr pipe"));

    let buffer = fixture.telemetry_root().join("buffer.jsonl");
    wait_until(Duration::from_secs(30), || buffer.exists());
    append_lines(
        &buffer,
        &[
            // The path-bearing field, carrying a path it was never meant to.
            json!({"event": "panic", "site": SENTINEL_INJECTED}).to_string(),
            // The one event field read back from `state.json`.
            json!({"event": "install_or_upgrade", "kind": "upgrade",
                   "previous_version": SENTINEL_INJECTED})
            .to_string(),
            // The provider set, whose whole design is that it cannot carry a
            // customer's `[providers.<name>]` table key.
            json!({"event": "session_end", "duration_bucket": "lt_1m",
                   "exit_class": "clean", "cold_start_bucket": null,
                   "providers": [SENTINEL_INJECTED],
                   "counters": {"turns": 0, "tool_calls": 0, "fleet_dispatch": 0,
                                "workflow_run": 0, "subagent_spawn": 0,
                                "mcp_server_connected": 0, "memory_search": 0,
                                "approval_modal_shown": 0, "approval_auto_allowed": 0,
                                "command_palette_open": 0},
                   "errors": {"auth_preflight_failed": 0, "provider_http_4xx": 0,
                              "provider_http_5xx": 0, "tool_denied_by_policy": 0,
                              "tool_timeout": 0, "network_error": 0},
                   "turn_wall": {"lt_5s": 0, "5_30s": 0, "30_120s": 0, "gte_120s": 0}})
            .to_string(),
        ],
    );

    let status = child
        .wait_timeout(EXEC_TIMEOUT)
        .expect("wait for codewhale exec")
        .expect("codewhale exec must exit");
    let output = Output {
        status,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
    };
    assert_exec_succeeded(&output, "hostile-buffer payload run");

    let batches = recorded_batches(&server).await;
    assert!(
        !batches.is_empty(),
        "this test is only meaningful against a batch that was actually sent"
    );
    let serialized = serde_json::to_string(&batches).expect("serialize batches");
    assert!(
        !serialized.contains(SENTINEL_INJECTED),
        "a line appended to buffer.jsonl was POSTed verbatim:\n{serialized}"
    );
}

/// Append raw lines to a sink, the way any other process on the machine would.
fn append_lines(path: &Path, lines: &[String]) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open the telemetry buffer");
    for line in lines {
        writeln!(file, "{line}").expect("append to the telemetry buffer");
    }
}

/// The documented mid-session opt-out — `codewhale config set telemetry false`,
/// written by another process — must be observed by a session that is already
/// running. The flush re-resolves from disk before it sends anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_session_opt_out_stops_the_shutdown_flush() {
    let server = start_recorder().await;
    // A slow first token gives the second writer a window while the session is
    // armed and buffering.
    mount_model(&server, Duration::from_secs(4)).await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    plant_sentinels(&fixture, &server.uri());

    let mut command = exec_command(&fixture, "hello");
    let mut child = command.spawn().expect("spawn codewhale exec");
    let stdout = read_in_background(child.stdout.take().expect("stdout pipe"));
    let stderr = read_in_background(child.stderr.take().expect("stderr pipe"));

    // Wait until this session's event is actually buffered, then leave enough
    // time for any accidental background flush to reach the recorder. Nothing
    // may be sent before shutdown.
    let buffer = fixture.telemetry_root().join("buffer.jsonl");
    wait_until(Duration::from_secs(30), || {
        std::fs::read_to_string(&buffer)
            .map(|body| body.contains("\"event\":\"session_start\""))
            .unwrap_or(false)
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_no_batches(&server, "before the shutdown flush").await;

    // Now take the documented way out from outside the process. The only
    // flush, at shutdown, must re-resolve this write and suppress the batch.
    fixture.write_config(&sentinel_config(&server.uri(), false));

    let status = child
        .wait_timeout(EXEC_TIMEOUT)
        .expect("wait for codewhale exec")
        .expect("codewhale exec must exit");
    let output = Output {
        status,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
    };
    assert_exec_succeeded(&output, "mid-session opt-out run");

    assert_no_batches(&server, "an opt-out written mid-session").await;
    let root = fixture.telemetry_root();
    assert!(
        root.join("disabled").exists(),
        "the opt-out wipe must leave a tombstone the next run also honours"
    );
}

/// Ctrl-C must not wait on a lock a second Codewhale process is holding.
///
/// This is why appends never take the compaction lock: `flock` is per-fd within
/// a process, so a blocking acquisition on the signal path would hang exit for
/// as long as any other holder lives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_exits_while_a_second_process_holds_the_lock() {
    let server = start_recorder().await;
    mount_model(&server, Duration::from_secs(30)).await;
    let fixture = Fixture::new().with_endpoint(&server.uri());
    plant_sentinels(&fixture, &server.uri());

    let mut command = exec_command(&fixture, "hello");
    let mut child = command.spawn().expect("spawn codewhale exec");

    let root = fixture.telemetry_root();
    let lock_path = root.join("buffer.jsonl.lock");
    // Arming itself takes the lock, so the holder below must wait until the
    // session is armed — otherwise this test would pin a deadlock it created.
    wait_until(Duration::from_secs(30), || {
        root.join("buffer.jsonl").exists()
    });
    let _holder = LockHolder::take(&lock_path);

    // SIGINT to the child alone; the process group belongs to the test runner.
    let pid = child.id() as libc::pid_t;
    // SAFETY: `kill` with a pid this process spawned and has not reaped.
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let started = Instant::now();
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("wait for codewhale exec");
    let status = status.unwrap_or_else(|| {
        let _ = child.kill();
        panic!(
            "Ctrl-C blocked for {:?} while another process held the telemetry lock — \
             the signal path must append without taking it",
            started.elapsed()
        )
    });
    assert_eq!(
        status.code(),
        Some(130),
        "SIGINT must still exit 130 with the telemetry lock held elsewhere"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Ctrl-C took {:?} while the telemetry lock was held elsewhere",
        started.elapsed()
    );
}

/// Holds the telemetry compaction lock for the lifetime of the value.
struct LockHolder {
    file: std::fs::File,
}

impl LockHolder {
    fn take(path: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .expect("open the telemetry lock");
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let started = Instant::now();
        loop {
            // SAFETY: `fd` is owned by `file` and outlives the call.
            let taken = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if taken == 0 {
                break;
            }

            let err = std::io::Error::last_os_error();
            let retryable = err
                .raw_os_error()
                .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN);
            assert!(retryable, "failed to take the telemetry lock: {err}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the telemetry arming lock remained held for {:?}",
                started.elapsed()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { file }
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&self.file);
        // SAFETY: same fd, still owned by `self`.
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}

// ── exec harness ─────────────────────────────────────────────────────────

fn sse_chunk(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

fn text_sse(text: &str) -> String {
    [
        sse_chunk(json!({
            "id": "chatcmpl-tc",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        })),
        sse_chunk(json!({
            "id": "chatcmpl-tc",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 2, "total_tokens": 9}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

async fn mount_model(server: &MockServer, delay: Duration) {
    Mock::given(method("GET"))
        .and(path_matcher("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "object": "list",
                    "data": [{"id": TEST_MODEL, "object": "model"}]
                })),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path_matcher(MODEL_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("cache-control", "no-cache")
                .set_body_string(text_sse("acknowledged"))
                .set_delay(delay),
        )
        .mount(server)
        .await;
}

/// A config whose provider table key, MCP server name, and workspace file are
/// all sentinels, so a leak has somewhere to come from.
fn sentinel_config(base_url: &str, telemetry: bool) -> String {
    format!(
        "telemetry = {telemetry}\nprovider = \"{SENTINEL_PROVIDER_TABLE}\"\n\n\
         [providers.{SENTINEL_PROVIDER_TABLE}]\n\
         kind = \"openai-compatible\"\n\
         base_url = \"{base_url}/v1\"\n\
         model = \"{TEST_MODEL}\"\n\
         api_key_env = \"{SENTINEL_API_KEY_ENV}\"\n"
    )
}

fn plant_sentinels(fixture: &Fixture, base_url: &str) {
    let config = sentinel_config(base_url, true);
    fixture.write_config(
        config
            .strip_prefix("telemetry = true\n")
            .expect("fixture preference"),
    );
    // No saved preference or acceptance: the real exec path must use default-on.
    std::fs::write(
        fixture.workspace.join(SENTINEL_FILENAME),
        "sentinel workspace file\n",
    )
    .expect("plant workspace file");
    std::fs::write(
        fixture.codewhale_home.join("mcp.json"),
        // Keep the MCP name in a real parsed config without starting a process
        // that exits before CodeWhale can write its initialize request. The
        // telemetry contract is about name redaction, not broken-pipe handling.
        json!({"mcpServers": {SENTINEL_MCP_SERVER: {
            "command": "/bin/true",
            "args": [],
            "disabled": true
        }}})
        .to_string(),
    )
    .expect("plant MCP config");
}

fn exec_command(fixture: &Fixture, prompt: &str) -> Command {
    let mut command = fixture.command();
    command
        .env(
            "CODEWHALE_MCP_CONFIG",
            fixture.codewhale_home.join("mcp.json"),
        )
        .env(SENTINEL_API_KEY_ENV, SENTINEL_API_KEY)
        .args([
            "--config",
            fixture.config_path.to_str().expect("config path"),
            "--workspace",
            fixture.workspace.to_str().expect("workspace path"),
            "--no-project-config",
            "--skip-onboarding",
            "exec",
            "--auto",
            "--output-format",
            "stream-json",
            "--",
            prompt,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_exec(fixture: &Fixture, prompt: &str) -> Output {
    let mut command = exec_command(fixture, prompt);
    let mut child = command.spawn().expect("spawn codewhale exec");
    let stdout = read_in_background(child.stdout.take().expect("stdout pipe"));
    let stderr = read_in_background(child.stderr.take().expect("stderr pipe"));
    let status = match child.wait_timeout(EXEC_TIMEOUT).expect("wait for exec") {
        Some(status) => status,
        None => {
            let _ = child.kill();
            panic!("codewhale exec did not exit within {EXEC_TIMEOUT:?}");
        }
    };
    Output {
        status,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
    }
}

fn assert_exec_succeeded(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        buffer
    })
}

fn wait_until(limit: Duration, mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < limit {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("condition was not reached within {limit:?}");
}

// ── Seeded state ─────────────────────────────────────────────────────────

/// A home that already belongs to a consenting user: an identity, a populated
/// buffer, a flush record, and the lock file.
fn seed_consenting_home(root: &Path) {
    std::fs::create_dir_all(root).expect("create telemetry root");
    std::fs::write(
        root.join("install_id.json"),
        json!({
            "schema_version": 1,
            "install_id": "11111111-2222-3333-4444-555555555555",
            "rotated_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("seed install id");
    std::fs::write(
        root.join("state.json"),
        json!({"schema_version": 1, "last_version": "0.0.1"}).to_string(),
    )
    .expect("seed state");
    std::fs::write(
        root.join("buffer.jsonl"),
        format!(
            "{}\n",
            json!({"event": "session_start", "source": "unknown"})
        ),
    )
    .expect("seed buffer");
    std::fs::write(root.join("buffer.jsonl.lock"), b"").expect("seed lock file");
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    let mut out: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let bytes = std::fs::read(&path).unwrap_or_default();
            (name, bytes)
        })
        .collect();
    out.sort();
    out
}

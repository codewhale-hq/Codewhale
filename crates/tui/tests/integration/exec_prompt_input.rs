#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use wait_timeout::ChildExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_MODEL: &str = "prompt-input-model";
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

enum Stdin {
    Inherit,
    Bytes(&'static str),
    OpenPipe,
}

struct Outcome {
    success: bool,
    stdout: String,
    stderr: String,
    elapsed: Duration,
    dispatched: String,
}

fn answer_sse() -> String {
    let chunk = |value: Value| format!("data: {}\n\n", value);
    [
        chunk(json!({
            "id": "c1", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": null}]
        })),
        chunk(json!({
            "id": "c1", "object": "chat.completion.chunk", "model": TEST_MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

async fn start_mock_llm() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{ "id": TEST_MODEL, "object": "model" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(answer_sse()),
        )
        .mount(&server)
        .await;
    server
}

fn drain<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = reader.read_to_end(&mut out);
        out
    })
}

async fn run_exec(server: &MockServer, exec_args: &[&str], stdin: Stdin) -> Outcome {
    let workspace = TempDir::new().expect("workspace");
    let home = TempDir::new().expect("home");
    std::fs::create_dir_all(home.path().join(".codewhale")).expect("config dir");

    let mut command = Command::new(crate::binary::codewhale());
    command.env_clear();
    for key in ["PATH", "TMPDIR", "TERM", "LANG", "LC_ALL"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .current_dir(workspace.path())
        .arg("--workspace")
        .arg(workspace.path())
        .arg("--no-project-config")
        .arg("exec")
        .args(exec_args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env(
            "CODEWHALE_CONFIG_PATH",
            home.path().join(".codewhale").join("config.toml"),
        )
        .env("DEEPSEEK_API_KEY", "ci-test-key-not-real")
        .env("DEEPSEEK_BASE_URL", server.uri())
        .env("CODEWHALE_BASE_URL", server.uri())
        .env("DEEPSEEK_MODEL", TEST_MODEL)
        .env("CODEWHALE_MODEL", TEST_MODEL)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Stdin::Inherit => {
            command.stdin(Stdio::null());
        }
        Stdin::Bytes(_) | Stdin::OpenPipe => {
            command.stdin(Stdio::piped());
        }
    }

    let started = Instant::now();
    let mut child = command.spawn().expect("spawn codewhale exec");
    let mut held_open = None;
    match stdin {
        Stdin::Bytes(text) => {
            let mut pipe = child.stdin.take().expect("stdin pipe");
            let _ = pipe.write_all(text.as_bytes());
        }
        Stdin::OpenPipe => held_open = child.stdin.take(),
        Stdin::Inherit => {}
    }
    let stdout = drain(child.stdout.take().expect("stdout"));
    let stderr = drain(child.stderr.take().expect("stderr"));
    let status = match child.wait_timeout(RUN_TIMEOUT).expect("wait") {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "codewhale exec did not exit within {RUN_TIMEOUT:?}\nstderr:\n{}",
                String::from_utf8_lossy(&stderr.join().unwrap_or_default())
            );
        }
    };
    let elapsed = started.elapsed();
    drop(held_open);
    let dispatched = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/v1/chat/completions")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    Outcome {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned(),
        elapsed,
        dispatched,
    }
}

fn assert_rejected_before_dispatch(outcome: &Outcome, flag: &str) {
    assert!(
        !outcome.success,
        "a flag after the prompt must fail\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    assert!(
        outcome.stderr.contains(flag),
        "stderr must name {flag}\nstderr:\n{}",
        outcome.stderr
    );
    assert!(
        outcome.dispatched.is_empty(),
        "nothing may reach the provider: {}",
        outcome.dispatched
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_after_the_prompt_are_rejected_not_sent_as_text() {
    let server = start_mock_llm().await;
    for args in [
        vec!["fix tests", "--auto"],
        vec!["fix", "tests", "--auto", "now"],
        vec!["fix tests", "--auto=true"],
    ] {
        let outcome = run_exec(&server, &args, Stdin::Inherit).await;
        assert_rejected_before_dispatch(&outcome, "--auto");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn full_width_and_typographic_flags_after_the_prompt_are_rejected() {
    let server = start_mock_llm().await;
    for (word, shown) in [
        ("\u{FF0D}\u{FF0D}auto", "--auto"),
        ("\u{2014}auto", "--auto"),
        ("\u{2013}\u{2013}auto", "--auto"),
        ("--\u{FF41}\u{FF55}\u{FF54}\u{FF4F}", "--auto"),
    ] {
        let outcome = run_exec(&server, &["fix tests", word], Stdin::Inherit).await;
        assert_rejected_before_dispatch(&outcome, shown);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn separator_keeps_a_literal_flag_in_the_prompt() {
    let server = start_mock_llm().await;
    let outcome = run_exec(&server, &["--", "explain", "--auto"], Stdin::Inherit).await;
    assert!(outcome.success, "stderr:\n{}", outcome.stderr);
    assert!(
        outcome.dispatched.contains("explain --auto"),
        "dispatched: {}",
        outcome.dispatched
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unrelated_open_pipe_does_not_hang_an_explicit_prompt() {
    let server = start_mock_llm().await;
    let outcome = run_exec(&server, &["say hello"], Stdin::OpenPipe).await;
    assert!(
        outcome.success,
        "stdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    assert!(outcome.dispatched.contains("say hello"));
    assert!(
        outcome.stderr.contains("stdin produced no data"),
        "the ignored stdin must be reported\nstderr:\n{}",
        outcome.stderr
    );
    assert!(
        outcome.elapsed < Duration::from_secs(30),
        "elapsed {:?}",
        outcome.elapsed
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn piped_stdin_is_appended_to_the_prompt_and_is_the_prompt_alone() {
    let server = start_mock_llm().await;
    let appended = run_exec(
        &server,
        &["review this"],
        Stdin::Bytes("diff --git a/x b/x\n"),
    )
    .await;
    assert!(appended.success, "stderr:\n{}", appended.stderr);
    assert!(appended.dispatched.contains("review this"));
    assert!(appended.dispatched.contains("diff --git a/x b/x"));

    let alone = run_exec(&server, &[], Stdin::Bytes("summarize the pipe prompt\n")).await;
    assert!(alone.success, "stderr:\n{}", alone.stderr);
    assert!(alone.dispatched.contains("summarize the pipe prompt"));
}

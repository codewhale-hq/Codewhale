//! Process-level acceptance for mid-stream provider drops.
//!
//! An opened stream that drops after partial content has an unknown provider
//! outcome, so the engine must not replay it: `codewhale exec` surfaces the
//! failure once and exits 1, and an interactive session accepts the next
//! manual turn.
//!
//! These tests drive the real binary against a raw TCP server that sends a
//! partial SSE body and then closes mid-`content-length`, reproducing the
//! exact reqwest decode failure from the bench artifacts.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use wait_timeout::ChildExt;

#[cfg(all(unix, feature = "long-running-tests"))]
#[path = "../support/qa_harness/mod.rs"]
mod qa_harness;

const MODEL: &str = "stream-drop-test";
/// The server claims a body far larger than it delivers on a "drop" response,
/// so hyper raises `error decoding response body` after the first SSE chunk —
/// the exact failure string in the Terminal-Bench crash artifacts.
const CLAIMED_DROP_BODY_LEN: usize = 1_048_576;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_exec_does_not_replay_a_mid_stream_drop() {
    let workspace = TempDir::new().expect("workspace");
    let home = TempDir::new().expect("home");
    let (base_url, chat_posts, _requests, _server) =
        start_flaky_server(usize::MAX, Duration::ZERO).await;

    let output = run_exec(workspace.path(), home.path(), &base_url);

    assert_eq!(
        output.status.code(),
        Some(1),
        "an opened stream that drops has an unknown outcome and fails the exec turn\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        chat_posts.load(Ordering::SeqCst),
        1,
        "an opened stream must never be replayed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let error_events = stdout
        .lines()
        .filter(|line| line.contains(r#""type":"error""#))
        .count();
    assert_eq!(error_events, 1, "{stdout}");
    let error_line = stdout
        .lines()
        .find(|line| line.contains(r#""type":"error""#))
        .expect("terminal error event");
    assert!(
        error_line.contains("Provider stream connection dropped"),
        "the error channel must carry the real failure: {error_line}"
    );
    let meta = terminal_metadata(&stdout);
    assert_eq!(meta["meta"]["status"].as_str(), Some("failed"), "{meta}");
    assert_eq!(
        meta["meta"]["error_category"].as_str(),
        Some("network"),
        "the terminal receipt must classify the failure as network: {meta}"
    );
}

/// #5769's later report: the NEXT manually submitted turn, with no approval,
/// must reach the real dispatcher and survive its UI watchdog after a loss.
#[cfg(all(unix, feature = "long-running-tests"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tui_next_manual_turn_completes_after_exhausted_partial_sse_drop() {
    use qa_harness::harness::{Harness, make_sealed_workspace};

    const FIRST: &str = "R4_FIRST_MANUAL";
    const SECOND: &str = "R4_NEXT_MANUAL";
    // Deliberately cross the current 30-second dispatch watchdog using wall
    // time. The request is admitted, but its HTTP response is still pending.
    const HEALTHY_RESPONSE_DELAY: Duration = Duration::from_secs(35);
    const WAIT: Duration = Duration::from_secs(60);
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let (base_url, chat_posts, requests, server) =
        start_flaky_server(1, HEALTHY_RESPONSE_DELAY).await;
    let outbox = workspace.home().join("turn-receipts.jsonl");
    std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").expect("onboarding receipt");
    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).expect("trust directory");
    std::fs::write(trust.join("trusted"), "").expect("workspace trust");
    let config = format!(
        r#"provider = "loopback"
prompt_suggestion = false
allow_shell = false
[providers.loopback]
kind = "openai-compatible"
base_url = {base_url}
api_key = "synthetic-loopback-key"
model = "{MODEL}"
[retry]
enabled = false
[notifications]
method = "off"
completion_sound = "off"
[lifecycle_outbox]
path = {outbox}
"#,
        base_url = json!(base_url),
        outbox = json!(outbox),
    );
    std::fs::write(workspace.home().join(".codewhale/config.toml"), config)
        .expect("loopback TUI configuration");
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_TELEMETRY", "0")
        .env("NO_ANIMATIONS", "1")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--fresh",
        ])
        .size(40, 120)
        .spawn()
        .expect("start the real TUI");
    let pid = tui.pid().expect("TUI PID");
    tui.wait_for_text("Type a message", WAIT)
        .expect("ready composer");
    tui.type_line(FIRST).expect("first manual submission");
    wait_for_tui_turn_ends(&mut tui, &outbox, 1, WAIT);
    let first_events = read_tui_outbox(&outbox);
    let first = first_events
        .iter()
        .find(|event| event["event"] == "turn_end")
        .expect("first terminal UI receipt");
    assert_eq!(first["kind"], "turn.failed", "{first:#}");
    assert!(
        first["payload"]["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("Provider stream connection dropped")),
        "the first UI turn must fail on an actual partial SSE loss: {first:#}"
    );
    // The lifecycle outbox contains a bounded user-facing error, not the
    // underlying reqwest chain. The server's partial SSE body and the exact
    // request count prove the real transport loss and that it was not replayed.
    assert_eq!(chat_posts.load(Ordering::SeqCst), 1);

    // No restart, continuation, event injection, approval answer, or queue
    // admission: type into the same running composer after its failed receipt.
    let submitted = std::time::Instant::now();
    tui.type_line(SECOND).expect("next manual submission");
    wait_for_tui_turn_ends(&mut tui, &outbox, 2, WAIT);
    assert!(submitted.elapsed() >= HEALTHY_RESPONSE_DELAY);
    tui.wait_for_text("recovered after retry", WAIT)
        .expect("second answer reaches the actual UI");
    // Completion is proved by the lifecycle receipt below. The composer
    // intentionally omits the old transient "turn completed" chrome; prove
    // that the actual UI is ready and still accepts an unsent edit instead.
    tui.wait_for(
        |frame| frame.contains("recovered after retry") && frame.contains("Type a message"),
        WAIT,
    )
    .expect("completed answer and ready composer reach the actual UI");
    const UNSENT: &str = "R4_UNSENT_RECOVERY_CHECK";
    tui.paste(UNSENT).expect("edit the recovered composer");
    tui.wait_for(|frame| frame.row(frame.cursor().0).contains(UNSENT), WAIT)
        .expect("the same composer remains editable after completion");
    assert_eq!(tui.pid(), Some(pid));
    let events = read_tui_outbox(&outbox);
    let starts = events
        .iter()
        .filter(|event| event["event"] == "turn_start")
        .collect::<Vec<_>>();
    let ends = events
        .iter()
        .filter(|event| event["event"] == "turn_end")
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2, "two real TUI dispatches: {events:#?}");
    assert_eq!(ends.len(), 2, "two real TUI completions: {events:#?}");
    assert_eq!(ends[1]["kind"], "turn.completed");
    assert!(ends[1]["payload"]["error"].is_null());
    let session = ends[0]["thread_id"].as_str().expect("TUI session identity");
    assert!(!session.is_empty());
    assert!(
        starts
            .iter()
            .chain(&ends)
            .all(|event| event["thread_id"] == session)
    );
    assert!(ends.iter().all(|event| event["turn_id"].is_string()));
    assert_ne!(ends[0]["turn_id"], ends[1]["turn_id"]);
    assert!(events.iter().all(|event| {
        !event["event"].as_str().unwrap().contains("approval")
            && !event["event"].as_str().unwrap().contains("user_input")
    }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "one failed attempt then one new turn");
    let final_messages = requests.last().unwrap()["messages"].as_array().unwrap();
    let users = final_messages
        .iter()
        .filter(|message| {
            message["role"] == "user" && !message.to_string().contains("codewhale:runtime_event")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        users.len(),
        2,
        "no synthetic continuation user turns: {final_messages:#?}"
    );
    assert!(users[0]["content"].to_string().contains(FIRST));
    assert!(users[1]["content"].to_string().contains(SECOND));
    assert!(
        final_messages
            .iter()
            .all(|message| message["role"] != "tool")
    );
    assert!(requests.iter().all(|request| request["stream"] == true));
    drop(requests);
    tui.shutdown();
    server.abort();
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn read_tui_outbox(path: &Path) -> Vec<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("read TUI lifecycle receipt: {error}"),
    };
    // A concurrent append can expose an unfinished last line. Only complete
    // records count as receipts; a malformed complete record is a failure.
    text.split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str(line).expect("complete TUI outbox record"))
        .collect()
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn wait_for_tui_turn_ends(
    tui: &mut qa_harness::harness::Harness,
    outbox: &Path,
    count: usize,
    timeout: Duration,
) {
    tui.wait_for(
        |frame| {
            for unexpected in [
                "Turn dispatch timed out",
                "engine may have stopped",
                "Approval required",
                "engine session id diverged",
            ] {
                assert!(
                    !frame.contains(unexpected),
                    "{unexpected}: {}",
                    frame.text()
                );
            }
            read_tui_outbox(outbox)
                .iter()
                .filter(|event| event["event"] == "turn_end")
                .count()
                >= count
        },
        timeout,
    )
    .unwrap_or_else(|error| {
        panic!(
            "UI did not finish turn {count}: {error}\n{}",
            tui.diagnostics()
        )
    });
}

/// Extract the terminal `metadata` event from the stream-json stdout.
fn terminal_metadata(stdout: &str) -> Value {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.contains(r#""type":"metadata""#))
        .unwrap_or_else(|| panic!("stream-json metadata event missing: {stdout}"));
    serde_json::from_str(line).expect("metadata event is valid JSON")
}

/// Spawn a raw HTTP server that answers `GET /v1/models` and, for every POST
/// to the chat endpoint, either truncates the SSE body mid-stream (the first
/// `drops` requests) or completes a normal text turn. Returns the base URL
/// and a counter of chat-completion POSTs.
async fn start_flaky_server(
    drops: usize,
    healthy_response_delay: Duration,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind flaky server");
    let addr = listener.local_addr().expect("flaky server addr");
    let chat_posts = Arc::new(AtomicUsize::new(0));
    let server_posts = Arc::clone(&chat_posts);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let request = match read_http_request(&mut socket).await {
                Some(request) => request,
                None => continue,
            };
            if request.starts_with("GET ") {
                write_all(
                    &mut socket,
                    &http_response(
                        "application/json",
                        &json!({"object":"list","data":[{"id":MODEL,"object":"model"}]})
                            .to_string(),
                    ),
                )
                .await;
                continue;
            }
            let body = request.split_once("\r\n\r\n").expect("request body").1;
            captured
                .lock()
                .unwrap()
                .push(serde_json::from_str(body).expect("chat request JSON"));
            let call = server_posts.fetch_add(1, Ordering::SeqCst) + 1;
            if call <= drops {
                // Partial SSE (one real content chunk), then the socket
                // closes with the declared content-length unmet — the
                // production "Provider stream connection dropped" failure.
                let partial = sse_chunk(
                    json!({"id":"drop","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{"content":"partial answer that must be discarded"},"finish_reason":null}]}),
                );
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {CLAIMED_DROP_BODY_LEN}\r\nconnection: close\r\n\r\n"
                );
                write_all(&mut socket, &format!("{head}{partial}")).await;
            } else {
                tokio::time::sleep(healthy_response_delay).await;
                let body = [
                    sse_chunk(json!({"id":"final","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{"content":"recovered after retry"},"finish_reason":null}]})),
                    sse_chunk(json!({"id":"final","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":2,"total_tokens":22}})),
                    "data: [DONE]\n\n".to_string(),
                ]
                .join("");
                write_all(&mut socket, &http_response("text/event-stream", &body)).await;
            }
        }
    });
    (format!("http://{addr}/v1"), chat_posts, requests, task)
}

/// Read one HTTP request (headers plus the content-length body, when any).
async fn read_http_request(socket: &mut tokio::net::TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&buffer, b"\r\n\r\n") {
            break pos + 4;
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 1 << 20 {
            return None;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let request_len = header_end.checked_add(content_length)?;
    if request_len > 1 << 20 {
        return None;
    }
    while buffer.len() < request_len {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    buffer.truncate(request_len);
    String::from_utf8(buffer).ok()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn write_all(socket: &mut tokio::net::TcpStream, bytes: &str) {
    use tokio::io::AsyncWriteExt;
    let _ = socket.write_all(bytes.as_bytes()).await;
    let _ = socket.shutdown().await;
}

fn http_response(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sse_chunk(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

fn run_exec(workspace: &Path, home: &Path, base_url: &str) -> std::process::Output {
    std::fs::create_dir_all(home.join(".codewhale")).expect("config directory");
    std::fs::create_dir_all(home.join(".deepseek")).expect("legacy config directory");
    std::fs::write(
        home.join(".codewhale/config.toml"),
        "allow_shell = true\n\n[retry]\nenabled = false\n",
    )
    .expect("headless test config");
    let mut command = Command::new(crate::binary::codewhale());
    preserve_host_env(&mut command);
    command
        .current_dir(workspace)
        .args(["--workspace", workspace.to_str().expect("workspace utf8")])
        .arg("--no-project-config")
        .args([
            "exec",
            "--auto",
            "--model",
            MODEL,
            "--output-format",
            "stream-json",
        ])
        .arg("answer briefly")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("CODEWHALE_CONFIG_PATH", home.join(".codewhale/config.toml"))
        .env("DEEPSEEK_CONFIG_PATH", home.join(".deepseek/config.toml"))
        .env("DEEPSEEK_API_KEY", "ci-test-key-not-real")
        .env("DEEPSEEK_BASE_URL", base_url)
        .env("CODEWHALE_BASE_URL", base_url)
        .env("DEEPSEEK_MODEL", MODEL)
        .env("CODEWHALE_MODEL", MODEL)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_with_timeout(command, Duration::from_secs(45))
}

fn preserve_host_env(command: &mut Command) {
    command.env_clear();
    for key in [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "SystemDrive",
        "WINDIR",
        "COMSPEC",
        "TEMP",
        "TMP",
        "TERM",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    let mut child = command.spawn().expect("spawn codewhale exec");
    let stdout = read_in_background(child.stdout.take().expect("stdout"));
    let stderr = read_in_background(child.stderr.take().expect("stderr"));
    let status = child
        .wait_timeout(timeout)
        .expect("wait")
        .unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("codewhale exec timed out")
        });
    std::process::Output {
        status,
        stdout: stdout.join().expect("stdout thread").expect("read stdout"),
        stderr: stderr.join().expect("stderr thread").expect("read stderr"),
    }
}

fn read_in_background<R: Read + Send + 'static>(
    mut reader: R,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| bytes)
    })
}

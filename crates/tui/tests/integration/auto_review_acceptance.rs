#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::qa_harness;
use qa_harness::harness::{Harness, ci_scaled, make_sealed_workspace};

const MODEL: &str = "fixture-model";
const SAFE_CMD: &str = "printf safe > safe-canary.txt";
const DANGER_CMD: &str = "printf danger > danger-canary.txt";
const RETRY_CMD: &str = "printf retried > retried-canary.txt";

const SAFE_PROMPT: &str = "Please run printf safe > safe-canary.txt";
const DANGER_PROMPT: &str = "Inspect the directory without writing files";
const RETRY_PROMPT: &str = "Please run printf retried > retried-canary.txt";
const FOLLOWUP_PROMPT: &str = "Run the approved command again";

#[derive(Debug, Clone)]
struct GuardianRecord {
    decision: String,
    reason: String,
    person_messages: Vec<String>,
    person_approved_retry: bool,
    proposed: String,
}

struct MockProvider {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    guardians: Arc<Mutex<Vec<GuardianRecord>>>,
    acceptor: Option<std::thread::JoinHandle<()>>,
}

impl MockProvider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback provider");
        let address = listener.local_addr().expect("provider addr");
        let stop = Arc::new(AtomicBool::new(false));
        let guardians = Arc::new(Mutex::new(Vec::new()));
        let stop_flag = Arc::clone(&stop);
        let log = Arc::clone(&guardians);
        let call_seq = Arc::new(AtomicUsize::new(1));
        let acceptor = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_flag.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let log = Arc::clone(&log);
                let seq = Arc::clone(&call_seq);
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, &log, &seq);
                });
            }
        });
        Self {
            address,
            stop,
            guardians,
            acceptor: Some(acceptor),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn records(&self) -> Vec<GuardianRecord> {
        self.guardians.lock().unwrap().clone()
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

fn text_of(content: &Value) -> String {
    if let Some(items) = content.as_array() {
        return items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" ");
    }
    content.as_str().unwrap_or_default().to_string()
}

fn strip_turn_meta(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<turn_meta>") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</turn_meta>") {
            rest = &rest[start + end + "</turn_meta>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn is_human_turn(msg: &Value) -> bool {
    if msg.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let text = strip_turn_meta(&text_of(&msg["content"]));
    let trimmed = text.trim_start();
    !trimmed.starts_with("<codewhale:runtime_event")
        && !text.contains("\"current_deferred_toolbox\"")
        && !text.contains("\"proposed_tool_call\"")
}

fn extract_review_doc(messages: &[Value]) -> Option<Value> {
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let text = text_of(&msg["content"]);
        let mut idx = 0usize;
        while let Some(offset) = text[idx..].find('{') {
            let start = idx + offset;
            let mut de = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
            if let Some(Ok(doc)) = de.next()
                && doc.is_object()
                && doc.get("person_messages").is_some()
            {
                return Some(doc);
            }
            idx = start + 1;
        }
    }
    None
}

fn judge_guardian(doc: &Value) -> (&'static str, &'static str, &'static str) {
    let persons: Vec<String> = doc["person_messages"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let proposed = serde_json::to_string(&doc["proposed_tool_call"]).unwrap_or_default();
    let approved_retry =
        doc["deterministic_observations"]["person_approved_retry"].as_bool() == Some(true);
    if persons.is_empty() {
        return (
            "deny",
            "high",
            "No person request provided (person_messages is empty).",
        );
    }
    if proposed.contains("safe-canary.txt") && persons.iter().any(|p| p.contains("safe-canary.txt"))
    {
        return (
            "allow",
            "low",
            "Person explicitly requested creating safe-canary.txt.",
        );
    }
    if proposed.contains("danger-canary.txt") {
        return (
            "deny",
            "high",
            "Unrequested dangerous write to danger-canary.txt blocked.",
        );
    }
    if proposed.contains("retried-canary.txt") {
        if approved_retry {
            return (
                "allow",
                "low",
                "Person approved one-time retry of this exact call.",
            );
        }
        return (
            "deny",
            "high",
            "Held write awaiting explicit person retry approval.",
        );
    }
    ("deny", "medium", "Unrecognized command blocked.")
}

fn handle_connection(
    mut stream: TcpStream,
    guardians: &Mutex<Vec<GuardianRecord>>,
    call_seq: &AtomicUsize,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length.min(8 * 1024 * 1024)];
    reader.read_exact(&mut body)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method == "GET" && path.ends_with("/models") {
        return write_json(
            &mut stream,
            "200 OK",
            &json!({"object": "list", "data": [{"id": MODEL, "object": "model"}]}),
        );
    }
    if method != "POST" {
        return write_json(&mut stream, "404 Not Found", &json!({"error": "not found"}));
    }

    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system_text = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .map(|m| text_of(&m["content"]).to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    if system_text.contains("auto-review guardian") {
        let doc = extract_review_doc(&messages).unwrap_or_else(|| json!({}));
        let (decision, risk, reason) = judge_guardian(&doc);
        let person_messages: Vec<String> = doc["person_messages"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let person_approved_retry =
            doc["deterministic_observations"]["person_approved_retry"].as_bool() == Some(true);
        let proposed = serde_json::to_string(&doc["proposed_tool_call"]).unwrap_or_default();
        guardians.lock().unwrap().push(GuardianRecord {
            decision: decision.to_string(),
            reason: reason.to_string(),
            person_messages,
            person_approved_retry,
            proposed,
        });
        let content = json!({
            "risk_level": risk,
            "decision": decision,
            "reason": reason,
        })
        .to_string();
        return write_json(
            &mut stream,
            "200 OK",
            &json!({
                "id": "guardian",
                "object": "chat.completion",
                "model": MODEL,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 20, "completion_tokens": 40, "total_tokens": 60},
            }),
        );
    }

    let bash_name = request["tools"].as_array().and_then(|tools| {
        tools.iter().find_map(|tool| {
            let name = tool["function"]["name"].as_str()?;
            name.eq_ignore_ascii_case("bash").then(|| name.to_string())
        })
    });
    let last_user_idx = messages
        .iter()
        .rposition(|m| {
            if !is_human_turn(m) {
                return false;
            }
            let t = strip_turn_meta(&text_of(&m["content"]));
            t.contains(SAFE_PROMPT)
                || t.contains(DANGER_PROMPT)
                || t.contains(RETRY_PROMPT)
                || t.contains(FOLLOWUP_PROMPT)
        })
        .or_else(|| messages.iter().rposition(is_human_turn));
    let prompt_text = last_user_idx
        .map(|idx| strip_turn_meta(&text_of(&messages[idx]["content"])))
        .unwrap_or_default();
    let already_called = last_user_idx.is_some_and(|idx| {
        messages[idx + 1..].iter().any(|m| {
            m.get("role").and_then(Value::as_str) == Some("assistant")
                && m.get("tool_calls").is_some()
        })
    });

    let command_to_run = if prompt_text.contains(SAFE_PROMPT) {
        Some(SAFE_CMD)
    } else if prompt_text.contains(DANGER_PROMPT) {
        Some(DANGER_CMD)
    } else if prompt_text.contains(RETRY_PROMPT) || prompt_text.contains(FOLLOWUP_PROMPT) {
        Some(RETRY_CMD)
    } else {
        None
    };

    let tool_call = match (command_to_run, already_called, bash_name) {
        (Some(cmd), false, Some(tool_name)) => {
            let id = format!("call_ar_{}", call_seq.fetch_add(1, Ordering::AcqRel));
            Some((id, tool_name, json!({ "command": cmd }).to_string()))
        }
        _ => None,
    };

    if !request["stream"].as_bool().unwrap_or(false) {
        let message = match tool_call {
            Some((id, name, arguments)) => json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{"id": id, "type": "function", "function": {"name": name, "arguments": arguments}}],
            }),
            None => json!({"role": "assistant", "content": "Fixture turn complete."}),
        };
        let finish = if message.get("tool_calls").is_some() {
            "tool_calls"
        } else {
            "stop"
        };
        return write_json(
            &mut stream,
            "200 OK",
            &json!({
                "id": "fixture",
                "object": "chat.completion",
                "model": MODEL,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                "usage": {"prompt_tokens": 20, "completion_tokens": 40, "total_tokens": 60},
            }),
        );
    }

    stream.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
    )?;
    stream.flush()?;
    let finish = if let Some((id, name, arguments)) = tool_call {
        send_sse_chunk(
            &mut stream,
            &json!({
                "id": "fixture",
                "object": "chat.completion.chunk",
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": arguments},
                        }]
                    },
                    "finish_reason": null,
                }]
            }),
        )?;
        "tool_calls"
    } else {
        send_sse_chunk(
            &mut stream,
            &json!({
                "id": "fixture",
                "object": "chat.completion.chunk",
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": {"content": "Fixture turn complete."},
                    "finish_reason": null,
                }]
            }),
        )?;
        "stop"
    };
    send_sse_chunk(
        &mut stream,
        &json!({
            "id": "fixture",
            "object": "chat.completion.chunk",
            "model": MODEL,
            "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 40, "total_tokens": 60},
        }),
    )?;
    stream.write_all(b"data: [DONE]\n\n")?;
    stream.flush()
}

fn send_sse_chunk(stream: &mut TcpStream, chunk: &Value) -> std::io::Result<()> {
    stream.write_all(format!("data: {chunk}\n\n").as_bytes())?;
    stream.flush()
}

fn write_json(stream: &mut TcpStream, status: &str, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.flush()
}

fn read_outbox(path: &Path) -> Vec<Value> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("valid outbox JSON line"))
        .collect()
}

fn wait_for_turn_ends(tui: &mut Harness, outbox: &Path, count: usize, timeout: Duration) {
    tui.wait_for(
        |frame| {
            assert!(
                !frame.contains("Turn dispatch timed out"),
                "turn timed out: {}",
                frame.text()
            );
            read_outbox(outbox)
                .iter()
                .filter(|event| event["event"] == "turn_end")
                .count()
                >= count
        },
        timeout,
    )
    .unwrap_or_else(|error| {
        panic!(
            "TUI did not finish turn {count}: {error}\n{}",
            tui.diagnostics()
        )
    });
}

#[test]
fn tui_auto_review_end_to_end_person_messages_block_and_retry() {
    let wait = ci_scaled(Duration::from_secs(30));
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let provider = MockProvider::start();
    let outbox = workspace.home().join("turn-receipts.jsonl");
    std::fs::write(workspace.home().join(".codewhale/.onboarded"), "").expect("onboarded");
    std::fs::write(
        workspace.home().join(".codewhale/settings.toml"),
        "permission_posture = \"auto_review\"\n",
    )
    .expect("settings");
    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).expect("trust dir");
    std::fs::write(trust.join("trusted"), "").expect("trusted");
    let config = format!(
        r#"provider = "fixture"
default_text_model = "{MODEL}"
prompt_suggestion = false
allow_shell = true
telemetry = false

[providers.fixture]
kind = "openai-compatible"
base_url = {base_url}
api_key = "synthetic-auto-review-key"
model = "{MODEL}"

[auto_review]
review_all_shell = true

[retry]
enabled = false

[notifications]
method = "off"
completion_sound = "off"

[lifecycle_outbox]
path = {outbox}
"#,
        base_url = json!(provider.base_url()),
        outbox = json!(outbox),
    );
    std::fs::write(workspace.home().join(".codewhale/config.toml"), config).expect("config");

    let mut tui = Harness::builder(crate::binary::codewhale())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        )
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", "1")
        .env("NO_PROXY", "*,127.0.0.1")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
            "--permission-mode",
            "auto-review",
        ])
        .size(40, 120)
        .spawn()
        .expect("spawn TUI");

    tui.wait_for_text("Type a message", wait)
        .expect("ready composer");

    let safe_path = workspace.workspace().join("safe-canary.txt");
    let danger_path = workspace.workspace().join("danger-canary.txt");
    let retried_path = workspace.workspace().join("retried-canary.txt");

    tui.type_line(SAFE_PROMPT).expect("send safe prompt");
    wait_for_turn_ends(&mut tui, &outbox, 1, wait);
    assert!(safe_path.is_file(), "requested safe command must execute");
    assert_eq!(std::fs::read_to_string(&safe_path).unwrap(), "safe");

    tui.type_line(DANGER_PROMPT).expect("send danger prompt");
    wait_for_turn_ends(&mut tui, &outbox, 2, wait);
    assert!(
        !danger_path.exists(),
        "dangerous unrequested command must stay blocked"
    );

    tui.type_line(RETRY_PROMPT)
        .expect("send retry-target prompt");
    wait_for_turn_ends(&mut tui, &outbox, 3, wait);
    assert!(
        !retried_path.exists(),
        "held command must not run before approval"
    );
    tui.wait_for(
        |frame| {
            frame
                .text()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("Press r on an empty prompt")
        },
        wait,
    )
    .expect("retry hint rendered in TUI");

    tui.send(b"r").expect("press r to approve one-time retry");
    tui.wait_for(
        |frame| {
            frame
                .text()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("Approved one retry of")
        },
        wait,
    )
    .expect("TUI status confirms one-time retry approval");

    tui.type_line(FOLLOWUP_PROMPT)
        .expect("send follow-up retry turn");
    wait_for_turn_ends(&mut tui, &outbox, 4, wait);
    assert!(
        retried_path.is_file(),
        "approved retry command must execute"
    );
    assert_eq!(std::fs::read_to_string(&retried_path).unwrap(), "retried");

    let records = provider.records();
    let safe_rec = records
        .iter()
        .find(|r| r.proposed.contains("safe-canary.txt"))
        .expect("guardian saw safe command");
    assert_eq!(safe_rec.decision, "allow", "{safe_rec:?}");
    assert!(
        safe_rec
            .person_messages
            .iter()
            .any(|m| m.contains("safe-canary.txt")),
        "guardian must see person message for safe command: {safe_rec:?}"
    );

    let danger_rec = records
        .iter()
        .find(|r| r.proposed.contains("danger-canary.txt"))
        .expect("guardian saw dangerous command");
    assert_eq!(danger_rec.decision, "deny", "{danger_rec:?}");
    assert!(
        danger_rec
            .person_messages
            .iter()
            .any(|m| m.contains(DANGER_PROMPT)),
        "guardian must see person message on dangerous command: {danger_rec:?}"
    );

    let retry_records: Vec<&GuardianRecord> = records
        .iter()
        .filter(|r| r.proposed.contains("retried-canary.txt"))
        .collect();
    assert_eq!(retry_records.len(), 2, "{retry_records:#?}");
    assert_eq!(retry_records[0].decision, "deny");
    assert!(!retry_records[0].person_approved_retry);
    assert_eq!(retry_records[1].decision, "allow");
    assert!(retry_records[1].person_approved_retry);

    println!(
        "AUTO_REVIEW_ACCEPTANCE_RECEIPT: safe={:?} danger={:?} held={:?} retried={:?}",
        safe_rec.reason, danger_rec.reason, retry_records[0].reason, retry_records[1].reason
    );

    tui.shutdown();
}

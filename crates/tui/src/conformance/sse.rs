//! Recorded-SSE family: provider wire bytes → normalized stream events.
//!
//! Each case replays a synthetic recording (`<case>.sse`, never a real
//! provider capture) from a loopback HTTP server into the real
//! `CodewhaleClient::create_message_stream` for one route, and pins what the
//! wire adapter yields. The first golden line is the request the adapter
//! sent (method and path only — the body is the prompt family's business);
//! every later line is one normalized [`super::stream_json`] event, or one
//! `stream_failure` / `open_failure` record with exact `detail` bytes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use codewhale_models::{ContentBlock, Message, MessageRequest, Role};

use super::golden::{self, Failures};
use super::stream_json;
use crate::client::CodewhaleClient;
use crate::llm_client::LlmClient;
use crate::test_support::{EnvVarGuard, lock_test_env};
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};

const FAMILY: &str = "sse";
const STREAM_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
struct ReceivedRequest {
    method: String,
    path: String,
}

#[tokio::test]
async fn harness_timeout_rejects_a_real_stalled_sse_connection() {
    let mut case = golden::read_case(FAMILY, "anthropic_thinking_text_usage");
    case["hold_open"] = json!(true);
    case["framing"] = json!("chunked");
    let outcome = replay_with_deadline(&case, Vec::new(), Duration::from_secs(1)).await;
    assert!(
        matches!(outcome, Err(error) if error.contains("harness timeout: SSE replay")),
        "a stalled SSE connection became recordable output"
    );
}

#[tokio::test]
async fn empty_sse_recording_cannot_be_qualified() {
    let case = golden::read_case(FAMILY, "anthropic_thinking_text_usage");
    assert!(
        replay(&case, Vec::new()).await.is_err(),
        "empty recording became a passing case"
    );
}

/// One-shot-per-connection loopback server that answers every request with
/// the recording. `close` framing ends the body by closing the socket (a
/// provider that stops sending); `chunked` framing uses HTTP chunks, and
/// `transport_cut` drops the connection before the terminating chunk so the
/// client sees a transport error rather than a clean EOF.
struct LoopbackRecording {
    base: String,
    requests: Arc<Mutex<Vec<ReceivedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackRecording {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct Framing {
    chunked: bool,
    transport_cut: bool,
    chunk_bytes: usize,
    hold_open: bool,
}

impl LoopbackRecording {
    async fn start(body: Vec<u8>, framing: Framing) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = connections.join_next(), if !connections.is_empty() => continue,
                };
                let Ok((socket, _)) = accepted else {
                    break;
                };
                let body = body.clone();
                let framing = framing.clone();
                let captured = Arc::clone(&captured);
                connections.spawn(async move {
                    serve(socket, body, framing, captured).await;
                });
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }
}

async fn serve(
    socket: tokio::net::TcpStream,
    body: Vec<u8>,
    framing: Framing,
    captured: Arc<Mutex<Vec<ReceivedRequest>>>,
) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut request_body = vec![0u8; content_length];
    if reader.read_exact(&mut request_body).await.is_err() {
        return;
    }
    captured
        .lock()
        .expect("request log")
        .push(ReceivedRequest { method, path });

    let mut socket = reader.into_inner();
    let head = if framing.chunked {
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    } else {
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    };
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let size = if framing.chunk_bytes == 0 {
        body.len().max(1)
    } else {
        framing.chunk_bytes
    };
    for piece in body.chunks(size) {
        let written = if framing.chunked {
            let mut frame = format!("{:x}\r\n", piece.len()).into_bytes();
            frame.extend_from_slice(piece);
            frame.extend_from_slice(b"\r\n");
            socket.write_all(&frame).await
        } else {
            socket.write_all(piece).await
        };
        if written.is_err() || socket.flush().await.is_err() {
            return;
        }
        // Give the client a chance to observe a split line before the rest.
        tokio::task::yield_now().await;
    }
    if framing.hold_open {
        // A stalled provider for the harness timeout regression. The listener
        // owns this task, so dropping the replay cancels the open socket too.
        std::future::pending::<()>().await;
    }
    if framing.chunked && !framing.transport_cut {
        let _ = socket.write_all(b"0\r\n\r\n").await;
    }
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn client_for_route(route: &str, base: &str, model: &str) -> CodewhaleClient {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = match route {
        "anthropic" => {
            let __cw_config_init_0_0 = Some("anthropic".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                anthropic: ProviderConfig {
                    api_key: Some("conformance-key".to_string()),
                    base_url: Some(base.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        }
        "openai" => {
            let __cw_config_init_1_0 = Some("openai".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                openai: ProviderConfig {
                    api_key: Some("conformance-key".to_string()),
                    base_url: Some(format!("{base}/v1")),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        }
        // Keep DeepSeek's semantic endpoint for route shaping; redirect only
        // its transport through the existing test seam below.
        "deepseek" => {
            let __cw_config_init_2_0 = Some("deepseek".to_string());
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value
        }
        .with_legacy_root(
            Some("conformance-key".to_string()),
            Some("https://api.deepseek.com/v1".to_string()),
        ),
        "openai-codex" => {
            let __cw_config_init_3_0 = Some("openai-codex".to_string());
            let __cw_config_init_3_1 = Some(ProvidersConfig {
                openai_codex: ProviderConfig {
                    base_url: Some(base.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        }
        other => panic!("sse fixture names unknown route `{other}`"),
    };
    // The fixture model and the client must share the production resolver.
    // Constructing from Config's default model can bind a different wire
    // format and record an open_failure without exercising the recording.
    let resolved = crate::route_runtime::resolve_runtime_route(
        &config,
        config.active_provider_identity().unwrap().provider,
        Some(model),
    )
    .expect("resolve fixture route");
    let mut client = CodewhaleClient::from_candidate(&resolved.config, &resolved.candidate)
        .expect("fixture client");
    if route == "deepseek" {
        client.set_test_chat_transport_base_url(base.to_string());
    }
    client
}

fn request_for_case(case: &Value) -> MessageRequest {
    let model = case["model"].as_str().expect("case.model").to_string();
    let tools = case.get("tools").map(|tools| {
        serde_json::from_value::<Vec<codewhale_models::Tool>>(tools.clone())
            .expect("case.tools parse as Tool definitions")
    });
    MessageRequest {
        model,
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "conformance request".to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: 512,
        system: None,
        tools,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: case["reasoning_effort"].as_str().map(str::to_string),
        stream: Some(true),
        temperature: None,
        top_p: None,
    }
}

async fn replay(case: &Value, recording: Vec<u8>) -> Result<Vec<Value>, String> {
    replay_with_deadline(case, recording, STREAM_DEADLINE).await
}

async fn replay_with_deadline(
    case: &Value,
    recording: Vec<u8>,
    deadline: Duration,
) -> Result<Vec<Value>, String> {
    let framing = Framing {
        chunked: case["framing"].as_str() == Some("chunked"),
        transport_cut: case["transport_cut"].as_bool().unwrap_or(false),
        chunk_bytes: case["chunk_bytes"].as_u64().unwrap_or(0) as usize,
        hold_open: case["hold_open"].as_bool().unwrap_or(false),
    };
    let server = LoopbackRecording::start(recording, framing).await;
    let route = case["route"].as_str().expect("case.route");
    let client = {
        // Client construction reads provider credentials from the
        // environment; only the Codex route needs one, and it is synthetic.
        let _env = lock_test_env();
        let _codex = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "conformance-token");
        let _legacy_codex = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        client_for_route(
            route,
            &server.base,
            case["model"].as_str().expect("case.model"),
        )
    };

    let mut lines = Vec::new();
    let events = golden::complete_within("SSE replay", deadline, async {
        let mut events = Vec::new();
        match client.create_message_stream(request_for_case(case)).await {
            Err(error) => events.push(json!({
                "type": "open_failure",
                "detail": format!("{error:#}"),
            })),
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(event) => events.push(stream_json::to_json(&event)),
                        Err(error) => events.push(json!({
                            "type": "stream_failure",
                            "detail": format!("{error:#}"),
                        })),
                    }
                }
            }
        }
        events
    })
    .await?;
    if events.is_empty() {
        return Err("SSE replay produced no event or provider error".to_string());
    }

    let requests = server.requests.lock().expect("request log").clone();
    if requests.is_empty()
        || !events
            .iter()
            .any(|event| stream_json::from_json(event).is_ok())
    {
        return Err(
            "SSE recording was not exercised: no loopback request or normalized stream event"
                .to_string(),
        );
    }
    lines.push(json!({
        "request": requests
            .iter()
            .map(|request| json!({ "method": request.method, "path": request.path }))
            .collect::<Vec<_>>(),
    }));
    lines.extend(events);
    Ok(lines)
}

#[tokio::test]
async fn recorded_sse_streams_match_goldens() {
    let dir = golden::family_dir(FAMILY);
    let names = golden::case_names(FAMILY);
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        let recording_name = case["recording"]
            .as_str()
            .map_or_else(|| format!("{name}.sse"), str::to_string);
        let recording = std::fs::read(dir.join(&recording_name))
            .unwrap_or_else(|error| panic!("read {recording_name}: {error}"));
        let mut lines = match replay(&case, recording).await {
            Ok(lines) => lines,
            Err(error) => {
                failures.push(name, error);
                continue;
            }
        };
        let mut masker = golden::Masker::new(&[]);
        for line in &mut lines {
            masker.value(line);
        }
        // Every event line must be the StreamEvent serde contract itself, so
        // the events family can feed these goldens straight back in.
        for line in lines.iter().skip(1) {
            let kind = line["type"].as_str().unwrap_or_default();
            if matches!(kind, "stream_failure" | "open_failure") {
                continue;
            }
            if let Err(message) = stream_json::round_trips(line) {
                failures.push(
                    name,
                    format!("normalized event is not StreamEvent: {message}"),
                );
            }
        }
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.golden.jsonl")),
                &golden::jsonl(&lines),
            ),
        );
    }
    failures.finish(FAMILY, names.len());
}

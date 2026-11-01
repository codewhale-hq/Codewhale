//! #5769: a terminal partial SSE loss must not strand the next manual turn.
//! These use the real HTTP client and one EngineHandle throughout, with no
//! approval, restart, SyncSession, or synthetic continuation between turns.

use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

const FIRST_USER: &str = "FIRST_REAL_USER";
const SECOND_USER: &str = "SECOND_REAL_USER";
const NEXT_ANSWER: &str = "NEXT_TURN_COMPLETED";
const SESSION_ID: &str = "sse-recovery-same-session";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_INPUT_TOKENS: u32 = 13;
const FIXTURE_OUTPUT_TOKENS: u32 = 5;

#[derive(Clone, Copy)]
enum Failure {
    TruncatedBody,
    StalledBody,
}

struct LoopbackSse {
    base_url: String,
    requests: Arc<StdMutex<Vec<Value>>>,
    stalled_connection_closed: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackSse {
    fn drop(&mut self) {
        // The server owns its connection tasks through JoinSet: aborting it
        // also closes any still-held response, including assertion failures.
        self.task.abort();
    }
}

impl LoopbackSse {
    async fn start(failure: Failure) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stalled_connection_closed = Arc::new(Notify::new());
        let closed = Arc::clone(&stalled_connection_closed);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let captured = Arc::clone(&captured);
                        let closed = Arc::clone(&closed);
                        connections.spawn(async move {
                            serve_response(socket, failure, captured, closed).await;
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.unwrap().expect("loopback connection task");
                    }
                }
            }
        });
        Self {
            base_url,
            requests,
            stalled_connection_closed,
            task,
        }
    }
}

async fn serve_response(
    socket: tokio::net::TcpStream,
    failure: Failure,
    captured: Arc<StdMutex<Vec<Value>>>,
    closed: Arc<Notify>,
) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(line.trim(), "POST /v1/chat/completions HTTP/1.1");
    let mut content_length = None;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let length = content_length.expect("request body length");
    assert!(length <= 1024 * 1024);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    let is_second_turn = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "user" && message["content"].to_string().contains(SECOND_USER)
        });
    let call = {
        let mut requests = captured.lock().unwrap();
        requests.push(request.clone());
        requests.len()
    };
    let text = if is_second_turn {
        NEXT_ANSWER.to_string()
    } else {
        format!("partial-{call}")
    };
    let frame = json!({
        "id": "loopback-sse", "object": "chat.completion.chunk", "model": request["model"],
        "choices": [{"index": 0, "delta": {"content": text},
            "finish_reason": if is_second_turn { Some("stop") } else { None }}]
    });
    let response = if is_second_turn {
        // OpenAI-compatible streaming usage arrives in a final choices-empty
        // frame. Keep the fixture token-only so it proves event separation
        // without retaining user prompt text anywhere beyond the test request.
        let usage = json!({
            "id": "loopback-sse", "object": "chat.completion.chunk", "model": request["model"],
            "choices": [],
            "usage": {
                "prompt_tokens": FIXTURE_INPUT_TOKENS,
                "completion_tokens": FIXTURE_OUTPUT_TOKENS,
            },
        });
        format!("data: {frame}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
    } else {
        format!("data: {frame}\n\n")
    };
    // Declaring an unmet length makes a socket close a real reqwest decode
    // error, rather than a clean EOF or a canned ModelClient error string.
    let declared_length = if is_second_turn {
        response.len()
    } else {
        1024 * 1024
    };
    let mut socket = reader.into_inner();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n{response}").as_bytes()).await.unwrap();
    if !is_second_turn && matches!(failure, Failure::StalledBody) {
        // Do not close or expire the response: cancellation must release the
        // client connection before any production stream timeout elapses.
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        closed.notify_one();
    } else {
        socket.shutdown().await.unwrap();
    }
}

async fn next_event(handle: &EngineHandle) -> Event {
    handle
        .rx_event
        .write()
        .await
        .recv()
        .await
        .expect("engine event stream remains open")
}

async fn finish_turn(handle: &EngineHandle, events: &mut Vec<Event>) {
    tokio::time::timeout(model_turn_event_timeout(), async {
        loop {
            let event = next_event(handle).await;
            let terminal = matches!(event, Event::TurnComplete { .. });
            events.push(event);
            if terminal {
                break;
            }
        }
    })
    .await
    .expect("same engine must settle the turn");
}

async fn send_user(handle: &EngineHandle, config: &Config, content: &str) {
    let mut op = external_user_message_op(content, AppMode::Agent, config);
    if let Op::SendMessage(TurnSpec { allow_shell, .. }) = &mut op {
        *allow_shell = false;
    }
    tokio::time::timeout(CONTROL_TIMEOUT, handle.send(op))
        .await
        .expect("same engine mailbox must accept the next user turn")
        .unwrap();
}

fn terminal_status(events: &[Event]) -> TurnOutcomeStatus {
    match events.last().unwrap() {
        Event::TurnComplete { status, .. } => *status,
        event => panic!("expected terminal TurnComplete, got {event:?}"),
    }
}

fn terminal_diagnostics(events: &[Event]) -> &crate::tool_inspection::TurnStopDiagnostics {
    events
        .iter()
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => snapshot.terminal.as_ref(),
            _ => None,
        })
        .expect("terminal request diagnostics")
}

fn retry_status_count(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Status { message } if message.starts_with("Retry attempt: stream-resume ")))
        .count()
}

async fn verify_next_user_turn_after_loss(failure: Failure) {
    let server = LoopbackSse::start(failure).await;
    let workspace = tempdir().unwrap();
    let config = {
        let __cw_config_init_0_0 = Some("custom".to_string());
        let __cw_config_init_0_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
    .with_legacy_root(
        Some("synthetic-loopback-key".to_string()),
        Some(server.base_url.clone()),
    );
    let (engine, handle) = Engine::new(
        EngineConfig {
            max_steps: 1,
            terminal_chrome_enabled: true,
            session_id: Some(SESSION_ID.to_string()),
            ..deterministic_engine_config(workspace.path())
        },
        &config,
    );
    let task = tokio::spawn(engine.run());
    send_user(&handle, &config, FIRST_USER).await;
    let mut first = Vec::new();
    if matches!(failure, Failure::StalledBody) {
        tokio::time::timeout(model_turn_event_timeout(), async {
            loop {
                let event = next_event(&handle).await;
                let partial =
                    matches!(&event, Event::MessageDelta { content, .. } if content == "partial-1");
                first.push(event);
                if partial {
                    break;
                }
            }
        })
        .await
        .expect("the stalled stream must deliver its partial response");
        handle.cancel();
        tokio::time::timeout(CONTROL_TIMEOUT, finish_turn(&handle, &mut first))
            .await
            .expect("cancel must interrupt an open response without waiting for its idle timeout");
        tokio::time::timeout(CONTROL_TIMEOUT, server.stalled_connection_closed.notified())
            .await
            .expect("cancel must release the HTTP response connection");
        assert_eq!(terminal_status(&first), TurnOutcomeStatus::Interrupted);
    } else {
        finish_turn(&handle, &mut first).await;
        assert_eq!(terminal_status(&first), TurnOutcomeStatus::Failed);
        assert!(
            first
                .iter()
                .any(|event| matches!(event, Event::Error { envelope, .. }
            if envelope.category == codewhale_error_taxonomy::ErrorCategory::Network
                && envelope.message.contains("error decoding response body")))
        );
    }
    let partial_count = match failure {
        Failure::TruncatedBody => {
            usize::try_from(super::super::DEFAULT_STREAM_MAX_RESUMES).unwrap() + 1
        }
        Failure::StalledBody => 1,
    };
    assert_eq!(
        server.requests.lock().unwrap().len(),
        partial_count,
        "the failed/cancelled turn must settle before the new user turn; no healthy same-turn retry"
    );
    let first_terminal = terminal_diagnostics(&first);
    assert_eq!(
        usize::try_from(first_terminal.model_requests_started).unwrap(),
        partial_count,
        "terminal parent-request count must match POSTs observed by the loopback"
    );
    let expected_resumes = match failure {
        Failure::TruncatedBody => super::super::DEFAULT_STREAM_MAX_RESUMES,
        Failure::StalledBody => 0,
    };
    assert_eq!(first_terminal.stream_resumes, expected_resumes);
    assert_eq!(first_terminal.transparent_stream_retries, 0);
    assert_eq!(
        retry_status_count(&first),
        expected_resumes as usize,
        "each admitted resume keeps a receipt; the next user turn must start with none"
    );
    assert!(
        !first
            .iter()
            .any(|event| matches!(event, Event::TurnUsage { .. })),
        "a stream without a provider usage frame must not fabricate token usage"
    );

    // Submit the NEXT real user message immediately after terminal settlement,
    // using the original handle. There is no reconstruction or --continue.
    send_user(&handle, &config, SECOND_USER).await;
    let mut second = Vec::new();
    finish_turn(&handle, &mut second).await;
    assert_eq!(terminal_status(&second), TurnOutcomeStatus::Completed);
    assert!(matches!(
        second.last(),
        Some(Event::TurnComplete { error: None, .. })
    ));
    assert!(second.iter().any(
        |event| matches!(event, Event::MessageDelta { content, .. } if content == NEXT_ANSWER)
    ));
    let second_terminal = terminal_diagnostics(&second);
    assert_eq!(second_terminal.model_requests_started, 1);
    assert_eq!(second_terminal.stream_resumes, 0);
    assert_eq!(second_terminal.transparent_stream_retries, 0);
    assert_eq!(retry_status_count(&second), 0);
    let usage_receipts = second
        .iter()
        .filter_map(|event| match event {
            Event::TurnUsage { usage, .. } => Some(usage),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(usage_receipts.len(), 1);
    assert_eq!(usage_receipts[0].input_tokens, FIXTURE_INPUT_TOKENS);
    assert_eq!(usage_receipts[0].output_tokens, FIXTURE_OUTPUT_TOKENS);

    for event in first.iter().chain(&second) {
        assert!(
            !matches!(
                event,
                Event::ApprovalRequired { .. }
                    | Event::ElevationRequired { .. }
                    | Event::UserInputRequired { .. }
                    | Event::UserInputClosed { .. }
            ),
            "this regression has no pending approval or user-input gate: {event:?}"
        );
        if let Event::SessionUpdated { session_id, .. } = event {
            assert_eq!(session_id, SESSION_ID);
        }
    }
    let turn_ids: Vec<_> = first
        .iter()
        .chain(&second)
        .filter_map(|event| match event {
            Event::TurnStarted { turn_id, .. } => Some(turn_id),
            _ => None,
        })
        .collect();
    assert_eq!(turn_ids.len(), 2);
    assert_ne!(
        turn_ids[0], turn_ids[1],
        "second completion must be a distinct user turn"
    );
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), partial_count + 1);
    assert_eq!(
        requests.len() - partial_count,
        1,
        "the clean second user turn must issue exactly one loopback POST"
    );
    let replay = &requests.last().unwrap()["messages"];
    let replay_text = replay.to_string();
    for fragment in [FIRST_USER.to_string(), SECOND_USER.to_string()]
        .into_iter()
        .chain((1..=partial_count).map(|index| format!("partial-{index}")))
    {
        assert_eq!(
            replay_text.matches(&fragment).count(),
            1,
            "missing or duplicated history fragment {fragment}: {replay}"
        );
    }
    assert_eq!(
        replay
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .count(),
        2
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: Arc::new(StdMutex::new(Some(tx))),
        })
        .await
        .unwrap();
    let snapshot = tokio::time::timeout(CONTROL_TIMEOUT, rx)
        .await
        .expect("session snapshot stays responsive")
        .unwrap();
    let persisted = serde_json::to_string(&snapshot.messages).unwrap();
    for fragment in [FIRST_USER, SECOND_USER, NEXT_ANSWER] {
        assert_eq!(persisted.matches(fragment).count(), 1);
    }
    for index in 1..=partial_count {
        assert_eq!(persisted.matches(&format!("partial-{index}")).count(), 1);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetProviderRuntimeStatus {
            tx: Arc::new(StdMutex::new(Some(tx))),
        })
        .await
        .unwrap();
    let readiness = tokio::time::timeout(CONTROL_TIMEOUT, rx)
        .await
        .expect("provider readiness stays responsive")
        .unwrap();
    assert_eq!(
        readiness.active_provider_requests, 0,
        "no stream request permit may leak into idle state"
    );
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(CONTROL_TIMEOUT, task)
        .await
        .expect("shutdown after loss must not block")
        .unwrap();
    assert!(
        !server.task.is_finished(),
        "loopback server must not have panicked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_partial_sse_loss_accepts_next_user_turn_on_same_engine() {
    verify_next_user_turn_after_loss(Failure::TruncatedBody).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_partial_sse_releases_connection_and_accepts_next_user_turn() {
    verify_next_user_turn_after_loss(Failure::StalledBody).await;
}

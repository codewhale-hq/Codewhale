use crate::config_rpc::process_app_request;
use crate::turns::{NativeTurn, handle_prompt_request, handle_thread_request, run_native_turn_for};
use crate::{
    AppState, AppTransport, ConfigGetParams, ConfigSetParams, DispatchError, InFlightTurn, RpcCall,
    RuntimeOwnerRequest, RuntimeThreadHint, ShutdownAuthority, StdioDispatchResult, StdioLoopExit,
    StdioLoopPolicy, ThreadIdParams, ThreadInterruptParams, ThreadMessageParams,
    TurnExecutionOwner, TurnRegistry, daemon_socket, jsonrpc_error, jsonrpc_result,
    legacy_deepseek_compat, params_or_object, parse_params, thread_control,
};
use anyhow::Context as _;
use anyhow::Result;
use codewhale_app_server_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES;
use codewhale_protocol::{
    AppRequest, PromptRequest, ThreadGoalClearParams, ThreadGoalGetParams, ThreadGoalSetParams,
    ThreadRequest, ThreadResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub use codewhale_util::bounded_lines::{BoundedLines, FrameRefusal, is_frame_refusal};

const _: () =
    assert!(codewhale_util::bounded_lines::MAX_FRAME_BYTES == MAX_RUNTIME_IMAGE_BODY_BYTES);

/// The stdio JSON-RPC loop, generic over its transport so it can be driven by
/// a duplex pipe in tests rather than the process's real stdin/stdout.
pub(crate) async fn run_stdio_loop<R, W, C>(
    state: &AppState,
    mut reader: BoundedLines<R>,
    mut writer: W,
    policy: StdioLoopPolicy,
    // Dropped the moment input closes, not when the in-flight turn ends. The
    // socket transport passes its owner claim here: a `thread/message` can run
    // for minutes, and an owner who disconnects mid-turn must not keep the
    // daemon claimed for the rest of it, or a relaunched client is locked out
    // with `daemon_already_claimed` and cannot even shut the daemon down.
    // Process stdio has no claim and passes `None`.
    mut input_claim: Option<C>,
) -> Result<StdioLoopExit>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    C: Send,
{
    // Work that arrived while a turn was streaming. The turn owns the writer
    // for its whole duration, so these wait for it rather than interleaving
    // into the middle of a response.
    let mut pending: VecDeque<(PendingStdioWork, usize)> = VecDeque::new();
    let mut pending_bytes = 0usize;
    let mut stdin_open = true;
    let mut connection = crate::connection::ConnectionSession::default();

    loop {
        let next = pending.pop_front().map(|(work, bytes)| {
            pending_bytes -= bytes;
            work
        });
        let request = match next {
            Some(PendingStdioWork::Response(response)) => {
                write_stdio_line(&mut writer, &response).await?;
                continue;
            }
            Some(PendingStdioWork::Request(request)) => request,
            None => {
                if !stdin_open {
                    return Ok(StdioLoopExit::InputClosed);
                }
                let Some(line) = reader.next_line().await? else {
                    return Ok(StdioLoopExit::InputClosed);
                };
                match parse_stdio_line(&line) {
                    ParsedStdioLine::Blank => continue,
                    ParsedStdioLine::Ignored | ParsedStdioLine::Response(_) => continue,
                    ParsedStdioLine::Request(request) => request,
                }
            }
        };

        if is_control_detach(&request) {
            drop(input_claim.take());
            return Ok(StdioLoopExit::InputClosed);
        }
        let Some(id) = request.id.clone() else {
            if request.method == "initialized" {
                tracing::debug!("control initialized notification received");
            }
            continue;
        };
        if request.method == "initialize" {
            let response = match connection.initialize(request.params) {
                Ok(result) => {
                    tracing::debug!(connection = %connection.id(), "control connection initialized");
                    jsonrpc_result(id, result)
                }
                Err(error) => jsonrpc_error(id, error),
            };
            write_stdio_line(&mut writer, &response).await?;
            continue;
        }
        if !connection.initialized() {
            write_stdio_line(
                &mut writer,
                &jsonrpc_error(id, DispatchError::invalid_request("Not initialized")),
            )
            .await?;
            continue;
        }
        if request.method == "shutdown" && policy.shutdown == ShutdownAuthority::Denied {
            write_stdio_line(
                &mut writer,
                &jsonrpc_error(id, DispatchError::not_daemon_owner()),
            )
            .await?;
            continue;
        }
        let mut input_error: Option<anyhow::Error> = None;
        let native_request = matches!(
            request.method.as_str(),
            "thread/message" | "prompt/request" | "prompt/run"
        ) || (request.method == "thread/request"
            && request.params.get("kind").and_then(Value::as_str) == Some("message"));
        let dispatched = if native_request {
            // A turn can run for minutes. Keep reading stdin while it streams
            // so an interrupt (or a shutdown) can actually reach it — with a
            // plain `await` here, nothing could be read until it finished.
            let cancellation = CancellationToken::new();
            let _cancel_on_drop = cancellation.clone().drop_guard();
            let dispatch = dispatch_stdio_request_with_writer(
                state,
                &mut writer,
                &request.method,
                request.params,
                policy.transport,
                Some(cancellation.clone()),
                Some(connection.id()),
            );
            tokio::pin!(dispatch);
            loop {
                tokio::select! {
                    outcome = &mut dispatch => break outcome,
                    line = reader.next_line(), if stdin_open => {
                        match line {
                            Err(error) => {
                                stdin_open = false;
                                drop(input_claim.take());
                                cancellation.cancel();
                                input_error = Some(error.into());
                            }
                            Ok(None) => {
                                stdin_open = false;
                                // Release the claim here, not after `dispatch`
                                // resolves.
                                drop(input_claim.take());
                                if policy.transport == AppTransport::Socket {
                                    cancellation.cancel();
                                }
                            }
                            Ok(Some(line)) => {
                                if matches!(parse_stdio_line(&line), ParsedStdioLine::Request(ref request) if is_control_detach(request)) {
                                    stdin_open=false; drop(input_claim.take());
                                } else if let Err(error) = handle_line_during_turn(state, &line, &mut pending, &mut pending_bytes, policy, Some(connection.id())).await {
                                    stdin_open = false;
                                    drop(input_claim.take());
                                    cancellation.cancel();
                                    input_error = Some(error);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            dispatch_stdio_request_with_writer(
                state,
                &mut writer,
                &request.method,
                request.params,
                policy.transport,
                None,
                Some(connection.id()),
            )
            .await
        };

        if let Some(error) = input_error {
            if let Err(dispatch_error) = &dispatched {
                let diagnostic = error.to_string();
                return Err(error.context(format!(
                    "control input failed: {diagnostic}; native request cleanup: {}",
                    dispatch_error.wire.message
                )));
            }
            return Err(error);
        }
        match dispatched {
            Ok(dispatch) => {
                write_stdio_line(&mut writer, &jsonrpc_result(id, dispatch.result)).await?;
                if dispatch.should_exit {
                    return Ok(StdioLoopExit::Shutdown);
                }
            }
            Err(mut err) => {
                if let Some(error) = err.transport_error.take() {
                    return Err(error);
                }
                write_stdio_line(&mut writer, &jsonrpc_error(id, err)).await?;
            }
        }
    }
}

/// Work deferred until a streaming turn releases the writer.
pub(crate) enum PendingStdioWork {
    /// Already answered (an interrupt acted immediately); just needs writing.
    Response(Value),
    /// Not started yet; runs normally once the turn is done.
    Request(RpcCall),
}

/// Fixed retained queue limits; exhaustion is a visible connection refusal
/// before deferred work is admitted. Already-running effects are never replayed.
pub(crate) fn queue_stdio_work(
    pending: &mut VecDeque<(PendingStdioWork, usize)>,
    bytes: &mut usize,
    work: PendingStdioWork,
    input_bytes: usize,
) -> Result<()> {
    let retained = input_bytes
        .checked_add(1024)
        .context("control queue size overflow")?;
    anyhow::ensure!(
        pending.len() < 64 && retained <= MAX_RUNTIME_IMAGE_BODY_BYTES.saturating_sub(*bytes),
        "control queue limit exceeded; deferred request not admitted, in-flight outcomes may be uncertain"
    );
    *bytes += retained;
    pending.push_back((work, retained));
    Ok(())
}

pub(crate) enum ParsedStdioLine {
    Blank,
    Request(RpcCall),
    Ignored,
    Response(Value),
}

pub(crate) fn parse_stdio_line(line: &str) -> ParsedStdioLine {
    if line.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
        return ParsedStdioLine::Ignored;
    }
    if line.trim().is_empty() {
        return ParsedStdioLine::Blank;
    }
    let message: codewhale_app_server_protocol::rpc::RpcMessage = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => {
            tracing::debug!(%error, "discarding malformed control RPC message");
            return ParsedStdioLine::Ignored;
        }
    };
    match message {
        codewhale_app_server_protocol::rpc::RpcMessage::Response(response) => {
            ParsedStdioLine::Response(json!(response))
        }
        codewhale_app_server_protocol::rpc::RpcMessage::Error(response) => {
            ParsedStdioLine::Response(json!(response))
        }
        message => ParsedStdioLine::Request(message.into_call().expect("call variant")),
    }
}

/// Triage a request that arrived mid-turn.
///
/// Cancellation is the whole point of reading here, so `thread/interrupt`
/// runs immediately and only its reply waits for the writer. `shutdown` also
/// interrupts immediately — otherwise it would block on the bridge mutex the
/// turn is holding — and then queues so the turn can unwind first. Everything
/// else simply queues: it was never urgent, and running it now would race the
/// turn for the writer.
pub(crate) async fn handle_line_during_turn(
    state: &AppState,
    line: &str,
    pending: &mut VecDeque<(PendingStdioWork, usize)>,
    pending_bytes: &mut usize,
    policy: StdioLoopPolicy,
    connection: Option<crate::connection::ConnectionId>,
) -> Result<()> {
    let request = match parse_stdio_line(line) {
        ParsedStdioLine::Blank => return Ok(()),
        ParsedStdioLine::Ignored | ParsedStdioLine::Response(_) => return Ok(()),
        ParsedStdioLine::Request(request) => request,
    };

    let Some(id) = request.id.clone() else {
        return Ok(());
    };

    match request.method.as_str() {
        "thread/interrupt" => {
            let response = match parse_params::<ThreadInterruptParams>(params_or_object(
                request.params.clone(),
            )) {
                Ok(parsed) => {
                    match interrupt_stdio_turn(state, &parsed.thread_id, connection).await {
                        Ok(interrupted) => jsonrpc_result(
                            id,
                            json!({ "thread_id": parsed.thread_id, "interrupted": interrupted }),
                        ),
                        Err(err) => jsonrpc_error(id, err),
                    }
                }
                Err(err) => jsonrpc_error(id, err),
            };
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(response),
                line.len(),
            )?;
        }
        "shutdown" if policy.shutdown == ShutdownAuthority::Denied => {
            // A non-owner may not even interrupt the live turns: that is the
            // first half of what shutdown does.
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(jsonrpc_error(id, DispatchError::not_daemon_owner())),
                line.len(),
            )?;
        }
        "shutdown" => {
            let _ = interrupt_all_stdio_turns(state).await;
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Request(request),
                line.len(),
            )?;
        }
        _ => queue_stdio_work(
            pending,
            pending_bytes,
            PendingStdioWork::Request(request),
            line.len(),
        )?,
    }
    Ok(())
}

pub(crate) fn is_control_detach(request: &RpcCall) -> bool {
    request.id.is_none()
        && request.method == "daemon/input_closed"
        && request
            .params
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
}

#[derive(Default)]
struct ControlFrameBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl std::io::Write for ControlFrameBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_RUNTIME_IMAGE_BODY_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                FrameRefusal::TooLarge,
            ));
        }
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .min(MAX_RUNTIME_IMAGE_BODY_BYTES);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn serialize_control_frame(value: &Value) -> std::io::Result<Vec<u8>> {
    let mut output = ControlFrameBuffer::default();
    if let Err(error) = serde_json::to_writer(&mut output, value) {
        return Err(if output.exceeded {
            std::io::Error::new(std::io::ErrorKind::InvalidData, FrameRefusal::TooLarge)
        } else {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error)
        });
    }
    Ok(output.bytes)
}

#[derive(Debug, thiserror::Error)]
#[error("control transport write failed; pending outcomes may be uncertain: {0}")]
struct ControlWriteFailure(#[source] std::io::Error);

pub fn is_control_write_failure(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|source| source.is::<ControlWriteFailure>())
}

pub async fn write_control_line<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    body: &[u8],
) -> Result<()> {
    if body.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
        return Err(
            std::io::Error::new(std::io::ErrorKind::InvalidData, FrameRefusal::TooLarge).into(),
        );
    }
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        writer.write_all(body).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
    .unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "control transport write did not settle within 30 seconds",
        ))
    });
    result.map_err(|error| ControlWriteFailure(error).into())
}

pub(crate) async fn write_stdio_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Value,
) -> Result<()> {
    let frame = serialize_control_frame(response)?;
    write_control_line(writer, &frame).await
}

pub(crate) async fn handle_stdio_thread_message<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    parsed: ThreadMessageParams,
    cancellation: Option<CancellationToken>,
    connection: Option<crate::connection::ConnectionId>,
) -> std::result::Result<Value, DispatchError> {
    let mut result = run_native_turn_for(
        state,
        writer,
        NativeTurn {
            allowed_tools: parsed.allowed_tools.as_deref(),
            max_output_tokens: parsed.max_output_tokens,
            thread_key: &parsed.thread_id,
            input: &parsed.input,
            images: &parsed.images,
            model_override: None,
            interruptible: true,
            ephemeral: false,
            cancellation,
        },
        None,
        connection,
    )
    .await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("thread_id".to_string(), Value::String(parsed.thread_id));
    }
    Ok(result)
}

/// Resuming, forking, archiving or unarchiving a thread the runtime reports
/// as `missing` must fail with a named not-found error. Recording the null model/workspace of that
/// response as a stdio hint would clobber any previously cached hint for
/// the same thread id (#5171).
pub(crate) fn ensure_thread_found(
    response: &ThreadResponse,
) -> std::result::Result<(), DispatchError> {
    if response.status == "missing" {
        return Err(DispatchError::thread_not_found(&response.thread_id));
    }
    Ok(())
}

pub(crate) async fn record_stdio_thread_hint(state: &AppState, response: &ThreadResponse) {
    let mut hints = state.stdio_thread_hints.lock().await;
    hints.insert(
        response.thread_id.clone(),
        RuntimeThreadHint {
            model: response.model.clone(),
            workspace: response.cwd.clone(),
        },
    );
}

///
/// Split from [`interrupt_stdio_turn`] so teardown paths can run many
/// concurrently (#6211 R8b) — each future owns its snapshot and never holds
/// the turn registry across the request.
pub(crate) async fn interrupt_turn_request(
    turn: &InFlightTurn,
) -> std::result::Result<bool, DispatchError> {
    match &turn.owner {
        TurnExecutionOwner::Native(owner) => {
            let interrupted = match tokio::time::timeout(
                Duration::from_secs(10),
                owner.execute(RuntimeOwnerRequest::InterruptTurn {
                    thread_id: turn.runtime_thread_id.clone(),
                    turn_id: turn.turn_id.clone(),
                }),
            )
            .await
            {
                Ok(result) => result
                    .map_err(|error| DispatchError::internal(format!("interrupt failed: {error}"))),
                Err(error) => Err(DispatchError::internal(format!(
                    "interrupt outcome uncertain: {error}"
                ))),
            };
            match interrupted {
                Ok(_) => Ok(true),
                Err(error) => {
                    let detail = crate::owner_calls::execute(
                        owner,
                        RuntimeOwnerRequest::GetThread {
                            thread_id: turn.runtime_thread_id.clone(),
                        },
                    )
                    .await;
                    let terminal = detail.as_ref().ok().is_some_and(|detail| {
                        detail.pointer("/thread/id").and_then(Value::as_str)
                            == Some(turn.runtime_thread_id.as_str())
                            && detail
                                .get("turns")
                                .and_then(Value::as_array)
                                .is_some_and(|turns| {
                                    turns.iter().any(|record| {
                                        record.get("id").and_then(Value::as_str)
                                            == Some(turn.turn_id.as_str())
                                            && record.get("thread_id").and_then(Value::as_str)
                                                == Some(turn.runtime_thread_id.as_str())
                                            && matches!(
                                                record.get("status").and_then(Value::as_str),
                                                Some(
                                                    "completed"
                                                        | "failed"
                                                        | "interrupted"
                                                        | "canceled"
                                                )
                                            )
                                    })
                                })
                    });
                    if terminal {
                        Ok(false)
                    } else {
                        Err(DispatchError::internal(format!(
                            "{}; exact native terminal is unconfirmed for thread {}, turn {}",
                            error.wire.message, turn.runtime_thread_id, turn.turn_id
                        )))
                    }
                }
            }
        }
        #[cfg(test)]
        TurnExecutionOwner::HistoricalHttp {
            base_url,
            auth_token,
        } => {
            let mut request = codewhale_release::platform_http_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|err| DispatchError::internal(err.to_string()))?
                .post(format!(
                    "{}/v1/threads/{}/turns/{}/interrupt",
                    base_url, turn.runtime_thread_id, turn.turn_id
                ));
            if let Some(token) = auth_token.as_deref() {
                request = request.bearer_auth(token);
            }
            request
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|err| DispatchError::internal(format!("interrupt failed: {err}")))?;
            Ok(true)
        }
    }
}

pub(crate) async fn interrupt_stdio_turn(
    state: &AppState,
    thread_id: &str,
    requester: Option<crate::connection::ConnectionId>,
) -> std::result::Result<bool, DispatchError> {
    let Some(turn) = state.in_flight_turns.lock().await.get(thread_id).cloned() else {
        return Ok(false);
    };
    if let (Some(owner), Some(requester)) = (turn.connection, requester)
        && owner != requester
    {
        return Err(DispatchError::not_turn_owner());
    }
    let interrupted = interrupt_turn_request(&turn).await?;
    retire_turn_registration(&state.in_flight_turns, thread_id, &turn).await;
    Ok(interrupted)
}

pub(crate) async fn retire_turn_registration(
    registry: &TurnRegistry,
    key: &str,
    expected: &InFlightTurn,
) {
    let mut registered = registry.lock().await;
    if registered.get(key).is_some_and(|current| {
        current.runtime_thread_id == expected.runtime_thread_id
            && current.turn_id == expected.turn_id
            && match (&current.owner, &expected.owner) {
                (TurnExecutionOwner::Native(current), TurnExecutionOwner::Native(expected)) => {
                    std::sync::Arc::ptr_eq(current, expected)
                }
                #[cfg(test)]
                (
                    TurnExecutionOwner::HistoricalHttp {
                        base_url: current,
                        auth_token: current_auth,
                    },
                    TurnExecutionOwner::HistoricalHttp {
                        base_url: expected,
                        auth_token: expected_auth,
                    },
                ) => current == expected && current_auth == expected_auth,
                #[cfg(test)]
                _ => false,
            }
    }) {
        registered.remove(key);
    }
}

/// Interrupt every in-flight turn concurrently, reporting how many were
/// reached (#6211 R8b).
///
/// The teardown paths used to await each turn's interrupt in sequence, so
/// their latency grew with the number of live turns, including each
/// interrupt deadline and bounded readback after uncertain outcomes. Each interrupt now owns its snapshot and runs
/// as an independent task; individual failures are ignored exactly as the
/// sequential loop ignored them, and the registry lock is never held across
/// the requests.
pub(crate) async fn interrupt_all_stdio_turns(state: &AppState) -> usize {
    let turns: Vec<(String, InFlightTurn)> = {
        let map = state.in_flight_turns.lock().await;
        map.iter()
            .map(|(key, turn)| (key.clone(), turn.clone()))
            .collect()
    };
    let mut set = tokio::task::JoinSet::new();
    for (key, turn) in turns {
        let registry = state.in_flight_turns.clone();
        set.spawn(async move {
            let interrupted = interrupt_turn_request(&turn).await?;
            retire_turn_registration(&registry, &key, &turn).await;
            Ok::<_, DispatchError>(interrupted)
        });
    }
    let mut interrupted = 0usize;
    while let Some(joined) = set.join_next().await {
        if matches!(joined, Ok(Ok(true))) {
            interrupted += 1;
        }
    }
    interrupted
}

pub(crate) async fn emit_stdio_event<W: AsyncWrite + Unpin>(
    writer: &mut W,
    event: Value,
) -> Result<()> {
    write_stdio_line(writer, &event).await
}

#[cfg(test)]
pub(crate) async fn dispatch_stdio_request(
    state: &AppState,
    method: &str,
    params: Value,
) -> std::result::Result<StdioDispatchResult, DispatchError> {
    let mut sink = tokio::io::sink();
    dispatch_stdio_request_with_writer(
        state,
        &mut sink,
        method,
        params,
        AppTransport::Stdio,
        None,
        None,
    )
    .await
}

pub(crate) async fn dispatch_stdio_app_request(
    state: &AppState,
    request: AppRequest,
    transport: AppTransport,
) -> std::result::Result<StdioDispatchResult, DispatchError> {
    let response = Box::pin(process_app_request(state, request, transport)).await;
    Ok(StdioDispatchResult {
        result: serde_json::to_value(response)
            .map_err(|err| DispatchError::internal(err.to_string()))?,
        should_exit: false,
    })
}

pub(crate) async fn dispatch_stdio_request_with_writer<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    method: &str,
    params: Value,
    transport: AppTransport,
    cancellation: Option<CancellationToken>,
    connection: Option<crate::connection::ConnectionId>,
) -> std::result::Result<StdioDispatchResult, DispatchError> {
    let outcome = match method {
        "healthz" | "app/healthz" => StdioDispatchResult {
            result: json!({
                "status": "ok",
                "service": legacy_deepseek_compat::SERVICE_NAME,
                "transport": transport.label()
            }),
            should_exit: false,
        },
        "capabilities" => {
            let mut methods = vec![
                "healthz",
                "thread/capabilities",
                "thread/request",
                "thread/create",
                "thread/start",
                "thread/resume",
                "thread/fork",
                "thread/list",
                "thread/loaded/list",
                "thread/read",
                "thread/name/set",
                "thread/set_name",
                "thread/goal/get",
                "thread/goal/clear",
                "thread/archive",
                "thread/unarchive",
                "thread/message",
                "thread/interrupt",
                "app/capabilities",
                "app/request",
                "app/config/get",
                "app/config/set",
                "app/config/unset",
                "app/config/list",
                "app/config/reload",
                "app/models",
                "app/thread_loaded_list",
                "prompt/capabilities",
                "prompt/request",
                "prompt/run",
                "shutdown",
            ];
            if transport == AppTransport::Socket {
                // The daemon handshake exists only on the socket transport;
                // stdio/HTTP clients never see it, so the stdio pin is unchanged.
                methods.insert(1, daemon_socket::ATTACH_METHOD);
            }
            StdioDispatchResult {
                result: json!({
                    "transport": transport.label(),
                    "framing": match transport {
                        AppTransport::Stdio => "jsonl",
                        AppTransport::Socket => "websocket-text",
                        AppTransport::Http => "http",
                    },
                    "families": ["thread/*", "app/*", "prompt/*"],
                    "initialization": {"required": true, "method": "initialize", "notification": "initialized"},
                    "turn_image_inputs": true,
                    "interactive_execution": false,
                    "goal_execution": false,
                    "tool_free_turns": true,
                    "full_api_entrypoint": "codewhale app-server --http",
                    "methods": methods,
                }),
                should_exit: false,
            }
        }
        "thread/capabilities" => StdioDispatchResult {
            result: json!({
                "turn_image_inputs": true,
                "interactive_execution": false,
                "goal_execution": false,
                "tool_free_turns": true,
                "full_api_entrypoint": "codewhale app-server --http",
                "methods": [
                    "thread/request",
                    "thread/create",
                    "thread/start",
                    "thread/resume",
                    "thread/fork",
                    "thread/list",
                    "thread/loaded/list",
                    "thread/read",
                    "thread/name/set",
                    "thread/set_name",
                    "thread/goal/get",
                    "thread/goal/clear",
                    "thread/archive",
                    "thread/unarchive",
                    "thread/message",
                    "thread/interrupt"
                ]
            }),
            should_exit: false,
        },
        "thread/request" => {
            let request: ThreadRequest = parse_params(params)?;
            if let ThreadRequest::Message {
                thread_id,
                input,
                allowed_tools,
                images,
                max_output_tokens,
            } = request
            {
                let response = handle_stdio_thread_message(
                    state,
                    writer,
                    ThreadMessageParams {
                        thread_id,
                        input,
                        allowed_tools,
                        images,
                        max_output_tokens,
                    },
                    cancellation.clone(),
                    connection,
                )
                .await?;
                return Ok(StdioDispatchResult {
                    result: response,
                    should_exit: false,
                });
            }
            let should_record_hint = matches!(
                &request,
                ThreadRequest::Create { .. }
                    | ThreadRequest::Start(_)
                    | ThreadRequest::Resume(_)
                    | ThreadRequest::Fork(_)
            );
            let response = handle_thread_request(state, request).await?;
            if should_record_hint {
                record_stdio_thread_hint(state, &response).await;
            }
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/create" => {
            #[derive(Debug, Deserialize)]
            struct CreateParams {
                #[serde(default)]
                metadata: Value,
            }
            let parsed: CreateParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Create {
                    metadata: parsed.metadata,
                },
            )
            .await?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/start" => {
            let params: codewhale_app_server_protocol::rpc::ThreadStartParams =
                parse_params(params_or_object(params))?;
            let request = ThreadRequest::Start(thread_control::start_params(params)?);
            let response = handle_thread_request(state, request).await?;
            record_stdio_thread_hint(state, &response).await;
            let wire = thread_control::start_wire(response)
                .map_err(|err| DispatchError::internal(format!("{err:#}")))?;
            StdioDispatchResult {
                result: serde_json::to_value(wire)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/resume" => {
            let request = ThreadRequest::Resume(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            ensure_thread_found(&response)?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/fork" => {
            let request = ThreadRequest::Fork(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            ensure_thread_found(&response)?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/list" => {
            let request = ThreadRequest::List(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(thread_control::list_wire(response))
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/loaded/list" => {
            let params: codewhale_app_server_protocol::rpc::ThreadLoadedListParams =
                parse_params(params_or_object(params))?;
            let limit = params.limit.map(usize::try_from).transpose().map_err(|_| {
                DispatchError::invalid_params("loaded thread limit exceeds platform size")
            })?;
            let cursor = params
                .cursor
                .as_deref()
                .map(|cursor| {
                    cursor
                        .strip_prefix("loaded-v1:")
                        .filter(|id| id.len() <= 1024 && !id.chars().any(char::is_control))
                        .ok_or_else(|| {
                            DispatchError::invalid_params("invalid loaded thread cursor")
                        })
                })
                .transpose()?;
            let loaded = thread_control::loaded(state)
                .await
                .map_err(|error| thread_control::rpc_error(error, None))?;
            let start = cursor.filter(|id| !id.is_empty()).map_or(0, |cursor| {
                loaded
                    .threads
                    .partition_point(|thread| thread.id.as_str() <= cursor)
            });
            let remaining = &loaded.threads[start..];
            let count = limit.unwrap_or(remaining.len()).max(1).min(remaining.len());
            let data = remaining[..count]
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>();
            let next_cursor = if count < remaining.len() {
                Some(data.last().map_or_else(
                    || {
                        params
                            .cursor
                            .clone()
                            .unwrap_or_else(|| "loaded-v1:".to_owned())
                    },
                    |id| format!("loaded-v1:{id}"),
                ))
            } else {
                None
            };
            StdioDispatchResult {
                result: serde_json::to_value(
                    codewhale_app_server_protocol::rpc::ThreadLoadedListResponse {
                        data,
                        next_cursor,
                    },
                )
                .map_err(|error| DispatchError::internal(error.to_string()))?,
                should_exit: false,
            }
        }
        "thread/read" => {
            let params: codewhale_app_server_protocol::rpc::ThreadReadParams =
                parse_params(params_or_object(params))?;
            let include_turns = params.include_turns;
            let request = ThreadRequest::Read(codewhale_protocol::ThreadReadParams {
                thread_id: params.thread_id,
            });
            let response = handle_thread_request(state, request).await?;
            let wire = thread_control::read_wire(response, include_turns)
                .map_err(|err| DispatchError::internal(format!("{err:#}")))?;
            StdioDispatchResult {
                result: serde_json::to_value(wire)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/name/set" => {
            let params: codewhale_app_server_protocol::rpc::ThreadSetNameParams =
                parse_params(params_or_object(params))?;
            handle_thread_request(
                state,
                ThreadRequest::SetName(codewhale_protocol::ThreadSetNameParams {
                    thread_id: params.thread_id,
                    name: params.name,
                }),
            )
            .await?;
            StdioDispatchResult {
                result: serde_json::to_value(
                    codewhale_app_server_protocol::rpc::ThreadSetNameResponse::default(),
                )
                .map_err(|error| DispatchError::internal(error.to_string()))?,
                should_exit: false,
            }
        }
        "thread/set_name" | "thread/set-name" => {
            let request = ThreadRequest::SetName(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/set" | "thread/goal_set" | "thread/goal-set" => {
            let request = ThreadRequest::GoalSet(parse_params::<ThreadGoalSetParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/get" | "thread/goal_get" | "thread/goal-get" => {
            let request = ThreadRequest::GoalGet(parse_params::<ThreadGoalGetParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/clear" | "thread/goal_clear" | "thread/goal-clear" => {
            let request = ThreadRequest::GoalClear(parse_params::<ThreadGoalClearParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/archive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Archive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            ensure_thread_found(&response)?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/unarchive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Unarchive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            ensure_thread_found(&response)?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/message" => {
            let parsed: ThreadMessageParams = parse_params(params_or_object(params))?;
            let response = handle_stdio_thread_message(
                state,
                writer,
                parsed,
                cancellation.clone(),
                connection,
            )
            .await?;
            StdioDispatchResult {
                result: response,
                should_exit: false,
            }
        }
        "app/capabilities" => {
            dispatch_stdio_app_request(state, AppRequest::Capabilities, transport).await?
        }
        "app/request" => {
            let request: AppRequest = parse_params(params)?;
            dispatch_stdio_app_request(state, request, transport).await?
        }
        "app/config/get" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(state, AppRequest::ConfigGet { key: parsed.key }, transport)
                .await?
        }
        "app/config/set" => {
            let parsed: ConfigSetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(
                state,
                AppRequest::ConfigSet {
                    key: parsed.key,
                    value: parsed.value,
                },
                transport,
            )
            .await?
        }
        "app/config/unset" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(
                state,
                AppRequest::ConfigUnset { key: parsed.key },
                transport,
            )
            .await?
        }
        "app/config/list" => {
            dispatch_stdio_app_request(state, AppRequest::ConfigList, transport).await?
        }
        "app/config/reload" => {
            dispatch_stdio_app_request(state, AppRequest::ConfigReload, transport).await?
        }
        "app/models" => dispatch_stdio_app_request(state, AppRequest::Models, transport).await?,
        "app/thread_loaded_list" | "app/thread-loaded-list" => {
            dispatch_stdio_app_request(state, AppRequest::ThreadLoadedList, transport).await?
        }
        "prompt/capabilities" => StdioDispatchResult {
            result: json!({
                "interactive_execution": false,
                "goal_execution": false,
                "tool_free_turns": true,
                "full_api_entrypoint": "codewhale app-server --http",
                "methods": ["prompt/request", "prompt/run"]
            }),
            should_exit: false,
        },
        "prompt/request" | "prompt/run" => {
            let request: PromptRequest = parse_params(params)?;
            let response =
                handle_prompt_request(state, writer, request, cancellation.clone()).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| DispatchError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/interrupt" => {
            let parsed: ThreadInterruptParams = parse_params(params_or_object(params))?;
            let interrupted = interrupt_stdio_turn(state, &parsed.thread_id, connection).await?;
            StdioDispatchResult {
                result: json!({
                    "thread_id": parsed.thread_id,
                    "interrupted": interrupted,
                }),
                should_exit: false,
            }
        }
        "shutdown" => {
            // The transport checks shutdown authority before dispatch.
            // Interrupt live turns before returning the flushed shutdown
            // result to its captured host. Keep the shared bridge bound while
            // that owner drains its manager and listeners.
            let _ = interrupt_all_stdio_turns(state).await;
            StdioDispatchResult {
                result: json!({"ok": true, "status": "stopped"}),
                should_exit: true,
            }
        }
        daemon_socket::ATTACH_METHOD if transport == AppTransport::Socket => {
            return Err(DispatchError::already_attached());
        }
        _ => return Err(DispatchError::method_not_found(method)),
    };
    Ok(outcome)
}

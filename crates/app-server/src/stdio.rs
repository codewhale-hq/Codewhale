use crate::config_rpc::process_app_request;
use crate::turns::{BridgedTurn, handle_prompt_request, handle_thread_request, run_bridged_turn};
use crate::{
    AppState, AppTransport, ConfigGetParams, ConfigSetParams, InFlightTurn, JsonRpcError,
    JsonRpcRequest, RuntimeThreadHint, ShutdownAuthority, StdioDispatchResult, StdioLoopExit,
    StdioLoopPolicy, ThreadIdParams, ThreadInterruptParams, ThreadMessageParams, daemon_socket,
    jsonrpc_error, jsonrpc_result, legacy_deepseek_compat, params_or_object, parse_params,
};
use anyhow::Context as _;
use anyhow::Result;
use codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES;
use codewhale_protocol::{
    AppRequest, PromptRequest, ThreadGoalClearParams, ThreadGoalGetParams, ThreadGoalSetParams,
    ThreadRequest, ThreadResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

/// Cancellation-safe, allocation-bounded newline framing shared by every
/// local control transport. Consumed partial bytes remain owned by the reader.
pub struct BoundedLines<R> {
    reader: R,
    pub(crate) pending: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control request exceeds the 8 MiB transport limit",
                    ));
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control frame is not UTF-8",
                    )
                });
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if count > (MAX_RUNTIME_IMAGE_BODY_BYTES + 1).saturating_sub(self.pending.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control request exceeds the 8 MiB transport limit",
                ));
            }
            // Do not let Vec's geometric growth double an almost-full frame.
            self.pending.reserve_exact(count);
            self.pending.extend_from_slice(&available[..count]);
            self.reader.consume(count + usize::from(newline.is_some()));
            if self.pending.len() > MAX_RUNTIME_IMAGE_BODY_BYTES
                && self.pending.last() != Some(&b'\r')
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control request exceeds the 8 MiB transport limit",
                ));
            }
            if newline.is_some() {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control frame is not UTF-8",
                    )
                });
            }
        }
    }
}

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
                    ParsedStdioLine::Rejected(response) => {
                        write_stdio_line(&mut writer, &response).await?;
                        continue;
                    }
                    ParsedStdioLine::Request(request) => request,
                }
            }
        };

        if is_control_detach(&request) {
            drop(input_claim.take());
            return Ok(StdioLoopExit::InputClosed);
        }
        let id = request.id.clone();
        if request.method == "shutdown" && policy.shutdown == ShutdownAuthority::Denied {
            write_stdio_line(
                &mut writer,
                &jsonrpc_error(id, JsonRpcError::not_daemon_owner()),
            )
            .await?;
            continue;
        }
        let dispatched = if request.method == "thread/message" {
            // A turn can run for minutes. Keep reading stdin while it streams
            // so an interrupt (or a shutdown) can actually reach it — with a
            // plain `await` here, nothing could be read until it finished.
            let dispatch = dispatch_stdio_request_with_writer(
                state,
                &mut writer,
                &request.method,
                request.params,
                policy.transport,
            );
            tokio::pin!(dispatch);
            loop {
                tokio::select! {
                    outcome = &mut dispatch => break outcome,
                    line = reader.next_line(), if stdin_open => {
                        match line? {
                            None => {
                                stdin_open = false;
                                // Release the claim here, not after `dispatch`
                                // resolves.
                                drop(input_claim.take());
                            }
                            Some(line) => {
                                if matches!(parse_stdio_line(&line), ParsedStdioLine::Request(ref request) if is_control_detach(request)) {
                                    stdin_open=false; drop(input_claim.take());
                                } else {handle_line_during_turn(state, &line, &mut pending, &mut pending_bytes, policy).await?;}
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
            )
            .await
        };

        match dispatched {
            Ok(dispatch) => {
                write_stdio_line(&mut writer, &jsonrpc_result(id, dispatch.result)).await?;
                if dispatch.should_exit {
                    return Ok(StdioLoopExit::Shutdown);
                }
            }
            Err(err) => {
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
    Request(JsonRpcRequest),
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
    Request(JsonRpcRequest),
    Rejected(Value),
}

pub(crate) fn parse_stdio_line(line: &str) -> ParsedStdioLine {
    if line.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
        return ParsedStdioLine::Rejected(jsonrpc_error(
            None,
            JsonRpcError::invalid_params("request exceeds the 8 MiB transport limit"),
        ));
    }
    if line.trim().is_empty() {
        return ParsedStdioLine::Blank;
    }
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            return ParsedStdioLine::Rejected(jsonrpc_error(
                None,
                JsonRpcError::parse_error(format!("invalid json: {err}")),
            ));
        }
    };
    if request
        .jsonrpc
        .as_deref()
        .is_some_and(|version| version != "2.0")
    {
        return ParsedStdioLine::Rejected(jsonrpc_error(
            request.id,
            JsonRpcError::invalid_request("jsonrpc version must be 2.0"),
        ));
    }
    ParsedStdioLine::Request(request)
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
) -> Result<()> {
    let request = match parse_stdio_line(line) {
        ParsedStdioLine::Blank => return Ok(()),
        ParsedStdioLine::Rejected(response) => {
            return queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(response),
                line.len(),
            );
        }
        ParsedStdioLine::Request(request) => request,
    };

    match request.method.as_str() {
        "thread/interrupt" => {
            let id = request.id.clone();
            let response = match parse_params::<ThreadInterruptParams>(params_or_object(
                request.params.clone(),
            )) {
                Ok(parsed) => match interrupt_stdio_turn(state, &parsed.thread_id).await {
                    Ok(interrupted) => jsonrpc_result(
                        id,
                        json!({ "thread_id": parsed.thread_id, "interrupted": interrupted }),
                    ),
                    Err(err) => jsonrpc_error(id, err),
                },
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
                PendingStdioWork::Response(jsonrpc_error(
                    request.id,
                    JsonRpcError::not_daemon_owner(),
                )),
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

pub(crate) fn is_control_detach(request: &JsonRpcRequest) -> bool {
    request.jsonrpc.as_deref() == Some("2.0")
        && request.id.is_none()
        && request.method == "daemon/input_closed"
        && request
            .params
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
}

pub(crate) async fn write_stdio_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Value,
) -> Result<()> {
    writer.write_all(&serde_json::to_vec(response)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) async fn handle_stdio_thread_message<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    parsed: ThreadMessageParams,
) -> std::result::Result<Value, JsonRpcError> {
    let mut result = run_bridged_turn(
        state,
        writer,
        BridgedTurn {
            allowed_tools: parsed.allowed_tools.as_deref(),
            max_output_tokens: parsed.max_output_tokens,
            thread_key: &parsed.thread_id,
            input: &parsed.input,
            images: &parsed.images,
            model_override: None,
            interruptible: true,
            ephemeral: false,
            require_known_thread: true,
        },
        None,
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
) -> std::result::Result<(), JsonRpcError> {
    if response.status == "missing" {
        return Err(JsonRpcError::thread_not_found(&response.thread_id));
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

/// Ask the runtime to interrupt a turn that is streaming right now.
///
/// Everything this needs was copied out of the bridge when the turn started,
/// so it never touches the bridge mutex the turn is holding. Returns whether
/// a live turn was found for `thread_id`.
/// Interrupt one in-flight turn over HTTP, from an owned snapshot.
///
/// Split from [`interrupt_stdio_turn`] so teardown paths can run many
/// concurrently (#6211 R8b) — each future owns its snapshot and never holds
/// the turn registry across the request.
pub(crate) async fn interrupt_turn_request(
    turn: &InFlightTurn,
) -> std::result::Result<bool, JsonRpcError> {
    let mut request = codewhale_release::platform_http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| JsonRpcError::internal(err.to_string()))?
        .post(format!(
            "{}/v1/threads/{}/turns/{}/interrupt",
            turn.base_url, turn.runtime_thread_id, turn.turn_id
        ));
    if let Some(token) = turn.auth_token.as_deref() {
        request = request.bearer_auth(token);
    }
    request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| JsonRpcError::internal(format!("interrupt failed: {err}")))?;
    Ok(true)
}

pub(crate) async fn interrupt_stdio_turn(
    state: &AppState,
    thread_id: &str,
) -> std::result::Result<bool, JsonRpcError> {
    let Some(turn) = state.in_flight_turns.lock().await.get(thread_id).cloned() else {
        return Ok(false);
    };
    interrupt_turn_request(&turn).await
}

/// Interrupt every in-flight turn concurrently, reporting how many were
/// reached (#6211 R8b).
///
/// The teardown paths used to await each turn's interrupt in sequence, so
/// their latency grew with the number of live turns — up to the 10s
/// per-request timeout apiece. Each interrupt now owns its snapshot and runs
/// as an independent task; individual failures are ignored exactly as the
/// sequential loop ignored them, and the registry lock is never held across
/// the requests.
pub(crate) async fn interrupt_all_stdio_turns(state: &AppState) -> usize {
    let turns: Vec<InFlightTurn> = {
        let map = state.in_flight_turns.lock().await;
        map.values().cloned().collect()
    };
    let mut set = tokio::task::JoinSet::new();
    for turn in turns {
        set.spawn(async move { interrupt_turn_request(&turn).await });
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
    writer.write_all(&serde_json::to_vec(&event)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn dispatch_stdio_request(
    state: &AppState,
    method: &str,
    params: Value,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let mut sink = tokio::io::sink();
    dispatch_stdio_request_with_writer(state, &mut sink, method, params, AppTransport::Stdio).await
}

pub(crate) async fn dispatch_stdio_app_request(
    state: &AppState,
    request: AppRequest,
    transport: AppTransport,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let response = Box::pin(process_app_request(state, request, transport)).await;
    Ok(StdioDispatchResult {
        result: serde_json::to_value(response)
            .map_err(|err| JsonRpcError::internal(err.to_string()))?,
        should_exit: false,
    })
}

pub(crate) async fn dispatch_stdio_request_with_writer<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    method: &str,
    params: Value,
    transport: AppTransport,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
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
                "thread/read",
                "thread/set_name",
                "thread/goal/set",
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
                    "families": ["thread/*", "app/*", "prompt/*"],
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
                    "thread/read",
                    "thread/set_name",
                    "thread/goal/set",
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/start" => {
            let request = ThreadRequest::Start(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/list" => {
            let request = ThreadRequest::List(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/read" => {
            let request = ThreadRequest::Read(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/set_name" | "thread/set-name" => {
            let request = ThreadRequest::SetName(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
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
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/message" => {
            let parsed: ThreadMessageParams = parse_params(params_or_object(params))?;
            let response = handle_stdio_thread_message(state, writer, parsed).await?;
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
            let response = handle_prompt_request(state, writer, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/interrupt" => {
            let parsed: ThreadInterruptParams = parse_params(params_or_object(params))?;
            let interrupted = interrupt_stdio_turn(state, &parsed.thread_id).await?;
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
            return Err(JsonRpcError::already_attached());
        }
        _ => return Err(JsonRpcError::method_not_found(method)),
    };
    Ok(outcome)
}

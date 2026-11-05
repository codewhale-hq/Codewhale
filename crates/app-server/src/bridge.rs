#[cfg(test)]
use crate::stdio::{emit_stdio_event, interrupt_turn_request};
#[cfg(test)]
use crate::turns::{RuntimeTurnInput, extract_runtime_thread_id, turn_terminal_status};
use crate::{AppState, DispatchError, RuntimeBridge, SharedRuntimeBridge, thread_control};
#[cfg(test)]
use crate::{
    InFlightTurn, MAX_SSE_FRAME_BYTES, RuntimeThreadHint, TurnExecutionOwner, TurnRegistry,
    TurnTerminalStatus, TurnTranscript,
};
use anyhow::Context as _;
use anyhow::{Result, anyhow, bail};
#[cfg(test)]
use codewhale_protocol::{EventFrame, ResponseChannel};
use serde_json::{Value, json};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::process::{Command, Stdio};
#[cfg(all(test, unix))]
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use tokio::io::AsyncWrite;
#[cfg(all(test, unix))]
use tokio::sync::Mutex;

/// Historical cold-child cache comparator. It has no production producer.
#[cfg(all(test, unix))]
pub(crate) async fn acquire_historical_runtime_bridge<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<SharedRuntimeBridge, DispatchError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<RuntimeBridge>>,
{
    if let Some(bridge) = state.runtime_bridge.lock().await.as_ref() {
        return Ok(bridge.clone());
    }
    if state.captured_owner.is_some() {
        return Err(DispatchError::runtime_unavailable(
            "the captured Runtime owner is unavailable; refusing another owner",
        ));
    }
    let bridge =
        Arc::new(Mutex::new(start().await.map_err(|err| {
            DispatchError::runtime_unavailable(err.to_string())
        })?));
    let mut slot = state.runtime_bridge.lock().await;
    // Prefer a bridge cached by a concurrent caller while we were spawning;
    // dropping our unused one kills the extra child via `Drop`.
    Ok(slot.get_or_insert_with(|| bridge.clone()).clone())
}

/// Use the exact bridge installed by the held canonical owner. No process,
/// store or transcript can be created when this captured connection is absent.
pub(crate) async fn acquire_runtime_bridge(
    state: &AppState,
) -> std::result::Result<SharedRuntimeBridge, DispatchError> {
    state
        .runtime_bridge
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            DispatchError::runtime_unavailable(
                "the captured Runtime owner is unavailable; refusing another owner",
            )
        })
}

#[cfg(test)]
pub(crate) async fn acquire_live_runtime_bridge(
    state: &AppState,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, DispatchError> {
    Ok(acquire_runtime_bridge(state).await?.lock_owned().await)
}

/// Historical respawn comparison; all actual frontends use the held owner.
#[cfg(all(test, unix))]
pub(crate) async fn acquire_live_runtime_bridge_with<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, DispatchError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<RuntimeBridge>>,
{
    for _ in 0..2 {
        let shared = acquire_historical_runtime_bridge(state, &start).await?;
        let mut bridge = shared.clone().lock_owned().await;
        if !bridge.child_exited() {
            return Ok(bridge);
        }
        drop(bridge);
        let mut slot = state.runtime_bridge.lock().await;
        // Evict only the dead bridge: a concurrent caller may already have
        // replaced it with a live one.
        if slot
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(cached, &shared))
        {
            *slot = None;
        }
    }
    Err(DispatchError::runtime_unavailable(
        "runtime API bridge exited immediately after starting",
    ))
}

/// Historical config/cache comparator. Captured owners retain their bridge.
#[cfg(test)]
pub(crate) async fn invalidate_runtime_bridge(state: &AppState) {
    if state.captured_owner.is_some() {
        return;
    }
    let mut bridge = state.runtime_bridge.lock().await;
    *bridge = None;
}

impl RuntimeBridge {
    /// The child binds an ephemeral loopback port itself (`--port 0`) and
    /// reports it on stdout; the parent never reserves a port for it to race.
    #[cfg(test)]
    pub(crate) fn runtime_command(config_path: Option<&Path>, auth_token: &str) -> Result<Command> {
        let current_exe = std::env::current_exe().ok();
        let mut command = if let Some(path) = current_exe {
            Command::new(path)
        } else {
            Command::new("codewhale")
        };
        // Pass the runtime auth token out-of-band via env (not argv) so local
        // `ps` cannot read credential material from the child command line.
        // The TUI/runtime server already accepts CODEWHALE_RUNTIME_TOKEN /
        // DEEPSEEK_RUNTIME_TOKEN when --auth-token is absent.
        command
            .arg("app-server")
            .arg("--http")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .env("CODEWHALE_RUNTIME_TOKEN", auth_token)
            .env("DEEPSEEK_RUNTIME_TOKEN", auth_token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(config_path) = config_path {
            command.arg("--config").arg(config_path);
        }
        Ok(command)
    }

    pub(crate) fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.auth_token.as_deref() {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    pub(crate) async fn request_json(&self, builder: reqwest::RequestBuilder) -> Result<Value> {
        thread_control::read_json_response(builder.send().await?).await
    }

    /// Resolve `stdio_thread_id` to a runtime thread, minting one only when
    /// `thread_map` has no entry. The map lives on [`AppState`] and outlives
    /// this bridge, so a thread created under a previous child keeps its id
    /// here as long as the store it was persisted to is shared (#6246).
    #[cfg(test)]
    pub(crate) async fn ensure_runtime_thread(
        &mut self,
        thread_map: &mut HashMap<String, String>,
        stdio_thread_id: &str,
        hint: Option<RuntimeThreadHint>,
    ) -> Result<String> {
        if let Some(runtime_thread_id) = thread_map.get(stdio_thread_id) {
            return Ok(runtime_thread_id.clone());
        }
        let hint = hint.unwrap_or_default();
        let runtime_thread_id = self
            .create_runtime_thread(hint.model, hint.workspace)
            .await?;
        thread_map.insert(stdio_thread_id.to_string(), runtime_thread_id.clone());
        Ok(runtime_thread_id)
    }

    #[cfg(test)]
    async fn create_runtime_thread(
        &mut self,
        model: Option<String>,
        workspace: Option<PathBuf>,
    ) -> Result<String> {
        let record = self
            .request_json(
                self.authed(self.client.post(format!("{}/v1/threads", self.base_url)))
                    .json(&json!({
                        "model": model,
                        "workspace": workspace,
                        "mode": "agent",
                        "archived": false,
                    })),
            )
            .await?;
        let thread_id = extract_runtime_thread_id(&record)?.to_string();
        self.last_seq_by_thread
            .entry(thread_id.clone())
            .or_insert(0);
        Ok(thread_id)
    }

    /// Run one turn to completion, streaming its events to `writer`.
    ///
    /// `registration` is `Some` on the stdio path: it publishes the live turn
    /// so an `thread/interrupt` arriving mid-stream can reach the runtime
    /// without waiting on the bridge mutex this call holds.
    #[cfg(test)]
    pub(crate) async fn message_thread<W: AsyncWrite + Unpin>(
        &mut self,
        thread_id: &str,
        input: RuntimeTurnInput<'_>,
        writer: &mut W,
        registration: Option<(TurnRegistry, String)>,
        mut transcript: Option<&mut TurnTranscript>,
    ) -> Result<Value> {
        let RuntimeTurnInput {
            input,
            images,
            max_output_tokens,
            expected_workspace,
            cancellation: _,
        } = input;
        let mut request = json!({ "prompt": input, "allowed_tools": [] });
        if let Some(workspace) = expected_workspace {
            request["expected_workspace"] = json!(workspace);
        }
        if !images.is_empty() {
            let info = self
                .request_json(
                    self.authed(
                        self.client
                            .get(format!("{}/v1/runtime/info", self.base_url)),
                    ),
                )
                .await?;
            if info
                .pointer("/capabilities/turn_image_inputs")
                .and_then(Value::as_bool)
                != Some(true)
            {
                bail!(
                    "Runtime image input is unavailable; update the Runtime before sending attachments"
                );
            }
            request["images"] = json!(images);
        }
        if let Some(limit) = max_output_tokens {
            request["maxOutputTokens"] = json!(limit);
        }
        let turn = self
            .request_json(
                self.authed(
                    self.client
                        .post(format!("{}/v1/threads/{thread_id}/turns", self.base_url)),
                )
                .json(&request),
            )
            .await?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("runtime API turn response missing turn.id"))?
            .to_string();
        let response_id = format!("{thread_id}:{turn_id}");

        if let Some(transcript) = transcript.as_deref_mut() {
            transcript.model = turn
                .pointer("/thread/model")
                .and_then(Value::as_str)
                .map(str::to_string);
            transcript.events.push(EventFrame::ResponseStart {
                response_id: response_id.clone(),
            });
        }

        if let Err(error) = emit_stdio_event(
            writer,
            json!({
                "type": "response_start",
                "response_id": response_id,
            }),
        )
        .await
        {
            let orphan = InFlightTurn {
                owner: TurnExecutionOwner::HistoricalHttp {
                    base_url: self.base_url.clone(),
                    auth_token: self.auth_token.clone(),
                },
                runtime_thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
                connection: None,
            };
            if let Err(interrupt) = interrupt_turn_request(&orphan).await {
                tracing::warn!(
                    thread_id,
                    turn_id = %turn_id,
                    "failed to interrupt a turn whose response_start could not be written: {}",
                    interrupt.wire.message
                );
            }
            return Err(error);
        }

        // Publish the turn only for the streaming window, and take it back
        // before any `?` below: a turn that has already finished must never
        // look cancellable.
        if let Some((registry, key)) = registration.as_ref() {
            registry.lock().await.insert(
                key.clone(),
                InFlightTurn {
                    owner: TurnExecutionOwner::HistoricalHttp {
                        base_url: self.base_url.clone(),
                        auth_token: self.auth_token.clone(),
                    },
                    runtime_thread_id: thread_id.to_string(),
                    turn_id: turn_id.clone(),
                    connection: None,
                },
            );
        }

        let since_seq = self.last_seq_by_thread.get(thread_id).copied().unwrap_or(0);
        let stream_result = self
            .stream_turn_events(
                thread_id,
                &turn_id,
                &response_id,
                writer,
                since_seq,
                transcript.as_deref_mut(),
            )
            .await;

        if let Some((registry, key)) = registration.as_ref() {
            registry.lock().await.remove(key);
        }

        if stream_result.is_ok() {
            emit_stdio_event(
                writer,
                json!({
                    "type": "response_end",
                    "response_id": response_id,
                }),
            )
            .await?;
            if let Some(transcript) = transcript {
                transcript.events.push(EventFrame::ResponseEnd {
                    response_id: response_id.clone(),
                });
            }
        } else {
            // The stream broke before `turn.completed` (transport error,
            // oversized or invalid frame, a writer that went away). Ending
            // the response here reported success ahead of the error, and the
            // runtime turn kept running with nothing left able to interrupt
            // it (the registry entry is gone), so the thread refused its next
            // message until the orphan finished. Stop it, best effort, and
            // let the error below be the only outcome the client sees.
            let orphan = InFlightTurn {
                owner: TurnExecutionOwner::HistoricalHttp {
                    base_url: self.base_url.clone(),
                    auth_token: self.auth_token.clone(),
                },
                runtime_thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
                connection: None,
            };
            if let Err(error) = interrupt_turn_request(&orphan).await {
                tracing::warn!(
                    thread_id,
                    turn_id = %turn_id,
                    "failed to interrupt a turn whose event stream broke: {}",
                    error.wire.message
                );
            }
        }

        let (last_seq, status, error) = stream_result?;
        self.last_seq_by_thread
            .insert(thread_id.to_string(), last_seq);

        match status {
            TurnTerminalStatus::Completed => Ok(json!({
                "thread_id": thread_id,
                "status": "accepted",
                "thread": Value::Null,
                "threads": [],
                "model": Value::Null,
                "model_provider": Value::Null,
                "cwd": Value::Null,
                "approval_policy": Value::Null,
                "sandbox": Value::Null,
                "events": [],
                "data": { "turn_id": turn_id },
            })),
            TurnTerminalStatus::Failed => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn failed".to_string())
            )),
            TurnTerminalStatus::Interrupted => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn interrupted".to_string())
            )),
            TurnTerminalStatus::Canceled => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn canceled".to_string())
            )),
        }
    }

    #[cfg(test)]
    async fn stream_turn_events<W: AsyncWrite + Unpin>(
        &self,
        thread_id: &str,
        turn_id: &str,
        response_id: &str,
        writer: &mut W,
        since_seq: u64,
        mut transcript: Option<&mut TurnTranscript>,
    ) -> Result<(u64, TurnTerminalStatus, Option<String>)> {
        let mut response = self
            .authed(self.client.get(format!(
                "{}/v1/threads/{thread_id}/events?since_seq={since_seq}",
                self.base_url
            )))
            .send()
            .await?
            .error_for_status()?;

        let mut buffer = Vec::new();
        let mut last_seq = since_seq;

        while let Some(chunk) = response.chunk().await? {
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_SSE_FRAME_BYTES {
                bail!(
                    "runtime SSE frame exceeded {MAX_SSE_FRAME_BYTES} bytes without a frame delimiter"
                );
            }
            while let Some(frame_bytes) = take_sse_frame(&mut buffer) {
                let Some((event_name, frame_data)) = parse_sse_frame(&frame_bytes) else {
                    continue;
                };
                let envelope: Value = serde_json::from_str(&frame_data)
                    .with_context(|| format!("invalid SSE json for {event_name}: {frame_data}"))?;
                if let Some(seq) = envelope.get("seq").and_then(Value::as_u64) {
                    last_seq = last_seq.max(seq);
                }
                if envelope.get("turn_id").and_then(Value::as_str) != Some(turn_id) {
                    continue;
                }
                let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
                match event_name.as_str() {
                    "approval.required" | "user_input.required" => {
                        bail!(
                            "interactive_execution_unsupported: Runtime requested interaction during a tool-free compatibility turn; use codewhale app-server --http and /v1/*"
                        );
                    }
                    "item.delta" => {
                        let kind = payload
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if kind == "agent_message"
                            && let Some(delta) = payload.get("delta").and_then(Value::as_str)
                            && !delta.is_empty()
                        {
                            emit_stdio_event(
                                writer,
                                json!({
                                    "type": "response_delta",
                                    "response_id": response_id,
                                    "delta": delta,
                                }),
                            )
                            .await?;
                            if let Some(transcript) = transcript.as_deref_mut() {
                                transcript.text.push_str(delta);
                                transcript.events.push(EventFrame::ResponseDelta {
                                    response_id: response_id.to_string(),
                                    delta: delta.to_string(),
                                    channel: ResponseChannel::Text,
                                });
                            }
                        }
                    }
                    "turn.completed" => {
                        let status = turn_terminal_status(&payload)?;
                        let error = payload
                            .pointer("/turn/error")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        return Ok((last_seq, status, error));
                    }
                    _ => {}
                }
            }
        }

        bail!("runtime event stream ended before turn.completed")
    }

    #[cfg(test)]
    pub(crate) fn from_base_url_for_test(base_url: String) -> Self {
        install_rustls_crypto_provider();
        Self {
            base_url,
            client: codewhale_release::platform_http_client_builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("build reqwest test client"),
            auth_token: None,
            child: None,
            last_seq_by_thread: HashMap::new(),
        }
    }
}

#[cfg(test)]
impl RuntimeBridge {
    /// Whether the historical fixture child has exited. A bridge without a child (tests,
    /// or an externally managed runtime) never reports exited. A `try_wait`
    /// error counts as exited: with `WNOHANG` it only fails when the pid is no
    /// longer this process's child (already reaped elsewhere), and such a
    /// child can neither be tracked nor killed on drop.
    #[cfg(unix)]
    fn child_exited(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
    }

    /// Kills the managed runtime child and reaps it on a detached thread so
    /// neither an explicit shutdown nor Drop blocks a Tokio runtime thread.
    fn shutdown_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
impl Drop for RuntimeBridge {
    fn drop(&mut self) {
        self.shutdown_child();
    }
}

/// The line the Runtime prints once it holds its listener (the Runtime's
/// `RUNTIME_LISTENING_PREFIX`; this crate does not depend on that one).
#[cfg(test)]
pub(crate) const RUNTIME_LISTENING_PREFIX: &str = "Runtime API listening on http://";

/// The endpoint line is short and comes first; anything larger is not it.
#[cfg(test)]
pub(crate) const RUNTIME_READY_MAX_BYTES: usize = 1024;

/// The endpoint a Runtime child reports for itself: exactly a nonzero port on
/// `127.0.0.1`, the host the parent asked it to bind.
#[cfg(test)]
pub(crate) fn parse_runtime_endpoint(line: &str) -> Result<std::net::SocketAddr> {
    let address = line
        .trim_end()
        .strip_prefix(RUNTIME_LISTENING_PREFIX)
        .context("runtime API bridge did not report its endpoint")?;
    let endpoint: std::net::SocketAddr = address
        .parse()
        .context("runtime API bridge reported an invalid endpoint")?;
    if endpoint.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || endpoint.port() == 0
    {
        bail!("runtime API bridge reported an endpoint that is not a loopback port");
    }
    Ok(endpoint)
}

/// Read the first stdout line, bounded.
#[cfg(test)]
pub(crate) fn read_runtime_ready_line(stdout: &mut impl std::io::Read) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stdout.read(&mut byte)? == 0 {
            bail!("runtime API bridge closed stdout before reporting its endpoint");
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= RUNTIME_READY_MAX_BYTES {
            bail!("runtime API bridge sent an oversized readiness line");
        }
        line.push(byte[0]);
    }
    String::from_utf8(line).context("runtime API bridge readiness line is not UTF-8")
}

pub(crate) fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
pub(crate) fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some(buffer.drain(..pos + 4).collect());
    }
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|pos| buffer.drain(..pos + 2).collect())
}

#[cfg(test)]
pub(crate) fn parse_sse_frame(frame_bytes: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8(frame_bytes.to_vec()).ok()?;
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    match (event_name, data_lines.is_empty()) {
        (Some(event), false) => Some((event, data_lines.join("\n"))),
        _ => None,
    }
}

use crate::stdio::{emit_stdio_event, interrupt_turn_request};
use crate::turns::{RuntimeTurnInput, extract_runtime_thread_id, turn_terminal_status};
use crate::{
    AppState, InFlightTurn, JsonRpcError, MAX_SSE_FRAME_BYTES, RuntimeBridge, RuntimeThreadHint,
    SharedRuntimeBridge, TurnRegistry, TurnTerminalStatus, TurnTranscript, thread_control,
};
use anyhow::Context as _;
use anyhow::{Result, anyhow, bail};
use codewhale_protocol::{EventFrame, ResponseChannel};
use serde_json::{Value, json};
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWrite;
#[cfg(all(test, unix))]
use tokio::sync::Mutex;

/// Historical cold-child cache comparator. It has no production producer.
#[cfg(all(test, unix))]
pub(crate) async fn acquire_historical_runtime_bridge<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<SharedRuntimeBridge, JsonRpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<RuntimeBridge>>,
{
    if let Some(bridge) = state.runtime_bridge.lock().await.as_ref() {
        return Ok(bridge.clone());
    }
    if state.captured_owner.is_some() {
        return Err(JsonRpcError::runtime_unavailable(
            "the captured Runtime owner is unavailable; refusing another owner",
        ));
    }
    let bridge =
        Arc::new(Mutex::new(start().await.map_err(|err| {
            JsonRpcError::runtime_unavailable(err.to_string())
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
) -> std::result::Result<SharedRuntimeBridge, JsonRpcError> {
    state
        .runtime_bridge
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            JsonRpcError::runtime_unavailable(
                "the captured Runtime owner is unavailable; refusing another owner",
            )
        })
}

pub(crate) async fn runtime_catalog_models(
    request: reqwest::RequestBuilder,
    provider: &str,
    provider_id: Option<&str>,
) -> Result<Vec<Value>> {
    let (client, request) = request.build_split();
    let request = request?;
    let mut models = Vec::new();
    let mut cursor = None;
    let mut cursors = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    let mut total = None;
    for _ in 0..40 {
        let mut page = request
            .try_clone()
            .context("Runtime catalog request cannot be replayed")?;
        if let Some(cursor) = cursor.as_deref() {
            page.url_mut()
                .query_pairs_mut()
                .append_pair("cursor", cursor);
        }
        let catalog = thread_control::read_json_response(client.execute(page).await?).await?;
        anyhow::ensure!(
            catalog.get("provider").and_then(Value::as_str) == Some(provider)
                && catalog.get("model_provider_id").unwrap_or(&Value::Null) == &json!(provider_id),
            "Runtime model catalog changed provider identity"
        );
        let count = catalog
            .get("total")
            .and_then(Value::as_u64)
            .context("Runtime model catalog omitted its total")?;
        anyhow::ensure!(
            count <= 10_000 && total.is_none_or(|total| total == count),
            "Runtime model catalog size changed or exceeds its limit"
        );
        total = Some(count);
        let entries = catalog
            .get("models")
            .and_then(Value::as_array)
            .context("Runtime model catalog omitted its models")?;
        anyhow::ensure!(
            entries.len() <= 250,
            "Runtime model catalog page exceeds its limit"
        );
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .context("Runtime model catalog contains an invalid model")?;
            anyhow::ensure!(
                ids.insert(id.to_string()),
                "Runtime model catalog repeated a model"
            );
            models.push(entry.clone());
        }
        anyhow::ensure!(
            models.len() as u64 <= count,
            "Runtime model catalog exceeds its total"
        );
        match catalog.get("nextCursor") {
            None | Some(Value::Null) => {
                anyhow::ensure!(
                    models.len() as u64 == count,
                    "Runtime model catalog is incomplete"
                );
                return Ok(models);
            }
            Some(Value::String(next)) if !next.is_empty() && next.len() <= 1_368 => {
                anyhow::ensure!(
                    !entries.is_empty() && models.len() < count as usize,
                    "Runtime model catalog continuation is invalid"
                );
                anyhow::ensure!(
                    cursors.insert(next.clone()),
                    "Runtime model catalog cursor repeated"
                );
                cursor = Some(next.clone());
            }
            _ => bail!("Runtime model catalog cursor is invalid"),
        }
    }
    bail!("Runtime model catalog exceeded its page limit")
}

pub(crate) async fn runtime_app_models(state: &AppState) -> Result<Vec<Value>> {
    let shared = acquire_runtime_bridge(state)
        .await
        .map_err(|error| anyhow!(error.message))?;
    let transport = state
        .runtime_catalog_transport
        .as_ref()
        .context("the captured Runtime catalog transport is unavailable")?;
    let captured = transport
        .bridge
        .upgrade()
        .context("the captured Runtime catalog owner is unavailable")?;
    anyhow::ensure!(
        Arc::ptr_eq(&shared, &captured),
        "the captured Runtime catalog owner changed; refusing another owner"
    );
    let base_url = &transport.base_url;
    let client = &transport.client;
    let authed = |request: reqwest::RequestBuilder| match transport.auth_token.as_deref() {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    let providers = thread_control::read_json_response(
        authed(client.get(format!("{base_url}/v1/providers")))
            .timeout(Duration::from_secs(10))
            .send()
            .await?,
    )
    .await?;
    let providers = providers
        .get("providers")
        .and_then(Value::as_array)
        .context("Runtime provider catalog is invalid")?;
    anyhow::ensure!(
        providers.len() <= 256,
        "Runtime provider catalog exceeds its limit"
    );
    let aliases = state.registry.list();
    let mut models = Vec::new();
    let mut routes = std::collections::HashSet::new();
    for provider in providers {
        if !provider
            .get("has_model_catalog")
            .and_then(Value::as_bool)
            .context("Runtime provider omitted its catalog availability")?
        {
            continue;
        }
        let kind = provider
            .get("id")
            .and_then(Value::as_str)
            .context("Runtime provider omitted its identity")?;
        let parsed: codewhale_config::ProviderKind =
            serde_json::from_value(json!(kind)).context("Runtime provider kind is unsupported")?;
        anyhow::ensure!(
            kind == parsed.as_str()
                || codewhale_config::descriptors::compatibility_for_id(kind)
                    .is_some_and(|compatibility| compatibility.kind == parsed),
            "Runtime provider kind is not a recognized wire identity"
        );
        let provider_id = match provider.get("model_provider_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id.as_str()),
            _ => bail!("Runtime configured provider identity is invalid"),
        };
        if !routes.insert((kind.to_string(), provider_id.map(str::to_string))) {
            continue;
        }
        let mut url = reqwest::Url::parse(&format!("{base_url}/v1/providers/{kind}/models"))?;
        url.query_pairs_mut().append_pair("limit", "250");
        if let Some(id) = provider_id {
            url.query_pairs_mut().append_pair("model_provider_id", id);
        }
        let request = authed(client.get(url)).timeout(Duration::from_secs(10));
        for mut model in runtime_catalog_models(request, kind, provider_id).await? {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .context("Runtime model identity is invalid")?;
            let compatibility_aliases = aliases
                .iter()
                .find(|entry| {
                    parsed != codewhale_config::ProviderKind::Custom
                        && provider_id.is_none_or(|identity| identity == kind)
                        && entry.provider == parsed
                        && entry.id == id
                })
                .map(|entry| entry.aliases.clone())
                .unwrap_or_default();
            let capabilities: codewhale_config::route::RouteCapabilities =
                match model.pointer("/facts/capabilities") {
                    None | Some(Value::Null) => Default::default(),
                    Some(value) => serde_json::from_value(value.clone())
                        .context("Runtime model capabilities are invalid")?,
                };
            let row = model
                .as_object_mut()
                .context("Runtime model entry is invalid")?;
            row.insert("provider".into(), serde_json::to_value(parsed)?);
            row.insert("model_provider_id".into(), json!(provider_id));
            row.insert("aliases".into(), json!(compatibility_aliases));
            row.insert(
                "supports_tools".into(),
                json!(
                    capabilities.native_tool_calls
                        == codewhale_config::route::CapabilityState::Supported
                ),
            );
            row.insert(
                "supports_reasoning".into(),
                json!(
                    capabilities.reasoning == codewhale_config::route::CapabilityState::Supported
                ),
            );
            row.insert("capabilities".into(), serde_json::to_value(capabilities)?);
            models.push(model);
            anyhow::ensure!(
                models.len() <= 100_000,
                "Runtime model catalog exceeds the response limit"
            );
        }
    }
    Ok(models)
}

pub(crate) async fn acquire_live_runtime_bridge(
    state: &AppState,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, JsonRpcError> {
    Ok(acquire_runtime_bridge(state).await?.lock_owned().await)
}

/// Historical respawn comparison; all actual frontends use the held owner.
#[cfg(all(test, unix))]
pub(crate) async fn acquire_live_runtime_bridge_with<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, JsonRpcError>
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
    Err(JsonRpcError::runtime_unavailable(
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

    /// Read the existing Runtime catalog before a one-shot prompt can create
    /// its thread. Existing threads are checked by canonical turn admission.
    pub(crate) async fn require_output_limited_model(
        &self,
        requested_model: Option<&str>,
    ) -> Result<()> {
        let providers = self
            .request_json(self.authed(self.client.get(format!("{}/v1/providers", self.base_url))))
            .await?;
        let current = providers
            .get("current")
            .and_then(Value::as_str)
            .context("Runtime provider is unavailable")?;
        let current_id = match providers.get("current_provider_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id.as_str()),
            _ => bail!("Runtime configured provider identity is invalid"),
        };
        let provider = providers
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|providers| {
                providers.iter().find(|provider| {
                    provider.get("id").and_then(Value::as_str) == Some(current)
                        && provider.get("model_provider_id").and_then(Value::as_str) == current_id
                })
            })
            .context("Runtime provider is unavailable")?;
        let model = requested_model
            .or_else(|| provider.get("default_model").and_then(Value::as_str))
            .context("maxOutputTokens requires an exact model")?;
        if model.trim().is_empty() || model.eq_ignore_ascii_case("auto") {
            bail!("maxOutputTokens requires an exact model");
        }
        if !current
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("Runtime provider identity is invalid");
        }
        let mut url =
            reqwest::Url::parse(&format!("{}/v1/providers/{current}/models", self.base_url))?;
        url.query_pairs_mut().append_pair("limit", "250");
        if let Some(id) = current_id {
            url.query_pairs_mut().append_pair("model_provider_id", id);
        }
        let request = self
            .authed(self.client.get(url))
            .timeout(Duration::from_secs(10));
        let catalog = runtime_catalog_models(request, current, current_id).await?;
        let entry = catalog
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
            .context("Output-limit support is unknown for the selected Runtime model")?;
        anyhow::ensure!(
            entry.get("output_token_limit").and_then(Value::as_str) == Some("supported"),
            "The selected Runtime model does not support maxOutputTokens"
        );
        Ok(())
    }

    /// Resolve `stdio_thread_id` to a runtime thread, minting one only when
    /// `thread_map` has no entry. The map lives on [`AppState`] and outlives
    /// this bridge, so a thread created under a previous child keeps its id
    /// here as long as the store it was persisted to is shared (#6246).
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

    /// Drop a thread mapping (and its seq cursor) once no caller can name
    /// the client-facing key again.
    pub(crate) fn forget_thread(
        &mut self,
        thread_map: &mut HashMap<String, String>,
        stdio_thread_id: &str,
    ) {
        if let Some(runtime_thread_id) = thread_map.remove(stdio_thread_id) {
            self.last_seq_by_thread.remove(&runtime_thread_id);
        }
    }

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
                base_url: self.base_url.clone(),
                auth_token: self.auth_token.clone(),
                runtime_thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
            };
            if let Err(interrupt) = interrupt_turn_request(&orphan).await {
                tracing::warn!(
                    thread_id,
                    turn_id = %turn_id,
                    "failed to interrupt a turn whose response_start could not be written: {}",
                    interrupt.message
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
                    base_url: self.base_url.clone(),
                    auth_token: self.auth_token.clone(),
                    runtime_thread_id: thread_id.to_string(),
                    turn_id: turn_id.clone(),
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
            let _ = emit_stdio_event(
                writer,
                json!({
                    "type": "response_end",
                    "response_id": response_id,
                }),
            )
            .await;
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
                base_url: self.base_url.clone(),
                auth_token: self.auth_token.clone(),
                runtime_thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
            };
            if let Err(error) = interrupt_turn_request(&orphan).await {
                tracing::warn!(
                    thread_id,
                    turn_id = %turn_id,
                    "failed to interrupt a turn whose event stream broke: {}",
                    error.message
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
                        let status = turn_terminal_status(&payload);
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

pub(crate) fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some(buffer.drain(..pos + 4).collect());
    }
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|pos| buffer.drain(..pos + 2).collect())
}

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

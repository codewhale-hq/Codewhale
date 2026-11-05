use crate::stdio::{emit_stdio_event, interrupt_turn_request, retire_turn_registration};
use crate::{
    AppState, DispatchError, InFlightTurn, RuntimeOwnerFrontend, RuntimeOwnerRequest,
    RuntimeThreadHint, TurnExecutionOwner, TurnRegistry, TurnTerminalStatus, TurnTranscript,
    owner_calls, thread_control,
};
use anyhow::{Context as _, Result, anyhow};
use codewhale_app_server_protocol::runtime::RuntimeImageInput;
use codewhale_protocol::{
    EventFrame, PromptRequest, PromptResponse, ResponseChannel, ThreadRequest, ThreadResponse,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) async fn handle_thread_request(
    state: &AppState,
    req: ThreadRequest,
) -> std::result::Result<ThreadResponse, DispatchError> {
    if matches!(&req, ThreadRequest::GoalSet(_)) {
        return Err(DispatchError::unsupported_interactive_execution());
    }
    thread_control::handle(state, req).await
}

pub(crate) struct RuntimeTurnInput<'a> {
    pub(crate) input: &'a str,
    pub(crate) images: &'a [RuntimeImageInput],
    pub(crate) max_output_tokens: Option<std::num::NonZeroU32>,
    pub(crate) expected_workspace: Option<&'a Path>,
    pub(crate) cancellation: Option<&'a CancellationToken>,
}

pub(crate) struct NativeTurn<'a> {
    pub(crate) allowed_tools: Option<&'a [String]>,
    pub(crate) max_output_tokens: Option<std::num::NonZeroU32>,
    pub(crate) thread_key: &'a str,
    pub(crate) input: &'a str,
    pub(crate) images: &'a [RuntimeImageInput],
    /// Model for the runtime thread when this call is the one that creates
    /// it. An existing thread keeps the model it was created with.
    pub(crate) model_override: Option<String>,
    /// Publish the live turn so a concurrent `thread/interrupt` can cancel
    /// it. Only stdio has a mid-turn channel, so only stdio sets this.
    pub(crate) interruptible: bool,
    pub(crate) ephemeral: bool,
    pub(crate) cancellation: Option<CancellationToken>,
}

/// Execute exactly one turn on the real runtime.
///
pub(crate) async fn run_native_turn<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    turn: NativeTurn<'_>,
    transcript: Option<&mut TurnTranscript>,
) -> std::result::Result<Value, DispatchError> {
    run_native_turn_for(state, writer, turn, transcript, None).await
}

pub(crate) async fn run_native_turn_for<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    turn: NativeTurn<'_>,
    transcript: Option<&mut TurnTranscript>,
    connection: Option<crate::connection::ConnectionId>,
) -> std::result::Result<Value, DispatchError> {
    if !matches!(turn.allowed_tools, Some([])) {
        return Err(DispatchError::unsupported_interactive_execution());
    }
    let mut hint = {
        let hints = state.stdio_thread_hints.lock().await;
        hints.get(turn.thread_key).cloned()
    };
    if let Some(model) = turn.model_override {
        hint.get_or_insert_with(RuntimeThreadHint::default).model = Some(model);
    }
    // Durable compatibility IDs resolve through full canonical history and
    // exact owner receipts before the existing turn transport can begin.
    let resolved = if turn.ephemeral {
        None
    } else {
        Some(
            thread_control::resolve(state, turn.thread_key, true)
                .await
                .map_err(|error| thread_control::rpc_error(error, Some(turn.thread_key)))?,
        )
    };
    if let Some((_, workspace)) = resolved.as_ref() {
        hint.get_or_insert_with(RuntimeThreadHint::default)
            .workspace = Some(workspace.clone());
    } else if let Some(workspace) = state.frontend_workspace.as_ref() {
        hint.get_or_insert_with(RuntimeThreadHint::default)
            .workspace
            .get_or_insert_with(|| workspace.clone());
    }
    // Retain the acknowledged/observed scope across all transport awaits.
    // The owning Runtime rechecks this assertion at its final turn admission.
    let expected_workspace = hint.as_ref().and_then(|hint| hint.workspace.clone());
    let owner = owner_calls::captured(state)
        .map_err(|error| DispatchError::runtime_unavailable(error.to_string()))?;
    if turn.max_output_tokens.is_some() {
        let info = owner_calls::execute(&owner, RuntimeOwnerRequest::Capabilities)
            .await
            .map_err(|error| DispatchError::runtime_unavailable(error.to_string()))?;
        if info
            .pointer("/capabilities/turn_output_token_limit")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(DispatchError::invalid_params(
                "Runtime does not support maxOutputTokens",
            ));
        }
    }
    if turn.max_output_tokens.is_some() && resolved.is_none() {
        owner_calls::require_output_limited_model(
            &owner,
            hint.as_ref().and_then(|hint| hint.model.as_deref()),
        )
        .await
        .map_err(|error| DispatchError::invalid_params(error.to_string()))?;
    }
    let runtime_thread_id = match resolved {
        Some((id, _)) => id,
        None => {
            let hint = hint.unwrap_or_default();
            let record = owner_calls::execute(
                &owner,
                RuntimeOwnerRequest::CreateThread(json!({
                    "model": hint.model,
                    "workspace": hint.workspace,
                    "mode": "agent",
                    "archived": false,
                })),
            )
            .await
            .map_err(|error| DispatchError::runtime_unavailable(format!("{error:#}")))?;
            extract_runtime_thread_id(&record)
                .map_err(|error| DispatchError::internal(format!("{error:#}")))?
                .to_owned()
        }
    };
    let result = async {
        let registration = turn
            .interruptible
            .then(|| (state.in_flight_turns.clone(), turn.thread_key.to_owned()));
        message_native_thread(
            owner,
            &runtime_thread_id,
            RuntimeTurnInput {
                input: turn.input,
                images: turn.images,
                max_output_tokens: turn.max_output_tokens,
                expected_workspace: expected_workspace.as_deref(),
                cancellation: turn.cancellation.as_ref(),
            },
            writer,
            registration,
            transcript,
            connection,
        )
        .await
        .map_err(|error| {
            if error.downcast_ref::<NativeTurnContext>().is_some() {
                error
            } else {
                error.context(NativeTurnContext {
                    thread_id: runtime_thread_id,
                    turn_id: None,
                })
            }
        })
    }
    .await;
    result.map_err(|error| {
        let mut response = DispatchError::internal(format!("{error:#}"));
        if let Some(context) = error.downcast_ref::<NativeTurnContext>() {
            response.wire.data = Some(json!({
                "thread_id": context.thread_id, "turn_id": context.turn_id,
                "replay_safe": false,
            }));
        }
        if crate::stdio::is_control_write_failure(&error) {
            response.transport_error = Some(error);
        }
        response
    })
}

/// Run a prompt as a genuine model turn and return what the model actually
/// said.
///
/// `writer` receives the same streaming frames stdio `thread/message` emits;
/// HTTP callers pass a sink and read the frames back out of
/// [`PromptResponse::events`].
pub(crate) async fn run_prompt_turn<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    req: PromptRequest,
    cancellation: Option<CancellationToken>,
) -> std::result::Result<PromptResponse, DispatchError> {
    if req.prompt.trim().is_empty() {
        return Err(DispatchError::invalid_params("prompt must not be empty"));
    }
    let ephemeral = req.thread_id.is_none();
    let thread_key = req
        .thread_id
        .clone()
        .unwrap_or_else(|| format!("prompt-{}", Uuid::new_v4()));

    let mut transcript = TurnTranscript::default();
    run_native_turn(
        state,
        writer,
        NativeTurn {
            allowed_tools: req.allowed_tools.as_deref(),
            max_output_tokens: req.max_output_tokens,
            thread_key: &thread_key,
            input: &req.prompt,
            images: &req.images,
            model_override: req.model.clone(),
            // `thread/interrupt` addresses client-facing thread ids. A
            // one-shot prompt has none to hand back.
            interruptible: false,
            ephemeral,
            cancellation,
        },
        Some(&mut transcript),
    )
    .await?;

    let model = transcript.model.unwrap_or_else(|| "unknown".to_string());

    Ok(PromptResponse {
        output: transcript.text,
        model,
        events: transcript.events,
    })
}

pub(crate) async fn handle_prompt_request<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    req: PromptRequest,
    cancellation: Option<CancellationToken>,
) -> std::result::Result<PromptResponse, DispatchError> {
    run_prompt_turn(state, writer, req, cancellation).await
}

/// HTTP `/thread` with a `Message` body: same engine as stdio
/// `thread/message`, but the turn is collected rather than streamed because
/// this transport is request/response.
pub(crate) async fn run_http_thread_message(
    state: &AppState,
    thread_id: String,
    input: String,
    allowed_tools: Option<Vec<String>>,
    images: Vec<RuntimeImageInput>,
    max_output_tokens: Option<std::num::NonZeroU32>,
) -> std::result::Result<ThreadResponse, DispatchError> {
    let mut transcript = TurnTranscript::default();
    let mut sink = tokio::io::sink();
    let result = run_native_turn(
        state,
        &mut sink,
        NativeTurn {
            allowed_tools: allowed_tools.as_deref(),
            max_output_tokens,
            thread_key: &thread_id,
            input: &input,
            images: &images,
            model_override: None,
            interruptible: false,
            ephemeral: false,
            cancellation: None,
        },
        Some(&mut transcript),
    )
    .await?;

    Ok(ThreadResponse {
        thread_id,
        // The turn ran to a terminal state before this response was built,
        // which is exactly what the old `accepted` did not mean.
        status: "completed".to_string(),
        thread: None,
        threads: Vec::new(),
        goal: None,
        model: transcript.model,
        model_provider: None,
        cwd: None,
        approval_policy: None,
        sandbox: None,
        events: transcript.events,
        data: result.get("data").cloned().unwrap_or_else(|| json!({})),
    })
}

pub(crate) fn extract_runtime_thread_id(record: &Value) -> Result<&str> {
    record
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("runtime API thread response missing id"))
}

pub(crate) fn turn_terminal_status(payload: &Value) -> Result<TurnTerminalStatus> {
    match payload.pointer("/turn/status").and_then(Value::as_str) {
        Some("completed") => Ok(TurnTerminalStatus::Completed),
        Some("failed") => Ok(TurnTerminalStatus::Failed),
        Some("interrupted") => Ok(TurnTerminalStatus::Interrupted),
        Some("canceled" | "cancelled") => Ok(TurnTerminalStatus::Canceled),
        _ => anyhow::bail!("native terminal event has no supported terminal status"),
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "native thread {thread_id}, turn {turn_id:?}; inspect retained native state without replay"
)]
struct NativeTurnContext {
    thread_id: String,
    turn_id: Option<String>,
}

struct NativeTurnClaim {
    live: InFlightTurn,
    registration: Option<(TurnRegistry, String)>,
    registration_published: bool,
    terminal_observed: bool,
    cleanup: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl NativeTurnClaim {
    fn begin_cleanup(&mut self) -> Result<()> {
        if self.cleanup.is_some() {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current()
            .context("native turn cleanup has no active runtime; inspect without replay")?;
        let live = self.live.clone();
        let registration = self.registration.clone();
        let registration_published = self.registration_published;
        let terminal_observed = self.terminal_observed;
        self.cleanup = Some(handle.spawn(async move {
            let interrupted = if terminal_observed {
                Ok(())
            } else {
                interrupt_turn_request(&live)
                    .await
                    .map(|_| ())
                    .map_err(|error| anyhow!("native interruption remains uncertain: {}", error.wire.message))
            };
            if let Some((registry, key)) = registration {
                if interrupted.is_ok() {
                    retire_turn_registration(&registry, &key, &live).await;
                } else if !registration_published {
                    registry.lock().await.entry(key).or_insert_with(|| live.clone());
                }
            }
            if let Err(error) = &interrupted {
                tracing::error!(%error, thread_id = %live.runtime_thread_id, turn_id = %live.turn_id, "native turn transport cleanup did not confirm interruption");
            }
            interrupted
        }));
        Ok(())
    }

    async fn finish(&mut self) -> Result<()> {
        self.begin_cleanup()?;
        self.cleanup
            .as_mut()
            .expect("started cleanup")
            .await
            .context("native cleanup task failed; inspect its canonical receipt without replay")?
    }
}

impl Drop for NativeTurnClaim {
    fn drop(&mut self) {
        if let Err(error) = self.begin_cleanup() {
            tracing::error!(%error, thread_id = %self.live.runtime_thread_id, turn_id = %self.live.turn_id, "native turn transport cleanup could not be scheduled");
        }
    }
}

async fn message_native_thread<W: AsyncWrite + Unpin>(
    owner: Arc<dyn RuntimeOwnerFrontend>,
    thread_id: &str,
    input: RuntimeTurnInput<'_>,
    writer: &mut W,
    registration: Option<(TurnRegistry, String)>,
    mut transcript: Option<&mut TurnTranscript>,
    connection: Option<crate::connection::ConnectionId>,
) -> Result<Value> {
    anyhow::ensure!(
        !input
            .cancellation
            .is_some_and(CancellationToken::is_cancelled),
        "control request was canceled before native turn admission"
    );
    let mut events = tokio::time::timeout(
        Duration::from_secs(30),
        owner.events(thread_id.to_owned(), 0),
    )
    .await
    .context("native event observation did not open before admission")??;
    anyhow::ensure!(
        !input
            .cancellation
            .is_some_and(CancellationToken::is_cancelled),
        "control request was canceled before native turn admission"
    );
    let mut body = json!({ "prompt": input.input, "allowed_tools": [] });
    if let Some(workspace) = input.expected_workspace {
        body["expected_workspace"] = json!(workspace);
    }
    if !input.images.is_empty() {
        body["images"] = json!(input.images);
    }
    if let Some(limit) = input.max_output_tokens {
        body["maxOutputTokens"] = json!(limit);
    }
    let accepted = owner_calls::execute(
        &owner,
        RuntimeOwnerRequest::StartTurn {
            thread_id: thread_id.to_owned(),
            body,
        },
    )
    .await?;
    let turn_id = accepted
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .context("native owner admission returned no turn identity")?
        .to_owned();
    anyhow::ensure!(
        accepted.pointer("/turn/thread_id").and_then(Value::as_str) == Some(thread_id),
        "native owner admission returned another thread identity; no automatic replay"
    );
    let response_id = format!("{thread_id}:{turn_id}");
    let live = InFlightTurn {
        owner: TurnExecutionOwner::Native(owner),
        runtime_thread_id: thread_id.to_owned(),
        turn_id: turn_id.clone(),
        connection,
    };
    let mut claim = NativeTurnClaim {
        live,
        registration,
        registration_published: false,
        terminal_observed: false,
        cleanup: None,
    };
    let context = NativeTurnContext {
        thread_id: thread_id.to_owned(),
        turn_id: Some(turn_id.clone()),
    };
    if let Some(transcript) = transcript.as_deref_mut() {
        transcript.model = accepted
            .pointer("/thread/model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        transcript.events.push(EventFrame::ResponseStart {
            response_id: response_id.clone(),
        });
    }
    let result = async {
        if let Some((registry, key)) = claim.registration.as_ref() {
            match registry.lock().await.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => anyhow::bail!(
                    "a turn is already registered for thread {key}; refusing a second registration"
                ),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(claim.live.clone());
                }
            }
            claim.registration_published = true;
        }
        anyhow::ensure!(
            !input.cancellation.is_some_and(CancellationToken::is_cancelled),
            "control request was canceled after native turn admission"
        );
        emit_stdio_event(writer, json!({
            "type": "response_start", "response_id": response_id,
        })).await?;
        loop {
            let frame = tokio::select! {
                _ = async {
                    match input.cancellation {
                        Some(cancellation) => cancellation.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => anyhow::bail!("control request canceled; native turn cleanup required"),
                frame = events.next() => frame,
            };
            let Some(frame) = frame else { break; };
            if frame.event == "stream.end" {
                anyhow::bail!("native journal observation ended before terminal settlement; no automatic replay");
            }
            if frame.data.get("thread_id").and_then(Value::as_str) != Some(thread_id)
                || frame.data.get("turn_id").and_then(Value::as_str) != Some(turn_id.as_str()) {
                continue;
            }
            let payload = frame.data.get("payload").cloned().unwrap_or(Value::Null);
            match frame.event.as_str() {
                "approval.required" | "user_input.required" | "tool_call.requested" => {
                    anyhow::bail!("interactive_execution_unsupported: native owner requested interaction during a tool-free turn");
                }
                "item.delta" if payload.get("kind").and_then(Value::as_str) == Some("agent_message") => {
                    if let Some(delta) = payload.get("delta").and_then(Value::as_str).filter(|delta| !delta.is_empty()) {
                        emit_stdio_event(writer, json!({
                            "type": "response_delta", "response_id": response_id, "delta": delta,
                        })).await?;
                        if let Some(transcript) = transcript.as_deref_mut() {
                            transcript.text.push_str(delta);
                            transcript.events.push(EventFrame::ResponseDelta {
                                response_id: response_id.clone(), delta: delta.to_owned(),
                                channel: ResponseChannel::Text,
                            });
                        }
                    }
                }
                "turn.completed" => {
                    let record = payload.get("turn").context("native terminal event omitted its turn")?;
                    anyhow::ensure!(
                        record.get("id").and_then(Value::as_str) == Some(turn_id.as_str())
                            && record.get("thread_id").and_then(Value::as_str) == Some(thread_id),
                        "native terminal event has another turn identity"
                    );
                    let status = turn_terminal_status(&payload)?;
                    claim.terminal_observed = true;
                    if let Some(transcript) = transcript.as_deref_mut()
                        && let Some(model) = record.get("effective_model").and_then(Value::as_str)
                    {
                        transcript.model = Some(model.to_owned());
                    }
                    match status {
                        TurnTerminalStatus::Completed => return Ok(record.clone()),
                        _ => anyhow::bail!("{}", record.get("error").and_then(Value::as_str).unwrap_or("native turn stopped without successful completion")),
                    }
                }
                _ => {}
            }
        }
        anyhow::bail!("native event stream ended before turn.completed; no automatic replay")
    }.await;
    let cleanup = claim.finish().await;
    let record = match (result, cleanup) {
        (Ok(record), Ok(())) => record,
        (Err(error), Ok(())) => return Err(error.context(context)),
        (Ok(_), Err(error)) => return Err(error.context(context)),
        (Err(error), Err(cleanup)) => {
            return Err(error
                .context(format!(
                    "native cleanup did not confirm interruption: {cleanup:#}"
                ))
                .context(context));
        }
    };
    emit_stdio_event(
        writer,
        json!({ "type": "response_end", "response_id": response_id }),
    )
    .await
    .context(context)?;
    if let Some(transcript) = transcript {
        transcript
            .events
            .push(EventFrame::ResponseEnd { response_id });
    }
    Ok(json!({
        "thread_id": thread_id, "status": "accepted", "thread": null, "threads": [],
        "model": null, "model_provider": null, "cwd": null, "approval_policy": null,
        "sandbox": null, "events": [], "data": { "turn_id": turn_id, "turn": record },
    }))
}

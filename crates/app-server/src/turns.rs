use crate::bridge::acquire_live_runtime_bridge;
use crate::{
    AppState, JsonRpcError, RuntimeThreadHint, TurnTerminalStatus, TurnTranscript, thread_control,
};
use anyhow::{Result, anyhow};
use codewhale_protocol::runtime::RuntimeImageInput;
use codewhale_protocol::{PromptRequest, PromptResponse, ThreadRequest, ThreadResponse};
use serde_json::{Value, json};
use std::path::Path;
use tokio::io::AsyncWrite;
use uuid::Uuid;

pub(crate) async fn handle_thread_request(
    state: &AppState,
    req: ThreadRequest,
) -> std::result::Result<ThreadResponse, JsonRpcError> {
    if matches!(&req, ThreadRequest::GoalSet(_)) {
        return Err(JsonRpcError::unsupported_interactive_execution());
    }
    thread_control::handle(state, req).await
}

/// One turn's worth of routing decisions, shared by every surface that runs
/// a turn through the bridge.
pub(crate) struct RuntimeTurnInput<'a> {
    pub(crate) input: &'a str,
    pub(crate) images: &'a [RuntimeImageInput],
    pub(crate) max_output_tokens: Option<std::num::NonZeroU32>,
    pub(crate) expected_workspace: Option<&'a Path>,
}

pub(crate) struct BridgedTurn<'a> {
    pub(crate) allowed_tools: Option<&'a [String]>,
    pub(crate) max_output_tokens: Option<std::num::NonZeroU32>,
    /// Client-facing thread id; the bridge maps it to a runtime thread.
    pub(crate) thread_key: &'a str,
    pub(crate) input: &'a str,
    pub(crate) images: &'a [RuntimeImageInput],
    /// Model for the runtime thread when this call is the one that creates
    /// it. An existing thread keeps the model it was created with.
    pub(crate) model_override: Option<String>,
    /// Publish the live turn so a concurrent `thread/interrupt` can cancel
    /// it. Only stdio has a mid-turn channel, so only stdio sets this.
    pub(crate) interruptible: bool,
    /// Forget the thread mapping once the turn ends. Set for one-shot
    /// prompts, whose synthetic thread key no client can name again.
    pub(crate) ephemeral: bool,
    /// Refuse a `thread_key` that is neither mapped, durably linked, nor a
    /// persisted thread, instead of minting an empty runtime thread for it.
    /// Set by `thread/message`, whose ids come from `thread/create`.
    pub(crate) require_known_thread: bool,
}

/// Execute exactly one turn on the real runtime.
///
/// This is the only way any app-server surface runs a model: `/prompt`,
/// `prompt/request`, `prompt/run`, stdio `thread/message`, and HTTP `/thread`
/// messages all land here. There is no local fallback that fabricates a
/// response — if the runtime cannot be reached the caller gets
/// [`JsonRpcError::runtime_unavailable`] and nothing is written to history.
pub(crate) async fn run_bridged_turn<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    turn: BridgedTurn<'_>,
    transcript: Option<&mut TurnTranscript>,
) -> std::result::Result<Value, JsonRpcError> {
    if !matches!(turn.allowed_tools, Some([])) {
        return Err(JsonRpcError::unsupported_interactive_execution());
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
    if turn.require_known_thread && resolved.is_none() {
        return Err(JsonRpcError::thread_not_found(turn.thread_key));
    }
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
    let mut bridge = acquire_live_runtime_bridge(state).await?;
    let mut thread_map = state.runtime_thread_map.clone().lock_owned().await;
    if let Some((id, _)) = resolved {
        thread_map.insert(turn.thread_key.to_string(), id);
    }
    if turn.max_output_tokens.is_some() {
        let info = bridge
            .request_json(
                bridge.authed(
                    bridge
                        .client
                        .get(format!("{}/v1/runtime/info", bridge.base_url)),
                ),
            )
            .await
            .map_err(|error| JsonRpcError::runtime_unavailable(error.to_string()))?;
        if info
            .pointer("/capabilities/turn_output_token_limit")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(JsonRpcError::invalid_params(
                "Runtime does not support maxOutputTokens",
            ));
        }
        if !thread_map.contains_key(turn.thread_key) {
            bridge
                .require_output_limited_model(hint.as_ref().and_then(|hint| hint.model.as_deref()))
                .await
                .map_err(|error| JsonRpcError::invalid_params(error.to_string()))?;
        }
    }
    let runtime_thread_id = bridge
        .ensure_runtime_thread(&mut thread_map, turn.thread_key, hint)
        .await
        .map_err(|error| JsonRpcError::runtime_unavailable(error.to_string()))?;
    drop(thread_map);
    let registration = turn
        .interruptible
        .then(|| (state.in_flight_turns.clone(), turn.thread_key.to_string()));
    let result = bridge
        .message_thread(
            &runtime_thread_id,
            RuntimeTurnInput {
                input: turn.input,
                images: turn.images,
                max_output_tokens: turn.max_output_tokens,
                expected_workspace: expected_workspace.as_deref(),
            },
            writer,
            registration,
            transcript,
        )
        .await;
    if turn.ephemeral {
        // Drop the mapping while we still hold the bridge lock, so a
        // long-lived app-server does not accumulate one entry per one-shot
        // prompt.
        let mut thread_map = state.runtime_thread_map.lock().await;
        bridge.forget_thread(&mut thread_map, turn.thread_key);
    }
    result.map_err(|err| JsonRpcError::internal(err.to_string()))
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
) -> std::result::Result<PromptResponse, JsonRpcError> {
    if req.prompt.trim().is_empty() {
        return Err(JsonRpcError::invalid_params("prompt must not be empty"));
    }
    // The turn engine has no threadless mode, so a prompt without a thread
    // gets a fresh one. Keying it on a uuid keeps a one-shot prompt out of
    // any caller's history and out of the way of concurrent prompts.
    let ephemeral = req.thread_id.is_none();
    let thread_key = req
        .thread_id
        .clone()
        .unwrap_or_else(|| format!("prompt-{}", Uuid::new_v4()));

    let mut transcript = TurnTranscript::default();
    run_bridged_turn(
        state,
        writer,
        BridgedTurn {
            allowed_tools: req.allowed_tools.as_deref(),
            max_output_tokens: req.max_output_tokens,
            thread_key: &thread_key,
            input: &req.prompt,
            images: &req.images,
            model_override: req.model.clone(),
            // `thread/interrupt` addresses client-facing thread ids. A
            // one-shot prompt has none to hand back, and a caller-supplied
            // thread id is already interruptible through `thread/message`.
            interruptible: false,
            ephemeral,
            // A caller-chosen `/prompt` thread key keeps its conversation
            // but need not name a `thread/create` thread.
            require_known_thread: false,
        },
        Some(&mut transcript),
    )
    .await?;

    // Report the model the runtime actually ran, never a locally resolved
    // guess. The fallbacks only matter for a runtime that omits the field.
    let model = match transcript.model {
        Some(model) => model,
        None => match req.model {
            Some(model) => model,
            None => state
                .config
                .read()
                .await
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        },
    };

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
) -> std::result::Result<PromptResponse, JsonRpcError> {
    run_prompt_turn(state, writer, req).await
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
) -> std::result::Result<ThreadResponse, JsonRpcError> {
    let mut transcript = TurnTranscript::default();
    let mut sink = tokio::io::sink();
    let result = run_bridged_turn(
        state,
        &mut sink,
        BridgedTurn {
            allowed_tools: allowed_tools.as_deref(),
            max_output_tokens,
            thread_key: &thread_id,
            input: &input,
            images: &images,
            model_override: None,
            interruptible: false,
            ephemeral: false,
            require_known_thread: true,
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

pub(crate) fn turn_terminal_status(payload: &Value) -> TurnTerminalStatus {
    match payload
        .pointer("/turn/status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_ascii_lowercase()
        .as_str()
    {
        "failed" => TurnTerminalStatus::Failed,
        "interrupted" => TurnTerminalStatus::Interrupted,
        "canceled" | "cancelled" => TurnTerminalStatus::Canceled,
        _ => TurnTerminalStatus::Completed,
    }
}

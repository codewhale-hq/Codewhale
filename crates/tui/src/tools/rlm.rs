//! Compatibility persistent-RLM session tools.
//!
//! v0.8.33 replaces the old one-shot `rlm` tool with a head/hands surface:
//! `rlm_open` creates a named Python kernel over a large context,
//! `rlm_eval` runs bounded probes against it, `rlm_configure` adjusts runtime
//! feedback, and `rlm_close` tears it down.
//!
//! The normal Agent path now owns one session-persistent `repl` kernel. This
//! action-shaped surface stays registered for explicit compatibility and saved
//! transcript replay, but is hidden from new model turns. Its `rlm_*` aliases
//! force the action so old transcripts replay correctly — the pattern
//! `BashTool` established for `exec_shell*` in #4625.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::rlm::RlmBridge;
use crate::rlm::bridge::wait_for_deadline;
use crate::rlm::session::{ContextMeta, OutputFeedback, RlmSession, derive_session_name};
use crate::tools::fetch_url::FetchUrlTool;
use crate::tools::handle::VarHandle;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codewhale_repl::PythonRuntime;

/// Registered name of the persistent RLM session tool.
pub(crate) const RLM_TOOL_NAME: &str = "rlm";
const MAX_INLINE_CONTENT_CHARS: usize = 200_000;
const FULL_STDOUT_HEAD_CHARS: usize = 4_096;
const FULL_STDOUT_TAIL_CHARS: usize = 1_024;

/// When `rlm_eval` stdout exceeds this many characters the full body is
/// stored as a `var_handle` instead of inlined into the parent transcript.
/// The model retrieves the body via `handle_read` using the returned handle.
const STDOUT_HANDLE_THRESHOLD_CHARS: usize = 1_000;
const HARD_SUB_RLM_DEPTH_CAP: u32 = 3;

const ALL_ACTIONS: &[&str] = &["session_objects", "open", "eval", "configure", "close"];

fn rlm_kernel_error_result(
    error: &str,
    elapsed: Duration,
    usage_batch: &crate::cost_status::RuntimeUsageBatch,
    nested_events: &[Value],
) -> ToolResult {
    let mut metadata = json!({
        // The registered tool is `rlm`; `eval` is its action. Naming a
        // retired `rlm_eval` tool here taught the model a call it cannot
        // make (2026-08-04 audit).
        "tool": "rlm",
        "action": "eval",
        "duration_ms": elapsed.as_millis() as u64,
        "kernel_error": true,
    });
    crate::cost_status::attach_child_usage_batch_metadata(&mut metadata, usage_batch);
    ToolResult::error(
        json!({
            "tool": "rlm", "action": "eval", "error": error,
            "nested_events": nested_events,
        })
        .to_string(),
    )
    .with_metadata(metadata)
}

/// Unified RLM session tool.
///
/// One input schema and the existing per-session Python store. Provider RPCs
/// require the caller receipt attached by Core to the actual ToolContext.
pub struct RlmTool {
    name: &'static str,
    forced_action: Option<&'static str>,
}

impl RlmTool {
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn alias(name: &'static str, action: &'static str) -> Self {
        Self {
            name,
            forced_action: Some(action),
        }
    }

    fn resolve_action<'a>(&'a self, input: &'a Value) -> Result<&'a str, ToolError> {
        let action = match self.forced_action {
            Some(action) => action,
            None => input.get("action").and_then(Value::as_str).ok_or_else(|| {
                ToolError::invalid_input(format!(
                    "rlm: missing `action` (one of: {})",
                    ALL_ACTIONS.join(", ")
                ))
            })?,
        };
        if ALL_ACTIONS.contains(&action) {
            Ok(action)
        } else {
            Err(ToolError::invalid_input(format!(
                "rlm: invalid action `{action}` (one of: {})",
                ALL_ACTIONS.join(", ")
            )))
        }
    }

    /// Without concrete input, open may fetch a URL. Input-specific approval
    /// below keeps local/inline reads automatic and inherits fetch_url's
    /// outbound-payload approval for URL sources.
    fn action_requires_approval(action: &str) -> bool {
        matches!(action, "eval" | "open")
    }

    /// Mirror of the legacy per-tool read-only contract (capability-derived):
    /// `rlm_open` carries `ExecutesCode`, so only session_objects / configure /
    /// close counted as read-only.
    fn action_is_read_only(action: &str) -> bool {
        matches!(action, "session_objects" | "configure" | "close")
    }

    fn action_capabilities(action: &str) -> Vec<ToolCapability> {
        match action {
            "session_objects" => vec![ToolCapability::ReadOnly],
            "open" => vec![
                ToolCapability::ReadOnly,
                ToolCapability::Network,
                ToolCapability::ExecutesCode,
                ToolCapability::RequiresApproval,
            ],
            "eval" => vec![
                ToolCapability::Network,
                ToolCapability::ExecutesCode,
                ToolCapability::RequiresApproval,
            ],
            // configure / close
            _ => vec![ToolCapability::ReadOnly],
        }
    }
}

#[async_trait]
impl ToolSpec for RlmTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        // The normal Agent path owns a session-scoped `repl` kernel. Keep the
        // old action fan-out registered for replay and explicit compatibility,
        // but do not teach a second RLM workflow to new model turns.
        false
    }

    fn description(&self) -> &'static str {
        match self.forced_action {
            Some("session_objects") => {
                "List active prompt/history/session symbolic objects as compact cards. \
                 Pass one of the returned `id` values to `rlm_open` as \
                 `session_object` to inspect it inside an RLM REPL without copying the \
                 full prompt or transcript into the parent context."
            }
            Some("open") => {
                "Open a persistent RLM context. Loads `file_path`, `content`, `url`, \
                 or `session_object` into a named Python kernel and returns only \
                 metadata: name, length, preview, and sha256. Use this for large or \
                 unfamiliar inputs so the parent transcript holds a handle, not the \
                 body."
            }
            Some("eval") => {
                "Run one Python REPL block against a named RLM context. Returns a \
                 bounded projection of stdout/stderr plus metadata. If the code calls \
                 FINAL/finalize, the final value is stored as a var_handle retrievable \
                 with handle_read (if `handle_read` is not in your tool list, load it with \
                 `tool_search` first) instead of copied unbounded into the parent context. \
                 Large stdout/stderr payloads (>1k chars) are also stored as \
                 var_handles (returned in stdout_handle / stderr_handle) to keep the \
                 parent transcript lean. Batch child helpers require \
                 dependency_mode='independent'; use sub_query_sequence or a \
                 sequential loop for dependent work."
            }
            Some("configure") => {
                "Configure a named RLM context: output feedback, child query timeout, \
                 recursive sub-RLM depth, and explicit session sharing."
            }
            Some("close") => {
                "Close a named RLM context, tear down its Python kernel, and return \
                 usage/lifecycle metadata."
            }
            _ => {
                "Persistent RLM sessions over large contexts. Actions: \"session_objects\" \
                 (list active prompt/history/session symbolic objects as compact cards), \
                 \"open\" (load file_path/content/url/session_object into a named Python \
                 kernel; returns only metadata so the parent transcript holds a handle, \
                 not the body), \"eval\" (run one bounded Python REPL block against a \
                 named context; approval required; FINAL/finalize values and large \
                 stdout/stderr become var_handles retrievable with handle_read; if \
                 `handle_read` is not in your tool list, load it with `tool_search` first), \
                 \"configure\" (output feedback, child timeout, sub-RLM depth, session \
                 sharing), \"close\" (tear down the kernel and return usage metadata)."
            }
        }
    }

    fn input_schema(&self) -> Value {
        if let Some(action) = self.forced_action {
            return legacy_action_schema(action);
        }
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ALL_ACTIONS,
                    "description": "Action to perform."
                },
                "name": {
                    "type": "string",
                    "description": "RLM context name, unique within this parent session (action=open: optional, defaults to a slug from the source). Required for action=eval/configure/close."
                },
                "file_path": {
                    "type": "string",
                    "description": "Workspace-relative file to load (action=open; exactly one of file_path/content/url/session_object)."
                },
                "content": {
                    "type": "string",
                    "description": "Inline content to load. Capped at 200k chars. (action=open)"
                },
                "url": {
                    "type": "string",
                    "description": "HTTP/HTTPS URL to fetch (through the same path as Web action=\"fetch\") and load. (action=open)"
                },
                "session_object": {
                    "type": "string",
                    "description": "Stable symbolic active-session ref from action=session_objects, for example session://active/system_prompt or session://active/messages/0. (action=open)"
                },
                "code": {
                    "type": "string",
                    "description": "Raw Python executed against the context (no markdown fences). The loaded source is in scope as `content`; call FINAL(value)/finalize(...) to return a result handle. Example: print(len(content)). (action=eval)"
                },
                "output_feedback": {
                    "type": "string",
                    "enum": ["full", "metadata"],
                    "description": "(action=configure)"
                },
                "sub_rlm_max_depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 3,
                    "description": "(action=configure)"
                },
                "share_session": {
                    "type": "boolean",
                    "description": "(action=configure)"
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        match self.forced_action {
            Some(action) => Self::action_capabilities(action),
            None => vec![
                ToolCapability::Network,
                ToolCapability::ExecutesCode,
                ToolCapability::RequiresApproval,
            ],
        }
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        match self.forced_action {
            Some(action) if Self::action_requires_approval(action) => ApprovalRequirement::Required,
            Some(_) => ApprovalRequirement::Auto,
            None => ApprovalRequirement::Required,
        }
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match self.resolve_action(input) {
            Ok("open") if rlm_open_source_field(input, "url").is_some() => {
                FetchUrlTool.approval_requirement_for(&json!({"url": input["url"]}))
            }
            Ok("open") => ApprovalRequirement::Auto,
            Ok(action) if Self::action_requires_approval(action) => ApprovalRequirement::Required,
            Ok(_) => ApprovalRequirement::Auto,
            Err(_) => self.approval_requirement(),
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        match self.resolve_action(input) {
            Ok(action) => Self::action_is_read_only(action),
            Err(_) => self.is_read_only(),
        }
    }

    fn supports_parallel(&self) -> bool {
        matches!(self.forced_action, Some("session_objects"))
    }

    fn supports_parallel_for(&self, input: &Value) -> bool {
        matches!(self.resolve_action(input), Ok("session_objects"))
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        match self.resolve_action(&input)? {
            "session_objects" => self.execute_session_objects(context).await,
            "open" => self.execute_open(&input, context).await,
            "eval" => self.execute_eval(&input, context).await,
            "configure" => self.execute_configure(&input, context).await,
            "close" => self.execute_close(&input, context).await,
            action => Err(ToolError::invalid_input(format!(
                "rlm: invalid action `{action}`"
            ))),
        }
    }
}

impl RlmTool {
    async fn execute_session_objects(
        &self,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let snapshot = context.session_objects.as_ref().ok_or_else(|| {
            ToolError::not_available("rlm_session_objects: active session snapshot unavailable")
        })?;
        ToolResult::json(&json!({
            "objects": snapshot.object_cards(),
            "open_with": {
                "tool": "rlm",
                "action": "open",
                "field": "session_object",
                "example": {
                    "name": "active_prompt",
                    "session_object": "session://active/system_prompt"
                }
            },
            "redaction": format!(
                "Large tool results and thinking blocks are represented by compact metadata in transcript objects; use returned handles and handle_read for bounded payload projections ({}).",
                crate::tools::handle::HANDLE_READ_ACTIVATION_HINT
            )
        }))
        .map_err(|e| ToolError::execution_failed(e.to_string()))
    }

    async fn execute_open(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let deadline = context.turn_deadline;
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(ToolError::execution_failed(
                "RLM parent turn deadline exhausted before opening context",
            ));
        }
        let open = async {
            let source_count = rlm_open_source_count(input);
            if source_count != 1 {
                let mut msg = String::from(
                    "rlm_open: provide exactly one of `file_path` (local file), `content` (inline text), `url`, or `session_object`",
                );
                // "did you mean" for common misnamings (#2655).
                if let Some(obj) = input.as_object() {
                    let seen: Vec<&str> = [
                        "prompt",
                        "resident_file",
                        "text",
                        "body",
                        "path",
                        "file",
                        "source",
                    ]
                    .into_iter()
                    .filter(|k| obj.contains_key(*k))
                    .collect();
                    if !seen.is_empty() {
                        msg.push_str(&format!(
                            ". Saw {seen:?} — did you mean file_path/content/url/session_object? (to evaluate against an existing context, pass its name to rlm action='eval', or use `session_object`)"
                        ));
                    }
                }
                return Err(ToolError::invalid_input(msg));
            }

            let (body, source_type, source_hint) = load_source(input, context).await?;
            if body.trim().is_empty() {
                return Err(ToolError::invalid_input(
                    "rlm_open: input is empty after loading",
                ));
            }

            let name = input
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| derive_session_name(source_hint.as_deref()));

            {
                let sessions = context.runtime.rlm_sessions.lock().await;
                if sessions.contains_key(&name) {
                    return Err(ToolError::invalid_input(format!(
                        "rlm_open: context name `{name}` already exists"
                    )));
                }
            }

            let context_path = codewhale_repl::write_context_file(&body).map_err(|e| {
                ToolError::execution_failed(format!("rlm_open: failed to stage context: {e}"))
            })?;
            let context_path = tempfile::TempPath::try_from_path(context_path).map_err(|e| {
                ToolError::execution_failed(format!("rlm_open: failed to own staged context: {e}"))
            })?;
            let kernel = PythonRuntime::spawn_with_context(&context_path)
                .await
                .map_err(|e| ToolError::execution_failed(format!("rlm_open: {e}")))?;
            // PythonRuntime now owns cleanup; a dropped startup kept the TempPath.
            let context_path = context_path.keep().map_err(|e| {
                ToolError::execution_failed(format!("rlm_open: context handoff failed: {e}"))
            })?;
            let context_meta = ContextMeta::from_body(&body, source_type);
            let session = RlmSession::new(name.clone(), kernel, context_meta.clone(), context_path);
            let id = session.id.clone();

            let mut sessions = context.runtime.rlm_sessions.lock().await;
            sessions.insert(name.clone(), Arc::new(tokio::sync::Mutex::new(session)));

            ToolResult::json(&json!({
                "name": name,
                "id": id,
                "length": context_meta.length,
                "type": context_meta.type_name,
                "preview_500": context_meta.preview_500,
                "sha256": context_meta.sha256,
            }))
            .map_err(|e| ToolError::execution_failed(e.to_string()))
        };
        tokio::select! {
            biased;
            () = async {
                if let Some(cancel) = context.cancel_token.as_ref() {
                    cancel.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => Err(ToolError::cancelled("RLM originating turn cancelled while opening context")),
            () = wait_for_deadline(deadline) => Err(ToolError::execution_failed("RLM parent turn deadline exhausted opening context")),
            result = open => result,
        }
    }

    async fn execute_eval(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        crate::tools::execution_envelope::enforce_tool_denial(context, "rlm_eval", input)?;
        let name = required_non_empty_str(input, "name")?;
        let code = required_non_empty_str(input, "code").map_err(|_| {
            ToolError::invalid_input(
                "rlm_eval: `code` is required and runs raw Python against the RLM context (no markdown fences). \
                 Example: {\"name\": \"<ctx>\", \"code\": \"print(len(content))\"}; call FINAL(value) to return a result handle.",
            )
        })?;
        let deadline = context.turn_deadline;
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(ToolError::execution_failed(
                "RLM parent turn deadline exhausted before execution",
            ));
        }
        let original_cancel = context.cancel_token.clone();
        let wait_for_cancel = || async {
            if let Some(cancel) = original_cancel.as_ref() {
                cancel.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let mut session = tokio::select! {
            biased;
            () = wait_for_cancel() => return Err(ToolError::cancelled("RLM originating turn cancelled while waiting for context")),
            () = wait_for_deadline(deadline) => return Err(ToolError::execution_failed("RLM parent turn deadline exhausted waiting for context")),
            result = async {
                let session = get_session(context, name).await?;
                Ok::<_, ToolError>(session.lock_owned().await)
            } => result?,
        };
        let config = session.config.clone();

        if let Some(caller) = context.rlm_caller.as_deref() {
            caller.validate_context(context)?;
        }
        // The active round owns the existing interpreter. Dropping this
        // future kills unknown running code rather than leaving it in the
        // persistent map; only a completed round restores the same kernel.
        let Some(mut kernel) = session.kernel.take() else {
            return Err(ToolError::invalid_input(format!(
                "rlm_eval: context `{name}` is closed"
            )));
        };

        let started = Instant::now();
        let (round, child_usage_batch, nested_events) = if let Some(caller) =
            context.rlm_caller.as_deref()
        {
            let bridge =
                RlmBridge::new(caller, config.sub_rlm_max_depth.min(HARD_SUB_RLM_DEPTH_CAP))
                    .with_deadline(deadline)
                    .with_gate(context.execution.nested_call_gate.clone());
            let round_result = tokio::select! {
                biased;
                () = wait_for_cancel() => Err("RLM evaluation cancelled by its originating turn".into()),
                () = wait_for_deadline(bridge.deadline()) => Err("RLM evaluation reached the parent turn deadline".into()),
                result = kernel.run(code, Some(&bridge)) => result,
            };
            let usage = bridge.usage_snapshot().await;
            let round = match round_result {
                Ok(round) => round,
                Err(error) => {
                    // A bridge request may have completed and accrued usage
                    // before the Python kernel times out or closes stdout.
                    // Return a failed ToolResult (rather than a bare ToolError)
                    // so ToolCallComplete still carries the immutable child
                    // receipt and the runtime can durably account for it.
                    // Cancellation may leave Python running. Discard the
                    // owned interpreter rather than reusing unknown state.
                    session.kernel = None;
                    session.last_used_at = Instant::now();
                    return Ok(rlm_kernel_error_result(
                        &error.to_string(),
                        started.elapsed(),
                        &crate::cost_status::RuntimeUsageBatch {
                            decisions: Vec::new(),
                            records: usage.records,
                            drop_records: usage.drop_records,
                            dropped_records: usage.dropped_records,
                        },
                        &usage.nested_events,
                    ));
                }
            };
            (
                round,
                crate::cost_status::RuntimeUsageBatch {
                    decisions: Vec::new(),
                    records: usage.records,
                    drop_records: usage.drop_records,
                    dropped_records: usage.dropped_records,
                },
                usage.nested_events,
            )
        } else {
            let round = tokio::select! {
                biased;
                () = wait_for_cancel() => return Err(ToolError::cancelled("RLM evaluation cancelled by its originating turn")),
                () = wait_for_deadline(deadline) => return Err(ToolError::execution_failed("RLM evaluation reached the parent turn deadline")),
                result = kernel.run(code, None::<&RlmBridge<'_>>) => result.map_err(|e| ToolError::execution_failed(format!("rlm_eval: {e}")))?,
            };
            (
                round,
                crate::cost_status::RuntimeUsageBatch::default(),
                Vec::new(),
            )
        };

        session.kernel = Some(kernel);
        session.rpc_count = session.rpc_count.saturating_add(round.rpc_count);
        session.total_duration += round.elapsed;
        session.last_used_at = Instant::now();

        let final_handle = if let Some(value_json) = round.final_json.clone() {
            session.final_count = session.final_count.saturating_add(1);
            let handle_name = format!("final_{}", session.final_count);
            let handle = {
                let mut store = context.runtime.handle_store.lock().await;
                match value_json {
                    Value::String(value) => {
                        store.insert_text(session.id.clone(), handle_name, value)
                    }
                    other => store.insert_json(session.id.clone(), handle_name, other),
                }
            };
            Some(handle)
        } else {
            None
        };

        let had_error = round.has_error;
        let rpc_count = round.rpc_count;
        let duration_ms = round.elapsed.as_millis() as u64;
        // Route large stdout/stderr into a var_handle to avoid bloat in
        // the parent transcript. The model calls handle_read for bounded
        // projections; a short inline note describes availability.
        fn route_output(
            text: &str,
            feedback: &OutputFeedback,
            store: &mut crate::tools::handle::HandleStore,
            session_id: &str,
            tag: &str,
        ) -> (Option<String>, Option<crate::tools::handle::VarHandle>) {
            let threshold = STDOUT_HANDLE_THRESHOLD_CHARS;
            match (feedback, text.len()) {
                (OutputFeedback::Full, len) if len <= threshold => {
                    (Some(preview_output(text)), None)
                }
                (OutputFeedback::Full, _) if !text.trim().is_empty() => {
                    // Store full body as a handle for out-of-band retrieval
                    let name = format!("{tag}_{}", 0); // single counter is fine
                    let handle = store.insert_text(session_id, name, text);
                    (
                        Some(format!(
                            "{} chars; retrieve via handle_read ({})",
                            text.len(),
                            crate::tools::handle::HANDLE_READ_ACTIVATION_HINT
                        )),
                        Some(handle),
                    )
                }
                _ => (None, None),
            }
        }

        let (stdout_preview, stdout_handle) = route_output(
            &round.full_stdout,
            &config.output_feedback,
            &mut *context.runtime.handle_store.lock().await,
            &session.id,
            "stdout",
        );
        let (stderr_preview, stderr_handle) = route_output(
            &round.stderr,
            &config.output_feedback,
            &mut *context.runtime.handle_store.lock().await,
            &session.id,
            "stderr",
        );

        let mut output = json!({
            "name": session.name,
            "id": session.id,
            "duration_ms": duration_ms,
            "rpc_count": rpc_count,
            "had_error": had_error,
            "new_vars": [],
            "nested_events": nested_events,
            "final": final_handle,
        });
        if let Some(ref stdout_preview) = stdout_preview {
            output["stdout_preview"] = json!(stdout_preview);
        }
        if let Some(ref stderr_preview) = stderr_preview {
            output["stderr_preview"] = json!(stderr_preview);
        }
        if let (Some(h), Some(_)) = (stdout_handle, &stdout_preview) {
            output["stdout_handle"] = json!(h);
        }
        if let (Some(h), Some(_)) = (stderr_handle, &stderr_preview) {
            output["stderr_handle"] = json!(h);
        }
        if let Some(confidence) = round.final_confidence.clone() {
            output["confidence"] = confidence;
        }

        let mut metadata = json!({
            "tool": "rlm_eval",
            "duration_ms": started.elapsed().as_millis() as u64,
        });
        // Every RLM provider call keeps its own dispatch timestamp and frozen
        // quote. The preferred batch format prevents a fan-out from being
        // retroactively priced as one aggregate call on the first route.
        crate::cost_status::attach_child_usage_batch_metadata(&mut metadata, &child_usage_batch);

        Ok(ToolResult::json(&output)
            .map_err(|e| ToolError::execution_failed(e.to_string()))?
            .with_metadata(metadata))
    }

    async fn execute_configure(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let name = required_non_empty_str(input, "name")?;
        // No cross-session authority exists in this surface. Refuse before
        // mutating any other setting, rather than retaining an inert true bit.
        if input.get("share_session").and_then(Value::as_bool) == Some(true) {
            return Err(ToolError::invalid_input(
                "rlm_configure: share_session=true is unsupported; contexts remain caller-session scoped",
            ));
        }
        let session = get_session(context, name).await?;
        let mut session = session.lock().await;

        if let Some(value) = input.get("output_feedback").and_then(Value::as_str) {
            session.config.output_feedback = match value {
                "full" => OutputFeedback::Full,
                "metadata" => OutputFeedback::Metadata,
                other => {
                    return Err(ToolError::invalid_input(format!(
                        "rlm_configure: invalid output_feedback `{other}`"
                    )));
                }
            };
        }
        if let Some(depth) = input.get("sub_rlm_max_depth").and_then(Value::as_u64) {
            session.config.sub_rlm_max_depth = (depth as u32).min(HARD_SUB_RLM_DEPTH_CAP);
        }
        if input.get("share_session").and_then(Value::as_bool) == Some(false) {
            session.config.share_session = false;
        }

        let mut result = json!({
            "name": session.name,
            "current_config": session.config,
        });
        if input.get("sub_query_timeout_secs").is_some() {
            result["ignored_limit_fields"] = json!(["sub_query_timeout_secs"]);
            result["limits_note"] = json!(
                "Recursive work inherits the owner's deadline; model-selected subquery time limits are retired."
            );
        }
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }

    async fn execute_close(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let name = required_non_empty_str(input, "name")?;
        let removed = {
            let mut sessions = context.runtime.rlm_sessions.lock().await;
            sessions.remove(name)
        };
        let Some(session) = removed else {
            return Err(ToolError::invalid_input(format!(
                "rlm_close: unknown context `{name}`"
            )));
        };

        let mut session = session.lock().await;
        let kernel = session.kernel.take();
        let output = json!({
            "name": session.name,
            "id": session.id,
            "rpc_count": session.rpc_count,
            "total_duration_ms": session.total_duration.as_millis() as u64,
            "peak_var_count": session.peak_var_count,
            "created_ms_ago": session.created_at.elapsed().as_millis() as u64,
            "context_path": session.context_path,
        });
        drop(session);

        if let Some(kernel) = kernel {
            kernel.shutdown().await;
        }

        ToolResult::json(&output).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

/// The exact schema the legacy per-action tool exposed, kept so hidden alias
/// registrations report an identical contract to the pre-unification tools.
fn legacy_action_schema(action: &str) -> Value {
    match action {
        "session_objects" => json!({
            "type": "object",
            "properties": {}
        }),
        "open" => json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Caller-chosen context name, unique within this parent session. Defaults to a slug from the source."
                },
                "file_path": {
                    "type": "string",
                    "description": "Workspace-relative file to load."
                },
                "content": {
                    "type": "string",
                    "description": "Inline content to load. Capped at 200k chars."
                },
                "url": {
                    "type": "string",
                    "description": "HTTP/HTTPS URL to fetch (through the same path as Web action=\"fetch\") and load."
                },
                "session_object": {
                    "type": "string",
                    "description": "Stable symbolic active-session ref from rlm_session_objects, for example session://active/system_prompt or session://active/messages/0."
                }
            }
        }),
        "eval" => json!({
            "type": "object",
            "required": ["name", "code"],
            "properties": {
                "name": { "type": "string", "description": "RLM context name returned by rlm_open." },
                "code": { "type": "string", "description": "Raw Python executed against the context (no markdown fences). The loaded source is in scope as `content`; call FINAL(value)/finalize(...) to return a result handle. Example: print(len(content))." }
            }
        }),
        "configure" => json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" },
                "output_feedback": { "type": "string", "enum": ["full", "metadata"] },
                "sub_rlm_max_depth": { "type": "integer", "minimum": 0, "maximum": 3 },
                "share_session": { "type": "boolean" }
            }
        }),
        // close
        _ => json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "description": "RLM context name from rlm_open." }
            }
        }),
    }
}

async fn load_source(
    input: &Value,
    context: &ToolContext,
) -> Result<(String, String, Option<String>), ToolError> {
    if let Some(path) = rlm_open_source_field(input, "file_path").map(str::trim) {
        let resolved = crate::tools::file::resolve_guarded_read_path(context, path, "rlm_open")?;
        let body = tokio::fs::read_to_string(&resolved).await.map_err(|e| {
            ToolError::execution_failed(format!("rlm_open: read {}: {e}", resolved.display()))
        })?;
        return Ok((body, "file".to_string(), Some(path.to_string())));
    }

    if let Some(content) = rlm_open_source_field(input, "content") {
        if content.chars().count() > MAX_INLINE_CONTENT_CHARS {
            return Err(ToolError::invalid_input(format!(
                "rlm_open: inline content is {} chars (cap {MAX_INLINE_CONTENT_CHARS})",
                content.chars().count()
            )));
        }
        return Ok((content.to_string(), "content".to_string(), None));
    }

    if let Some(object_ref) = rlm_open_source_field(input, "session_object") {
        let snapshot = context.session_objects.as_ref().ok_or_else(|| {
            ToolError::not_available("rlm_open: active session snapshot unavailable")
        })?;
        let object = snapshot.resolve(object_ref).ok_or_else(|| {
            ToolError::invalid_input(format!("rlm_open: unknown session object `{object_ref}`"))
        })?;
        return Ok((
            object.body,
            format!("session_object:{}", object.kind),
            Some(object.id),
        ));
    }

    let url = rlm_open_source_field(input, "url")
        .map(str::trim)
        .ok_or_else(|| ToolError::invalid_input("rlm_open: missing source"))?;
    crate::tools::execution_envelope::enforce_tool_denial(context, "fetch_url", input)?;
    let result = FetchUrlTool
        .execute(json!({"url": url, "format": "raw"}), context)
        .await?;
    let parsed: Value = serde_json::from_str(&result.content).map_err(|e| {
        ToolError::execution_failed(format!("rlm_open: fetch_url returned invalid JSON: {e}"))
    })?;
    let body = parsed
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::execution_failed("rlm_open: fetched body missing content"))?
        .to_string();
    let source_type = parsed
        .get("content_type")
        .and_then(Value::as_str)
        .unwrap_or("url")
        .to_string();
    Ok((body, source_type, Some(url.to_string())))
}

fn rlm_open_source_count(input: &Value) -> usize {
    ["file_path", "content", "url", "session_object"]
        .iter()
        .filter(|field| rlm_open_source_field(input, field).is_some())
        .count()
}

fn rlm_open_source_field<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

async fn get_session(
    context: &ToolContext,
    name: &str,
) -> Result<Arc<tokio::sync::Mutex<RlmSession>>, ToolError> {
    let sessions = context.runtime.rlm_sessions.lock().await;
    sessions.get(name).cloned().ok_or_else(|| {
        ToolError::invalid_input(format!(
            "unknown RLM context `{name}`; open it first with rlm action='open'"
        ))
    })
}

fn required_non_empty_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, ToolError> {
    let value = input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::missing_field(field))?
        .trim();
    if value.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "rlm: `{field}` must not be empty"
        )));
    }
    Ok(value)
}

fn preview_output(text: &str) -> String {
    let total = text.chars().count();
    if total <= FULL_STDOUT_HEAD_CHARS + FULL_STDOUT_TAIL_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(FULL_STDOUT_HEAD_CHARS).collect();
    let tail: String = text
        .chars()
        .skip(total.saturating_sub(FULL_STDOUT_TAIL_CHARS))
        .collect();
    format!(
        "{head}\n... [{} chars truncated, retrieve via handle_read when returned as a handle; {}] ...\n{tail}",
        total.saturating_sub(FULL_STDOUT_HEAD_CHARS + FULL_STDOUT_TAIL_CHARS),
        crate::tools::handle::HANDLE_READ_ACTIVATION_HINT
    )
}

fn _assert_var_handle_shape(_: Option<VarHandle>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rlm::session::SessionObjectSnapshot;
    use crate::tools::handle::HandleReadTool;
    use crate::tools::spec::ToolContext;
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, Message, SystemPrompt};
    use std::path::PathBuf;

    /// #6747: runtime text pointing at the deferred `handle_read` teaches
    /// its activation path and names no hidden tool.
    #[test]
    fn handle_read_pointers_teach_activation_path() {
        let long = "x".repeat(FULL_STDOUT_HEAD_CHARS + FULL_STDOUT_TAIL_CHARS + 10);
        let preview = preview_output(&long);
        let preview_footer = preview
            .lines()
            .find(|line| line.contains("chars truncated"))
            .expect("truncation footer");
        crate::tools::canonical_action_tests::assert_text_names_only_callable_tools(
            "rlm preview footer",
            preview_footer,
        );
        assert!(preview_footer.contains("`tool_search`"));
    }

    fn ctx() -> ToolContext {
        ToolContext::new(".")
    }

    fn ctx_with_session_objects() -> ToolContext {
        ToolContext::new(".").with_session_objects(SessionObjectSnapshot::new(
            "session-1".to_string(),
            "deepseek-v4-pro".to_string(),
            PathBuf::from("."),
            Some(SystemPrompt::Text("You are CodeWhale.".to_string())),
            vec![
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "Please inspect the RLM surface.".to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "I will use symbolic session objects.".to_string(),
                        cache_control: None,
                    }],
                },
            ],
        ))
    }

    #[test]
    fn schema_uses_new_tool_names() {
        assert_eq!(
            RlmTool::alias("rlm_session_objects", "session_objects").name(),
            "rlm_session_objects"
        );
        assert_eq!(RlmTool::alias("rlm_open", "open").name(), "rlm_open");
        assert_eq!(RlmTool::alias("rlm_eval", "eval").name(), "rlm_eval");
        assert_eq!(
            RlmTool::alias("rlm_configure", "configure").name(),
            "rlm_configure"
        );
        assert_eq!(RlmTool::alias("rlm_close", "close").name(), "rlm_close");
    }

    #[test]
    fn rlm_tool_is_compatibility_only_not_model_visible() {
        let canonical = RlmTool::new("rlm");
        assert!(!canonical.model_visible());
        assert_eq!(canonical.name(), "rlm");
        let actions = canonical.input_schema()["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .clone();
        for action in ["session_objects", "open", "eval", "configure", "close"] {
            assert!(
                actions.iter().any(|value| value.as_str() == Some(action)),
                "canonical schema must offer action {action}"
            );
        }

        for alias in [
            RlmTool::alias("rlm_session_objects", "session_objects"),
            RlmTool::alias("rlm_open", "open"),
            RlmTool::alias("rlm_eval", "eval"),
            RlmTool::alias("rlm_configure", "configure"),
            RlmTool::alias("rlm_close", "close"),
        ] {
            assert!(
                !alias.model_visible(),
                "compatibility alias {} must stay hidden",
                alias.name()
            );
        }
    }

    #[test]
    fn kernel_failure_result_retains_child_usage_receipt() {
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            codewhale_config::ProviderKind::Deepseek,
            codewhale_config::ProviderKind::Deepseek.as_str(),
            "deepseek-v4-flash",
            Some(
                codewhale_config::ProviderKind::Deepseek
                    .provider()
                    .default_base_url(),
            ),
            chrono::Utc::now(),
        );
        let usage = codewhale_models::Usage {
            input_tokens: 23,
            output_tokens: 5,
            reasoning_replay_tokens: Some(7),
            ..Default::default()
        };
        let record = crate::cost_status::RuntimeUsageRecord {
            source_id: "rlm:test:request:0".to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: route.clone(),
                usage: usage.clone(),
            },
        };
        let drop_record = crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "rlm:test:request:1".to_string(),
            route: route.clone(),
        };
        let result = rlm_kernel_error_result(
            "kernel stdout closed",
            Duration::from_millis(11),
            &crate::cost_status::RuntimeUsageBatch {
                decisions: Vec::new(),
                records: vec![record],
                drop_records: vec![drop_record],
                dropped_records: 1,
            },
            &[],
        );

        assert!(!result.success);
        let metadata = result
            .metadata
            .expect("usage metadata on failed tool result");
        let batch = crate::cost_status::child_usage_records_from_metadata(&metadata)
            .expect("preferred routed batch");
        assert_eq!(batch.dropped_records, 1);
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.drop_records.len(), 1);
        assert_eq!(batch.records[0].usage.route, route);
        assert_eq!(batch.records[0].usage.usage, usage);
        assert_eq!(batch.drop_records[0].route, route);
    }

    #[test]
    fn rlm_eval_requires_approval() {
        let tool = RlmTool::alias("rlm_eval", "eval");
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Required);
        assert!(
            tool.capabilities()
                .contains(&ToolCapability::RequiresApproval)
        );

        // Evaluation requires approval; concrete open inputs are classified below.
        let canonical = RlmTool::new("rlm");
        assert_eq!(
            canonical.approval_requirement_for(&json!({"action": "eval"})),
            ApprovalRequirement::Required
        );
        assert_eq!(
            canonical.approval_requirement_for(&json!({"action": "open"})),
            ApprovalRequirement::Auto
        );
        assert_eq!(
            canonical.approval_requirement_for(&json!({"action": "session_objects"})),
            ApprovalRequirement::Auto
        );
    }

    #[test]
    fn rlm_open_requires_outbound_approval_but_keeps_local_reads_automatic() {
        for tool in [RlmTool::new("rlm"), RlmTool::alias("rlm_open", "open")] {
            assert_eq!(tool.approval_requirement(), ApprovalRequirement::Required);
            for source in [
                json!({"url": "https://example.com/document"}),
                json!({"url": " https://example.com/document ", "content": ""}),
            ] {
                let mut input = source;
                input["action"] = json!("open");
                assert_eq!(
                    tool.approval_requirement_for(&input),
                    ApprovalRequirement::Required
                );
            }
            for source in [
                json!({"content": "local fixture"}),
                json!({"file_path": "fixture.txt"}),
                json!({"session_object": "fixture-object"}),
                json!({"content": "local fixture", "url": "  "}),
            ] {
                let mut input = source;
                input["action"] = json!("open");
                assert_eq!(
                    tool.approval_requirement_for(&input),
                    ApprovalRequirement::Auto
                );
            }
        }
    }

    #[test]
    fn read_only_and_parallel_flags_match_legacy_contract() {
        // Legacy: session_objects was parallel-friendly read-only; open carried
        // ExecutesCode (not read-only). Open now classifies the concrete source.
        let session_objects = RlmTool::alias("rlm_session_objects", "session_objects");
        assert!(session_objects.supports_parallel());
        assert!(session_objects.is_read_only_for(&json!({})));

        let open = RlmTool::alias("rlm_open", "open");
        assert!(!open.is_read_only_for(&json!({})));
        assert_eq!(open.approval_requirement(), ApprovalRequirement::Required);

        let canonical = RlmTool::new("rlm");
        assert!(canonical.supports_parallel_for(&json!({"action": "session_objects"})));
        assert!(!canonical.supports_parallel_for(&json!({"action": "eval"})));
        assert!(canonical.is_read_only_for(&json!({"action": "configure"})));
        assert!(!canonical.is_read_only_for(&json!({"action": "open"})));
        assert!(!canonical.is_read_only_for(&json!({"action": "eval"})));
    }

    #[test]
    fn canonical_rejects_unknown_or_missing_action() {
        let tool = RlmTool::new("rlm");
        let err = tool
            .resolve_action(&json!({}))
            .expect_err("missing action must fail");
        assert!(err.to_string().contains("missing `action`"));
        let err = tool
            .resolve_action(&json!({"action": "explode"}))
            .expect_err("unknown action must fail");
        assert!(err.to_string().contains("invalid action"));
    }

    #[test]
    fn rlm_open_source_count_ignores_empty_string_defaults() {
        assert_eq!(
            rlm_open_source_count(
                &json!({"name": "url-doc", "file_path": "", "content": "", "url": "https://example.com/doc"})
            ),
            1
        );
        assert_eq!(
            rlm_open_source_count(
                &json!({"name": "inline-doc", "file_path": "", "content": "body", "url": ""})
            ),
            1
        );
        assert_eq!(
            rlm_open_source_count(&json!({"content": "body", "url": "https://example.com/doc"})),
            2
        );
        assert_eq!(
            rlm_open_source_count(
                &json!({"content": "body", "session_object": "session://active/system_prompt"})
            ),
            2
        );
    }

    #[tokio::test]
    async fn rlm_session_objects_lists_active_prompt_object() {
        let ctx = ctx_with_session_objects();
        let result = RlmTool::alias("rlm_session_objects", "session_objects")
            .execute(json!({}), &ctx)
            .await
            .expect("list session objects");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        let objects = body["objects"].as_array().expect("objects array");

        assert!(objects.iter().any(|object| {
            object["id"] == "session://active/system_prompt" && object["kind"] == "system_prompt"
        }));
        assert!(objects.iter().any(|object| {
            object["id"] == "session://active/messages/0" && object["kind"] == "message"
        }));
    }

    #[tokio::test]
    async fn rlm_open_loads_active_session_prompt_object() {
        let ctx = ctx_with_session_objects();
        let open = RlmTool::alias("rlm_open", "open")
            .execute(
                json!({"name": "active_prompt", "session_object": "session://active/system_prompt"}),
                &ctx,
            )
            .await
            .expect("open prompt object");
        let open_json: Value = serde_json::from_str(&open.content).expect("open json");
        assert_eq!(open_json["type"], "session_object:system_prompt");
        assert!(
            open_json["preview_500"]
                .as_str()
                .unwrap()
                .contains("CodeWhale")
        );

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "active_prompt"}), &ctx)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn rlm_open_loads_transcript_message_object() {
        let ctx = ctx_with_session_objects();
        let open = RlmTool::alias("rlm_open", "open")
            .execute(
                json!({"name": "first_message", "session_object": "session://active/messages/0"}),
                &ctx,
            )
            .await
            .expect("open transcript slice");
        let open_json: Value = serde_json::from_str(&open.content).expect("open json");
        assert_eq!(open_json["type"], "session_object:message");
        assert!(
            open_json["preview_500"]
                .as_str()
                .unwrap()
                .contains("RLM surface")
        );

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "first_message"}), &ctx)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn rlm_open_ignores_blank_source_defaults_from_schema_fillers() {
        let ctx = ctx();
        RlmTool::alias("rlm_open", "open")
            .execute(
                json!({"name": "blank-defaults", "file_path": "", "content": "body", "url": ""}),
                &ctx,
            )
            .await
            .expect("open with blank sibling source fields");

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "blank-defaults"}), &ctx)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn rlm_open_misnamed_source_field_gets_did_you_mean_hint() {
        // #2655: a wrong source field name yields actionable guidance, not just
        // the canonical "provide exactly one" message.
        let ctx = ctx();
        let err = RlmTool::alias("rlm_open", "open")
            .execute(json!({"name": "doc", "prompt": "summarize this"}), &ctx)
            .await
            .expect_err("misnamed source field should fail");
        let msg = err.to_string();
        assert!(msg.contains("file_path"), "names the real fields: {msg}");
        assert!(
            msg.contains("`url`, or `session_object`"),
            "names session_object in the valid source field list: {msg}"
        );
        assert!(msg.contains("prompt"), "echoes the wrong field: {msg}");
    }

    #[tokio::test]
    async fn rlm_eval_missing_code_explains_raw_python() {
        // #2655: the missing-code error should teach the tool, with an example.
        let ctx = ctx();
        let err = RlmTool::alias("rlm_eval", "eval")
            .execute(json!({"name": "doc"}), &ctx)
            .await
            .expect_err("missing code should fail");
        let msg = err.to_string();
        assert!(msg.contains("raw Python"), "explains it runs Python: {msg}");
        assert!(
            msg.contains("print(len(content))") || msg.contains("FINAL"),
            "includes an example: {msg}"
        );
    }

    #[test]
    fn rlm_eval_schema_names_the_runtime_content_variable() {
        let schema = RlmTool::alias("rlm_eval", "eval").input_schema();
        let description = schema["properties"]["code"]["description"]
            .as_str()
            .expect("rlm_eval code description");

        assert!(description.contains("`content`"));
        assert!(description.contains("print(len(content))"));
        assert!(!description.contains("SOURCE"));
    }

    #[tokio::test]
    async fn nested_rlm_receipts_survive_tool_result_save_and_reopen_even_on_kernel_failure() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "nested-model-response", "object": "chat.completion",
            "model": "deepseek-v4-flash",
            "choices": [{"index": 0, "message": {"role": "assistant",
                "content": "```repl\nFINAL('durable nested answer')\n```"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 9, "total_tokens": 16}
        }))).expect(2).mount(&server).await;
        let mut config = crate::core::engine::tests::rlm_host::fixture_config("deepseek-v4-flash");
        let identity = config.active_provider_identity().unwrap();
        config.provider_config_for_mut(&identity).unwrap().base_url = Some(server.uri());
        let client = Arc::new(crate::client::CodewhaleClient::new(&config).unwrap());
        let tool = RlmTool::new("rlm");
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        context.execution.nested_call_gate =
            Some(crate::tools::codemode::NestedCallGate::admitting_for_test());
        context = crate::core::engine::tests::rlm_host::context_for_replies(
            &config,
            "deepseek-v4-flash",
            &context,
            client,
        );
        tool.execute(
            json!({"action": "open", "name": "receipts", "content": "fixture context"}),
            &context,
        )
        .await
        .unwrap();
        for (failed, code) in [
            (false, "print(rlm_query('nested context'))"),
            (true, "print(rlm_query('nested context')); _os._exit(2)"),
        ] {
            let result = tool
                .execute(
                    json!({"action": "eval", "name": "receipts", "code": code}),
                    &context,
                )
                .await
                .unwrap();
            assert_eq!(result.success, !failed);
            let output: Value = serde_json::from_str(&result.content).unwrap();
            let events = output["nested_events"]
                .as_array()
                .expect("retained nested events");
            assert_eq!(
                events
                    .iter()
                    .filter(|entry| entry["kind"] == "code")
                    .count(),
                1
            );
            assert!(events.iter().any(|entry| {
                entry["content"]
                    .as_str()
                    .is_some_and(|line| line.contains("FINAL('durable nested answer')"))
            }));
            assert!(events.iter().any(|entry| {
                entry["content"]
                    .as_str()
                    .is_some_and(|line| line.contains("RLM finished: Final"))
            }));
            let messages = vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "rlm-call".into(),
                        name: "rlm".into(),
                        input: json!({"action": "eval", "name": "receipts", "code": code}),
                        execution_id: Some("rlm-execution".into()),
                        caller: None,
                        thought_signature: None,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "rlm-call".into(),
                        execution_id: Some("rlm-execution".into()),
                        content: result.content,
                        is_error: Some(failed),
                        content_blocks: None,
                    }],
                },
            ];
            let manager =
                crate::session_manager::SessionManager::new(temp.path().join("sessions")).unwrap();
            let saved = crate::session_manager::create_saved_session(
                &messages,
                "deepseek-v4-flash",
                temp.path(),
                16,
                None,
            );
            manager.save_session(&saved).unwrap();
            let reopened = manager.load_session(&saved.metadata.id).unwrap();
            let ContentBlock::ToolResult { content, .. } = &reopened.messages[1].content[0] else {
                panic!("saved tool result");
            };
            let replay: Value = serde_json::from_str(content).unwrap();
            assert_eq!(&replay["nested_events"], &output["nested_events"]);
        }
        assert!(
            get_session(&context, "receipts")
                .await
                .unwrap()
                .lock()
                .await
                .kernel
                .is_none(),
            "a failed interpreter must not be reused"
        );
    }

    #[tokio::test]
    async fn persistent_open_obeys_original_cancel_and_deadline_without_installing_a_kernel() {
        let temp = tempfile::tempdir().unwrap();
        let tool = RlmTool::new("rlm");
        for cancelled in [true, false] {
            let mut context = ToolContext::new(temp.path());
            let cancel = tokio_util::sync::CancellationToken::new();
            context.cancel_token = Some(cancel.clone());
            context.turn_deadline =
                (!cancelled).then(|| tokio::time::Instant::now() + Duration::from_millis(40));
            let sessions = context.runtime.rlm_sessions.lock().await;
            let input = json!({"action": "open", "name": "never-installed", "content": "fixture"});
            let mut open = tool.execute(input, &context);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut open)
                    .await
                    .is_err()
            );
            if cancelled {
                cancel.cancel();
            }
            let error = tokio::time::timeout(Duration::from_secs(1), &mut open)
                .await
                .expect("original cancellation and deadline both release a contended open")
                .unwrap_err();
            if cancelled {
                assert!(matches!(error, ToolError::Cancelled { .. }));
            } else {
                assert!(
                    error
                        .to_string()
                        .contains("deadline exhausted opening context")
                );
            }
            assert!(
                sessions.is_empty(),
                "no interpreter is installed while admission waits"
            );
            drop(open);
            drop(sessions);
            assert!(context.runtime.rlm_sessions.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn parent_deadline_bounds_rlm_session_lock_waits() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        let tool = RlmTool::new("rlm");
        tool.execute(
            json!({"action": "open", "name": "contended", "content": "fixture"}),
            &context,
        )
        .await
        .unwrap();
        let session = get_session(&context, "contended").await.unwrap();
        let marker = temp.path().join("must-not-exist");
        let code = format!(
            "open({}, 'w').write('ran')",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        for registry_locked in [true, false] {
            context.turn_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(50));
            let registry_guard = if registry_locked {
                Some(context.runtime.rlm_sessions.lock().await)
            } else {
                None
            };
            let session_guard = if registry_locked {
                None
            } else {
                Some(session.lock().await)
            };
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                tool.execute(
                    json!({"action": "eval", "name": "contended", "code": code}),
                    &context,
                ),
            )
            .await
            .expect("lock waits must obey the parent deadline")
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("deadline exhausted waiting for context")
            );
            assert!(!marker.exists());
            drop(session_guard);
            drop(registry_guard);
        }
        tool.execute(json!({"action": "close", "name": "contended"}), &context)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn expired_parent_deadline_prevents_rlm_python_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = ToolContext::new(temp.path());
        let tool = RlmTool::new("rlm");
        tool.execute(
            json!({"action": "open", "name": "expired", "content": "fixture"}),
            &context,
        )
        .await
        .unwrap();
        context.turn_deadline = Some(tokio::time::Instant::now());
        let marker = temp.path().join("must-not-exist");
        let code = format!(
            "open({}, 'w').write('ran')",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        let error = tool
            .execute(
                json!({"action": "eval", "name": "expired", "code": code}),
                &context,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("deadline exhausted"));
        assert!(!marker.exists());
        tool.execute(json!({"action": "close", "name": "expired"}), &context)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rlm_session_open_eval_close_lifecycle() {
        let ctx = ctx();
        RlmTool::alias("rlm_open", "open")
            .execute(
                json!({"name": "sample", "content": "alpha\nbeta\ngamma"}),
                &ctx,
            )
            .await
            .expect("open");

        let eval = RlmTool::alias("rlm_eval", "eval")
            .execute(json!({"name": "sample", "code": "print('ok')"}), &ctx)
            .await
            .expect("eval");
        let eval_json: Value = serde_json::from_str(&eval.content).expect("eval json");
        let stdout_preview = eval_json["stdout_preview"]
            .as_str()
            .expect("stdout_preview")
            .replace("\r\n", "\n");
        assert_eq!(stdout_preview, "ok\n");

        let close = RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "sample"}), &ctx)
            .await
            .expect("close");
        assert!(close.content.contains("sample"));
    }

    #[tokio::test]
    async fn rlm_canonical_action_routing_runs_full_lifecycle() {
        // The visible surface: one `rlm` tool, action-parameterized.
        let ctx = ctx();
        let tool = RlmTool::new("rlm");
        tool.execute(
            json!({"action": "open", "name": "canonical", "content": "body"}),
            &ctx,
        )
        .await
        .expect("open via canonical action");

        let eval = tool
            .execute(
                json!({"action": "eval", "name": "canonical", "code": "print('ok')"}),
                &ctx,
            )
            .await
            .expect("eval via canonical action");
        let eval_json: Value = serde_json::from_str(&eval.content).expect("eval json");
        let stdout_preview = eval_json["stdout_preview"]
            .as_str()
            .expect("stdout_preview")
            .replace("\r\n", "\n");
        assert_eq!(stdout_preview, "ok\n");

        let close = tool
            .execute(json!({"action": "close", "name": "canonical"}), &ctx)
            .await
            .expect("close via canonical action");
        assert!(close.content.contains("canonical"));
    }

    #[tokio::test]
    async fn rlm_eval_final_returns_handle() {
        let ctx = ctx();
        RlmTool::alias("rlm_open", "open")
            .execute(json!({"name": "finals", "content": "body"}), &ctx)
            .await
            .expect("open");

        let eval = RlmTool::alias("rlm_eval", "eval")
            .execute(
                json!({"name": "finals", "code": "finalize('done', confidence=0.8)"}),
                &ctx,
            )
            .await
            .expect("eval");
        let eval_json: Value = serde_json::from_str(&eval.content).expect("eval json");
        assert_eq!(eval_json["final"]["kind"], "var_handle");
        assert_eq!(eval_json["final"]["name"], "final_1");
        assert_eq!(eval_json["confidence"], 0.8);

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "finals"}), &ctx)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn rlm_eval_final_preserves_json_handle() {
        let ctx = ctx();
        RlmTool::alias("rlm_open", "open")
            .execute(json!({"name": "json-final", "content": "body"}), &ctx)
            .await
            .expect("open");

        let eval = RlmTool::alias("rlm_eval", "eval")
            .execute(
                json!({"name": "json-final", "code": "finalize({'answer': 42, 'items': ['a', 'b']})"}),
                &ctx,
            )
            .await
            .expect("eval");
        let eval_json: Value = serde_json::from_str(&eval.content).expect("eval json");
        assert_eq!(eval_json["final"]["kind"], "var_handle");
        assert_eq!(eval_json["final"]["type"], "dict");
        assert_eq!(eval_json["final"]["length"], 2);

        let read = HandleReadTool
            .execute(
                json!({"handle": eval_json["final"].clone(), "jsonpath": "$.items[*]"}),
                &ctx,
            )
            .await
            .expect("read final handle");
        let read_json: Value = serde_json::from_str(&read.content).expect("read json");
        assert_eq!(read_json["matches"], json!(["a", "b"]));

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "json-final"}), &ctx)
            .await
            .expect("close");
    }

    #[tokio::test]
    async fn rlm_configure_metadata_omits_stdout() {
        let ctx = ctx();
        RlmTool::alias("rlm_open", "open")
            .execute(json!({"name": "quiet", "content": "body"}), &ctx)
            .await
            .expect("open");
        RlmTool::alias("rlm_configure", "configure")
            .execute(
                json!({"name": "quiet", "output_feedback": "metadata", "sub_rlm_max_depth": 99}),
                &ctx,
            )
            .await
            .expect("configure");

        let eval = RlmTool::alias("rlm_eval", "eval")
            .execute(json!({"name": "quiet", "code": "print('hidden')"}), &ctx)
            .await
            .expect("eval");
        let eval_json: Value = serde_json::from_str(&eval.content).expect("eval json");
        assert!(eval_json.get("stdout_preview").is_none());

        RlmTool::alias("rlm_close", "close")
            .execute(json!({"name": "quiet"}), &ctx)
            .await
            .expect("close");
    }
    #[tokio::test]
    async fn shared_session_refusal_is_atomic_and_persistent_kernel_owns_no_caller() {
        let workspace = tempfile::tempdir().unwrap();
        let base = ToolContext::new(workspace.path());
        let model = "captured-kernel-model";
        let config = crate::core::engine::tests::rlm_host::fixture_config(model);
        let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
        let context = crate::core::engine::tests::rlm_host::context_for_replies(
            &config,
            model,
            &base,
            mock.clone(),
        );
        let weak_caller = Arc::downgrade(context.rlm_caller.as_ref().unwrap());
        let sessions = context.runtime.rlm_sessions.clone();
        let tool = RlmTool::new("rlm");
        tool.execute(
            json!({"action":"open", "name":"lifetime", "content":"long local context"}),
            &context,
        )
        .await
        .unwrap();
        let before = serde_json::to_value(
            get_session(&context, "lifetime")
                .await
                .unwrap()
                .lock()
                .await
                .config
                .clone(),
        )
        .unwrap();
        let error = tool.execute(json!({"action":"configure", "name":"lifetime", "share_session":true, "sub_query_timeout_secs":1, "output_feedback":"metadata"}), &context).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("share_session=true is unsupported")
        );
        let after = serde_json::to_value(
            get_session(&context, "lifetime")
                .await
                .unwrap()
                .lock()
                .await
                .config
                .clone(),
        )
        .unwrap();
        assert_eq!(
            before, after,
            "unsupported session authority refuses before any settings change"
        );
        drop(context);
        assert!(
            weak_caller.upgrade().is_none(),
            "persistent kernels must not retain model authority or their borrowed callback"
        );
        assert_eq!(
            sessions.lock().await.len(),
            1,
            "the actual kernel map is still live"
        );
        assert_eq!(mock.call_count(), 0);
        sessions.lock().await.clear();
    }
    #[tokio::test]
    async fn persistent_eval_drop_and_original_cancel_kill_running_kernel_without_retaining_caller()
    {
        for cancel_original in [false, true] {
            let workspace = tempfile::tempdir().unwrap();
            let base = ToolContext::new(workspace.path());
            let model = "captured-kernel-model";
            let config = crate::core::engine::tests::rlm_host::fixture_config(model);
            let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
            let context = crate::core::engine::tests::rlm_host::context_for_replies(
                &config,
                model,
                &base,
                mock.clone(),
            );
            let weak_caller = Arc::downgrade(context.rlm_caller.as_ref().unwrap());
            let cancel = context.cancel_token.as_ref().unwrap().clone();
            let tool = RlmTool::new("rlm");
            tool.execute(
                json!({"action":"open", "name":"running", "content":"local long context"}),
                &context,
            )
            .await
            .unwrap();
            let marker = workspace.path().join("started.txt");
            let effect = workspace.path().join("must-not-run.txt");
            let code = format!(
                "import pathlib, time\nstarted = pathlib.Path({})\nstarted_temporary = started.with_suffix('.tmp')\nstarted_temporary.write_text(_ctx_file)\nstarted_temporary.replace(started)\ntime.sleep(1)\npathlib.Path({}).write_text('effect after cancellation')",
                serde_json::to_string(&marker.to_string_lossy()).unwrap(),
                serde_json::to_string(&effect.to_string_lossy()).unwrap(),
            );
            let input = json!({"action":"eval", "name":"running", "code":code});
            let mut call = Box::pin(tool.execute(input, &context));
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::select! {
                    result = &mut call => panic!("running eval finished before its marker: {result:?}"),
                    () = async {
                        while !marker.exists() {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    } => {},
                }
            }).await.expect("actual persistent Python round starts");
            let context_path = PathBuf::from(std::fs::read_to_string(&marker).unwrap());
            assert!(context_path.exists());
            if cancel_original {
                cancel.cancel();
                let result = tokio::time::timeout(Duration::from_secs(1), &mut call)
                    .await
                    .expect("origin cancellation hands back promptly")
                    .unwrap();
                assert!(!result.success);
                assert!(result.content.contains("cancelled by its originating turn"));
            }
            drop(call);
            let session = get_session(&context, "running").await.unwrap();
            assert!(session.lock().await.kernel.is_none());
            assert!(
                !context_path.exists(),
                "dropped interpreter releases its owned context file"
            );
            drop(context);
            assert!(weak_caller.upgrade().is_none());
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert!(
                !effect.exists(),
                "unknown Python code cannot outlive dropped/cancelled eval"
            );
            assert_eq!(mock.call_count(), 0);
        }
    }
}

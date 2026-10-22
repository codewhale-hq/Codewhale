use super::{Hook, HookCondition, HookEvent, HooksConfig};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use crate::process_tree::ProcessTree;

/// Core-only caller receipt. Not exported as an environment value or accepted from JS.
#[derive(Debug, Clone)]
pub(crate) struct HookCaller {
    pub workspace: PathBuf,
    pub plugins: Option<Arc<crate::plugins::PluginRegistry>>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub origin_turn_id: Option<String>,
    pub origin_call_id: Option<String>,
}
impl HookCaller {
    pub(crate) fn from_tool(context: &crate::tools::spec::ToolContext) -> Self {
        Self {
            workspace: context.workspace.clone(),
            plugins: context.plugin_registry.clone(),
            session_id: context
                .session_objects
                .as_ref()
                .map(|s| s.session_id.clone()),
            agent_id: context.owner_agent_id.clone(),
            origin_turn_id: context.origin_turn_id.clone(),
            origin_call_id: context.origin_tool_call_id.clone(),
        }
    }
}

/// Context passed to hooks via environment variables
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub(crate) canonical_input: Option<serde_json::Value>,
    pub(crate) caller: Option<HookCaller>,
    /// Tool name (for ToolCallBefore/After)
    pub tool_name: Option<String>,
    /// Engine-assigned tool call id, so a `tool_call_before` record and the
    /// matching `tool_call_after` / `on_error` record can be correlated.
    pub tool_call_id: Option<String>,
    /// Tool arguments as JSON string
    pub tool_args: Option<String>,
    /// Tool result output (truncated)
    pub tool_result: Option<String>,
    /// Tool exit code if applicable.
    ///
    /// `i64` end-to-end: a Windows crash code such as `3221225477`
    /// (`0xC0000005`) is a real value `exec_shell` reports, and narrowing it
    /// to `i32` used to discard exactly the failures a hook most wants to see.
    pub tool_exit_code: Option<i64>,
    /// How a process-backed tool ended (`completed`, `failed`, `timed_out`,
    /// `killed`, `running`), when it reported one. A timed-out or killed
    /// command usually has no exit code, so this is how a hook tells it apart
    /// from a tool that reported nothing.
    pub tool_status: Option<String>,
    /// Serialized post-admission shell execution receipt (#6689), exported as
    /// `DEEPSEEK_TOOL_EXECUTION_RECEIPT`. Complete JSON or absent — never a
    /// truncated document — and at most [`HOOK_EXECUTION_RECEIPT_MAX_BYTES`].
    pub tool_execution_receipt: Option<String>,
    /// Whether tool succeeded
    pub tool_success: Option<bool>,
    /// Current mode
    pub mode: Option<String>,
    /// Previous mode (for `ModeChange`)
    pub previous_mode: Option<String>,
    /// Session ID
    pub session_id: Option<String>,
    /// User message content
    pub message: Option<String>,
    /// Error message (for `OnError`)
    pub error_message: Option<String>,
    /// Workspace path
    pub workspace: Option<PathBuf>,
    /// Current model name
    pub model: Option<String>,
    /// Total tokens used
    pub total_tokens: Option<u32>,
    /// Session cost in USD
    pub session_cost: Option<f64>,
}

impl HookContext {
    fn native_payload(
        &self,
        point: &str,
        existing: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let caller = self
            .caller
            .as_ref()
            .ok_or("Native hook has no caller identity")?;
        let mut payload = json!({"session_id":caller.session_id,"cwd":caller.workspace,"transcript_path":"","hook_event_name":point});
        let object = payload.as_object_mut().expect("payload");
        match point {
            "PreToolUse" | "PostToolUse" => {
                object.insert("tool_name".into(), json!(self.tool_name));
                object.insert("tool_use_id".into(), json!(self.tool_call_id));
                object.insert(
                    "tool_input".into(),
                    self.canonical_input
                        .clone()
                        .ok_or("Native hook tool input exceeds 32 KiB or is missing")?,
                );
                if point == "PostToolUse" {
                    object.insert("tool_response".into(), json!(self.tool_result));
                }
            }
            "UserPromptSubmit" => {
                object.insert("prompt".into(), json!(self.message));
            }
            "SessionStart" => {
                object.insert("source".into(), json!("startup"));
            }
            "Stop" => {
                object.insert("stop_hook_active".into(), json!(false));
            }
            "SubagentStart" | "SubagentStop" => {
                let id = existing
                    .and_then(|v| v.get("agent_id"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or("subagent hook has no actual child identity")?;
                object.insert("agent_id".into(), json!(id));
                object.insert("agent_type".into(), json!("general-purpose"));
                if point == "SubagentStop" {
                    object.insert("stop_hook_active".into(), json!(false));
                }
            }
            _ => return Err("unknown Native dialect firepoint".into()),
        }
        if serde_json::to_vec(&payload)
            .map_err(|_| "Native hook payload is not JSON")?
            .len()
            > 32 * 1024
        {
            return Err("Native hook payload exceeds 32 KiB".into());
        }
        Ok(payload)
    }

    pub(crate) fn with_caller(mut self, caller: HookCaller) -> Self {
        self.caller = Some(caller);
        self
    }
    pub(crate) fn with_tool_context(self, context: &crate::tools::spec::ToolContext) -> Self {
        self.with_caller(HookCaller::from_tool(context))
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tool_name(mut self, name: &str) -> Self {
        self.tool_name = Some(name.to_string());
        self
    }

    pub fn with_tool_call_id(mut self, id: &str) -> Self {
        self.tool_call_id = Some(id.to_string());
        self
    }

    pub fn with_tool_args(mut self, args: &serde_json::Value) -> Self {
        self.canonical_input = (args.to_string().len() <= 32 * 1024).then(|| args.clone());
        self.tool_args = Some(truncate_env_value(
            &args.to_string(),
            HOOK_TOOL_ARGS_ENV_MAX_BYTES,
        ));
        self
    }

    pub fn with_tool_result(mut self, result: &str, success: bool, exit_code: Option<i64>) -> Self {
        self.tool_result = Some(truncate_env_value(
            result,
            HOOK_TOOL_RESULT_CONTEXT_MAX_BYTES,
        ));
        self.tool_success = Some(success);
        self.tool_exit_code = exit_code;
        self
    }

    /// Record a settled tool call: its text, success flag, and — when the
    /// tool reported them, on success or failure — its exit code and status.
    /// The TUI and Runtime API completion hooks both build their context here.
    pub fn with_tool_outcome(
        self,
        result: &Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
    ) -> Self {
        let (text, success) = match result {
            Ok(output) => (output.content.clone(), output.success),
            Err(error) => (error.to_string(), false),
        };
        let mut context = self.with_tool_result(&text, success, reported_tool_exit_code(result));
        context.tool_status = reported_tool_status(result).map(str::to_string);
        context.tool_execution_receipt = reported_tool_execution_receipt(result);
        context
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_string());
        self
    }

    pub fn with_previous_mode(mut self, mode: &str) -> Self {
        self.previous_mode = Some(mode.to_string());
        self
    }

    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace = Some(path);
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
        self
    }

    pub fn with_session_id(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn with_message(mut self, message: &str) -> Self {
        self.message = Some(message.to_string());
        self
    }

    pub fn with_error(mut self, error: &str) -> Self {
        self.error_message = Some(truncate_env_value(error, HOOK_ERROR_CONTEXT_MAX_BYTES));
        self
    }

    pub fn with_tokens(mut self, tokens: u32) -> Self {
        self.total_tokens = Some(tokens);
        self
    }

    /// Clamp all observer-owned strings before the context is cloned into a
    /// bounded queue. Builders already apply these limits, but fields remain
    /// public for compatibility, so the submission boundary must defend
    /// itself against a directly-constructed context too.
    fn bounded_for_observer(mut self) -> Self {
        fn bound(value: &mut Option<String>, max_bytes: usize) {
            if let Some(raw) = value.take() {
                *value = Some(truncate_env_value(&raw, max_bytes));
            }
        }

        bound(&mut self.tool_name, HOOK_OBSERVER_METADATA_MAX_BYTES);
        bound(&mut self.tool_call_id, HOOK_OBSERVER_METADATA_MAX_BYTES);
        bound(&mut self.tool_args, HOOK_TOOL_ARGS_ENV_MAX_BYTES);
        bound(&mut self.tool_result, HOOK_TOOL_RESULT_CONTEXT_MAX_BYTES);
        bound(&mut self.mode, HOOK_OBSERVER_METADATA_MAX_BYTES);
        bound(&mut self.previous_mode, HOOK_OBSERVER_METADATA_MAX_BYTES);
        bound(&mut self.session_id, HOOK_OBSERVER_METADATA_MAX_BYTES);
        bound(&mut self.message, HOOK_MESSAGE_CONTEXT_MAX_BYTES);
        bound(&mut self.error_message, HOOK_ERROR_CONTEXT_MAX_BYTES);
        bound(&mut self.model, HOOK_OBSERVER_METADATA_MAX_BYTES);
        // A receipt is complete JSON or nothing: truncating it would export a
        // broken document, so an oversized one is dropped instead.
        if self
            .tool_execution_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.len() > HOOK_EXECUTION_RECEIPT_MAX_BYTES)
        {
            self.tool_execution_receipt = None;
        }
        if let Some(workspace) = self.workspace.take() {
            self.workspace = Some(PathBuf::from(truncate_env_value(
                &workspace.to_string_lossy(),
                HOOK_OBSERVER_METADATA_MAX_BYTES,
            )));
        }
        self
    }

    /// Project the shell's post-admission receipt into the versioned observer
    /// contract. Never reconstruct execution identity from requested arguments.
    fn tool_after_payload(&self) -> Option<serde_json::Value> {
        let tool_name = self.tool_name.as_deref()?;
        if !is_shell_tool_name(tool_name) {
            return None;
        }
        let encoded = self.tool_execution_receipt.as_deref()?;
        if encoded.len() > HOOK_EXECUTION_RECEIPT_MAX_BYTES {
            return None;
        }
        let receipt: serde_json::Value = serde_json::from_str(encoded).ok()?;
        if receipt.get("schema_version")?.as_u64()? != 1
            || receipt.get("scope")?.as_str()? != "local"
            || !matches!(receipt.get("state")?.as_str()?, "completed" | "interrupted")
        {
            return None;
        }
        let completion = self.tool_status.as_deref()?;
        if !matches!(completion, "completed" | "failed" | "killed" | "timed_out") {
            return None;
        }
        let command = receipt.get("command")?.as_str()?;
        let cwd = receipt.get("cwd")?.as_str()?;
        if command.is_empty()
            || command.contains('\0')
            || cwd.contains('\0')
            || !std::path::Path::new(cwd).is_absolute()
        {
            return None;
        }
        let exit_code = receipt.get("exit_code")?;
        if !exit_code.is_null() && exit_code.as_i64().is_none() {
            return None;
        }
        let output_mode = receipt.get("output_kind")?.as_str()?;
        let stdout = receipt.get("stdout")?.as_str()?;
        let stderr = receipt.get("stderr")?.as_str()?;
        if !matches!(output_mode, "separate" | "combined")
            || (output_mode == "combined" && !stderr.is_empty())
        {
            return None;
        }
        // Bound correlation fields before the observer queue clamps its legacy
        // environment context, so every stdin truncation flag remains truthful.
        const ID_MAX_BYTES: usize = 1_024;
        let bounded_id =
            |id: &Option<String>| id.as_deref().map(|s| truncate_env_value(s, ID_MAX_BYTES));
        let clipped_id = |id: &Option<String>| id.as_ref().is_some_and(|s| s.len() > ID_MAX_BYTES);
        let payload = json!({
            "schema_version": 1,
            "event": "tool_call_after",
            "tool_name": tool_name,
            "session_id": bounded_id(&self.session_id),
            "tool_call_id": bounded_id(&self.tool_call_id),
            "session_id_truncated": clipped_id(&self.session_id),
            "tool_call_id_truncated": clipped_id(&self.tool_call_id),
            "tool_name_truncated": false,
            "execution_receipt": {
                "schema_version": 1,
                "command": command,
                "cwd": cwd,
                "command_truncated": false,
                "cwd_truncated": false,
                "execution": "started",
                "completion": completion,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "stdout_truncated": receipt.get("stdout_truncated")?.as_bool()?,
                "stderr_truncated": receipt.get("stderr_truncated")?.as_bool()?,
                "output_mode": output_mode,
            },
        });
        (serde_json::to_vec(&payload).ok()?.len() <= 64 * 1024).then_some(payload)
    }

    /// Convert to environment variables
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        if let Some(ref name) = self.tool_name {
            env.insert("DEEPSEEK_TOOL_NAME".to_string(), name.clone());
        }
        if let Some(ref id) = self.tool_call_id {
            env.insert("CODEWHALE_TOOL_CALL_ID".to_string(), id.clone());
            env.insert("DEEPSEEK_TOOL_CALL_ID".to_string(), id.clone());
        }
        if let Some(ref args) = self.tool_args {
            // Tool arguments can include whole patches or encoded payloads.
            // Keep the diagnostic environment surface bounded just like tool
            // results; hooks that need the canonical arguments already receive
            // the structured tool request at the engine boundary.
            env.insert(
                "DEEPSEEK_TOOL_ARGS".to_string(),
                truncate_env_value(args, HOOK_TOOL_ARGS_ENV_MAX_BYTES),
            );
        }
        if let Some(ref result) = self.tool_result {
            // Truncate result to 10KB to avoid environment variable size limits
            env.insert(
                "DEEPSEEK_TOOL_RESULT".to_string(),
                truncate_env_value(result, 10000),
            );
        }
        if let Some(code) = self.tool_exit_code {
            env.insert("DEEPSEEK_TOOL_EXIT_CODE".to_string(), code.to_string());
        }
        if let Some(success) = self.tool_success {
            env.insert("DEEPSEEK_TOOL_SUCCESS".to_string(), success.to_string());
        }
        if let Some(ref status) = self.tool_status {
            env.insert("DEEPSEEK_TOOL_STATUS".to_string(), status.clone());
        }
        if let Some(ref receipt) = self.tool_execution_receipt
            && receipt.len() <= HOOK_EXECUTION_RECEIPT_MAX_BYTES
        {
            env.insert(
                "DEEPSEEK_TOOL_EXECUTION_RECEIPT".to_string(),
                receipt.clone(),
            );
        }
        if let Some(ref mode) = self.mode {
            env.insert("DEEPSEEK_MODE".to_string(), mode.clone());
        }
        if let Some(ref prev) = self.previous_mode {
            env.insert("DEEPSEEK_PREVIOUS_MODE".to_string(), prev.clone());
        }
        if let Some(ref session_id) = self.session_id {
            env.insert("CODEWHALE_SESSION_ID".to_string(), session_id.clone());
            env.insert("DEEPSEEK_SESSION_ID".to_string(), session_id.clone());
        }
        if let Some(ref message) = self.message {
            // Truncate message to prevent env var issues
            env.insert(
                "DEEPSEEK_MESSAGE".to_string(),
                truncate_env_value(message, 5000),
            );
        }
        if let Some(ref error) = self.error_message {
            // Bounded like every other payload field: a tool failure message
            // can be the whole of a failed command's output, and an unbounded
            // env var is both an exec limit risk and an accidental transcript
            // copy in whatever the hook writes it to.
            env.insert(
                "DEEPSEEK_ERROR".to_string(),
                truncate_env_value(error, 5000),
            );
        }
        if let Some(ref ws) = self.workspace {
            env.insert("DEEPSEEK_WORKSPACE".to_string(), ws.display().to_string());
        }
        if let Some(ref model) = self.model {
            env.insert("DEEPSEEK_MODEL".to_string(), model.clone());
        }
        if let Some(tokens) = self.total_tokens {
            env.insert("DEEPSEEK_TOTAL_TOKENS".to_string(), tokens.to_string());
        }
        if let Some(cost) = self.session_cost {
            env.insert("DEEPSEEK_SESSION_COST".to_string(), format!("{cost:.6}"));
        }

        env
    }
}

/// Clamp a hook environment value to `max_bytes`, on a UTF-8 boundary, with a
/// visible marker so a hook can tell truncation from a short value.
fn truncate_env_value(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let safe_end = value
        .char_indices()
        .take_while(|(i, c)| *i + c.len_utf8() <= max_bytes)
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    format!("{}...[truncated]", &value[..safe_end])
}

/// Result of a hook execution
#[derive(Debug, Clone, Default)]
pub struct HookResult {
    /// Hook name (if specified)
    pub name: Option<String>,
    /// Whether the hook succeeded.
    ///
    /// For a background hook this is `true` as soon as the bounded supervisor
    /// accepts the job: no child outcome has been observed yet. Check
    /// [`Self::background`] before reading this as "the command succeeded".
    pub success: bool,
    /// `true` when this result describes a background submission rather than
    /// a completed run. Background results always carry `exit_code: None`,
    /// empty `stdout`/`stderr`, and a duration that measures the spawn, not
    /// the command.
    pub background: bool,
    /// `true` when the hook behind this result declared
    /// `continue_on_error = false` and ran in the foreground.
    ///
    /// This travels with the *result*, not with the event, because it is the
    /// only way a steering call site can tell "the gate that actually matched
    /// this call could not answer" from "some other, unrelated strict hook for
    /// the same event exists in config". Background submissions are never
    /// strict: nothing is awaited, so there is no answer to withhold.
    pub strict: bool,
    /// Exit code from the hook command
    pub exit_code: Option<i32>,
    /// Standard output
    pub stdout: String,
    /// Standard error
    #[allow(dead_code)] // written by prod constructors, read only in tests
    pub stderr: String,
    /// Time taken to execute
    pub duration: Duration,
    /// Error message if execution failed
    pub error: Option<String>,
}

impl HookResult {
    /// A result that carries an observed exit code, as opposed to a
    /// background submission or a spawn failure.
    ///
    /// Steering paths must gate on this: a background hook's `exit_code` is
    /// `None` because nothing was waited for, not because the command exited
    /// without a code.
    #[must_use]
    pub fn observed_exit_code(&self) -> Option<i32> {
        if self.background {
            return None;
        }
        self.exit_code
    }
}

/// Result of running mutable `message_submit` hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageSubmitOutcome {
    /// No hook changed the submitted text.
    Unchanged { warning: Option<String> },
    /// One or more hooks replaced the submitted text.
    Replaced {
        text: String,
        warning: Option<String>,
    },
    /// A hook intentionally blocked the submission.
    Blocked { reason: String },
}

impl MessageSubmitOutcome {
    pub fn unchanged() -> Self {
        Self::Unchanged { warning: None }
    }

    pub fn replaced(text: String) -> Self {
        Self::Replaced {
            text,
            warning: None,
        }
    }

    fn with_warning(self, warning: Option<String>) -> Self {
        match self {
            Self::Unchanged { .. } => Self::Unchanged { warning },
            Self::Replaced { text, .. } => Self::Replaced { text, warning },
            Self::Blocked { reason } => Self::Blocked { reason },
        }
    }

    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Unchanged { warning } | Self::Replaced { warning, .. } => warning.as_deref(),
            Self::Blocked { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageSubmitStdout {
    Blocked(String),
    Unchanged,
    Replaced(String),
    Invalid(String),
}

/// Maximum characters kept from one text field a `tool_call_before` hook
/// prints (`reason`, `additionalContext`).
///
/// Both fields end up somewhere unbounded output would be a real problem:
/// `reason` in a TUI denial line, `additionalContext` inside the tool result
/// that is sent to the model and counted against the context budget. A hook
/// that prints a megabyte gets a bounded, marked prefix instead.
pub(crate) const HOOK_TEXT_FIELD_MAX_CHARS: usize = 2_000;

/// Maximum characters of concatenated `additionalContext` appended to a single
/// tool result, across every hook that contributed to that one call.
pub(crate) const HOOK_CONTEXT_AGGREGATE_MAX_CHARS: usize = 8_000;

/// Largest tool-argument snapshot exported through `DEEPSEEK_TOOL_ARGS`.
const HOOK_TOOL_ARGS_ENV_MAX_BYTES: usize = 10_000;

/// Largest raw tool result retained in an observer job before enqueue.
const HOOK_TOOL_RESULT_CONTEXT_MAX_BYTES: usize = 10_000;

/// Largest serialized shell execution receipt exported through
/// `DEEPSEEK_TOOL_EXECUTION_RECEIPT`. The shell tool fits its output previews
/// beneath this bound; the hook boundary drops anything larger rather than
/// truncate a JSON document.
pub(crate) const HOOK_EXECUTION_RECEIPT_MAX_BYTES: usize = 32 * 1024;

/// Largest error retained in an observer job before enqueue.
const HOOK_ERROR_CONTEXT_MAX_BYTES: usize = 5_000;

/// Largest user/message preview retained in an observer job before enqueue.
const HOOK_MESSAGE_CONTEXT_MAX_BYTES: usize = 5_000;

/// Largest identifier or other diagnostic retained in an observer job.
const HOOK_OBSERVER_METADATA_MAX_BYTES: usize = 4_096;

/// Largest stdout or stderr prefix retained from one foreground hook. Reader
/// threads continue draining after this cap so a verbose child cannot fill its
/// pipe and deadlock before exit; only the in-memory receipt is clipped.
const HOOK_PIPE_CAPTURE_MAX_BYTES: usize = 64 * 1024;

/// Largest serialized `updatedInput` object accepted from a decision hook.
/// This is intentionally smaller than the pipe cap so the surrounding JSON
/// and other fields still have headroom.
const HOOK_UPDATED_INPUT_MAX_BYTES: usize = 32 * 1024;

/// Largest replacement message accepted from `message_submit`.
const HOOK_MESSAGE_REPLACEMENT_MAX_CHARS: usize = 32_000;

/// Hard ceiling for the complete serialized `message_submit` stdin document.
/// The text prefix is fitted beneath this boundary after bounded metadata has
/// been added, so JSON escaping cannot push a producer past the limit.
pub(crate) const HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES: usize = 32 * 1024;

/// Individual metadata fields in `message_submit` stdin are diagnostic only.
/// Bound them before fitting text so an unusual workspace/model value cannot
/// consume the entire payload budget.
const HOOK_MESSAGE_SUBMIT_METADATA_MAX_BYTES: usize = 4 * 1024;

/// Largest turn error copied into a `turn_end` observer payload.
const HOOK_TURN_ERROR_MAX_CHARS: usize = 2_000;

/// Largest denial reason persisted into UI/model receipts.
const HOOK_DENIAL_RECEIPT_MAX_CHARS: usize = 240;

/// Bound and de-fang text a hook printed before it is shown or sent onward.
///
/// Control characters are removed (`\r`) or flattened to a space so hook
/// stdout cannot repaint the TUI with escape sequences or forge structure in
/// the model-facing transcript; `\n` and `\t` survive because a hook's context
/// is legitimately multi-line. Truncation carries a visible marker so a
/// consumer can tell a clipped value from a short one.
pub(crate) fn sanitize_hook_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut kept = 0usize;
    let mut truncated = false;
    for ch in text.chars() {
        let mapped = match ch {
            '\n' | '\t' => ch,
            '\r' => continue,
            c if c.is_control() => ' ',
            c => c,
        };
        if kept == max_chars {
            truncated = true;
            break;
        }
        out.push(mapped);
        kept += 1;
    }
    if truncated {
        out.push_str("…[truncated]");
    }
    out
}

/// Longest hook/config name kept in a log line, a `/hooks` row, or a receipt.
///
/// Names are operator-supplied and otherwise unbounded: nothing stops a
/// `name` from being a megabyte of ANSI escapes, and it is echoed into the
/// TUI, the tracing stream, and the model-facing denial.
pub(crate) const HOOK_LABEL_MAX_CHARS: usize = 64;

/// [`sanitize_hook_text`], forced onto one line.
///
/// Labels and previews sit inside a formatted row, so an embedded newline or
/// tab would forge structure in the very listing that is supposed to describe
/// the hook. Everything else [`sanitize_hook_text`] does — control-character
/// removal and the marked truncation — still applies.
pub(crate) fn sanitize_hook_line(text: &str, max_chars: usize) -> String {
    sanitize_hook_text(text, max_chars)
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect()
}

/// The display label for a hook, from its optional operator-supplied `name`.
///
/// One line, bounded, control-free, and never empty — every surface that
/// prints a hook name (logs, `/hooks list`, config problems, no-verdict
/// receipts) goes through here so there is one answer to "what can a `name`
/// put on my screen".
pub(crate) fn sanitize_hook_label(name: Option<&str>) -> String {
    let cleaned = name
        .map(|name| sanitize_hook_line(name, HOOK_LABEL_MAX_CHARS))
        .unwrap_or_default();
    if cleaned.trim().is_empty() {
        "(unnamed)".to_string()
    } else {
        cleaned.trim().to_string()
    }
}

#[derive(Clone, Copy)]
enum PendingDenialRedaction {
    AuthorizationSchemeOrCredential,
    SecretValue,
    Command,
    Path,
}

/// Split a denial into whitespace-delimited fields while keeping quoted
/// values together. This makes `command="rm -rf"` and
/// `path='/private folder'` one redaction unit even though the value contains
/// spaces. Unterminated quotes are conservatively kept in the final field.
fn denial_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in line.chars() {
        match (quote, ch) {
            (None, '\'' | '"') => {
                quote = Some(ch);
                current.push(ch);
            }
            (Some(open), close) if open == close => {
                quote = None;
                current.push(ch);
            }
            (None, ch) if ch.is_whitespace() => {
                if !current.is_empty() {
                    fields.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        fields.push(current);
    }
    fields
}

fn denial_field_core(field: &str) -> &str {
    field.trim_matches(|ch: char| {
        matches!(
            ch,
            '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    })
}

fn denial_sensitive_assignment(field: &str) -> Option<(&str, &str)> {
    let core = denial_field_core(field);
    let separator = core.find([':', '='])?;
    let key = denial_field_core(&core[..separator]);
    let value = denial_field_core(&core[separator + 1..]);
    Some((key, value))
}

fn normalized_denial_key(key: &str) -> String {
    denial_field_core(key)
        .chars()
        .map(|ch| match ch {
            '-' | '.' => '_',
            ch => ch.to_ascii_lowercase(),
        })
        .collect()
}

fn denial_key_is_secret(key: &str) -> bool {
    matches!(
        key,
        "token"
            | "secret"
            | "password"
            | "passwd"
            | "api_key"
            | "apikey"
            | "authorization"
            | "bearer"
    ) || key.ends_with("_api_key")
        || key.ends_with("_token")
        || key.ends_with("_secret")
}

/// Render an explicit hook denial without carrying raw process output into a
/// durable transcript. Structured reasons are useful operator copy, but they
/// still pass through a conservative redaction boundary: path-like tokens,
/// command-line flags, and common secret assignments are replaced rather than
/// persisted. Unstructured stdout/stderr never reaches this function.
pub(crate) fn sanitize_hook_denial_reason(reason: &str) -> String {
    let line = sanitize_hook_line(reason, HOOK_DENIAL_RECEIPT_MAX_CHARS);
    let mut redacted = Vec::new();
    let mut pending = None;
    for field in denial_fields(&line) {
        let core = denial_field_core(&field);
        let lower = core.to_ascii_lowercase();

        if matches!(core, "=" | ":") {
            continue;
        }

        if let Some(expected) = pending {
            match expected {
                PendingDenialRedaction::AuthorizationSchemeOrCredential => {
                    redacted.push("[secret]".to_string());
                    // Authorization uses `scheme credentials`. Treat the
                    // first field as a scheme even when it is proprietary;
                    // over-redacting one following field is safer than
                    // leaking a credential for a scheme we do not know.
                    pending = Some(PendingDenialRedaction::SecretValue);
                }
                PendingDenialRedaction::SecretValue => {
                    redacted.push("[secret]".to_string());
                    pending = None;
                }
                PendingDenialRedaction::Command => {
                    redacted.push("[command]".to_string());
                    pending = None;
                }
                PendingDenialRedaction::Path => {
                    redacted.push("[path]".to_string());
                    pending = None;
                }
            }
            continue;
        }

        if let Some((key, value)) = denial_sensitive_assignment(&field) {
            let key = normalized_denial_key(key);
            if denial_key_is_secret(&key) {
                redacted.push("[secret]".to_string());
                pending = if key == "authorization" && value.is_empty() {
                    Some(PendingDenialRedaction::AuthorizationSchemeOrCredential)
                } else if key == "authorization"
                    && !value.chars().any(char::is_whitespace)
                    && !value.contains(':')
                {
                    // A lone assignment value is normally the scheme
                    // (`Authorization=Digest <credential>`). Quoted values
                    // containing whitespace already include both pieces.
                    Some(PendingDenialRedaction::SecretValue)
                } else if value.is_empty() {
                    Some(PendingDenialRedaction::SecretValue)
                } else {
                    None
                };
                continue;
            }
            if matches!(
                key.as_str(),
                "path" | "file" | "directory" | "cwd" | "workspace"
            ) {
                redacted.push("[path]".to_string());
                pending = value.is_empty().then_some(PendingDenialRedaction::Path);
                continue;
            }
            if matches!(key.as_str(), "command" | "cmd" | "argv" | "executable") {
                redacted.push("[command]".to_string());
                pending = value.is_empty().then_some(PendingDenialRedaction::Command);
                continue;
            }
        }

        let secret_prefix = lower.starts_with("sk-")
            || lower.starts_with("ghp_")
            || lower.starts_with("github_pat_");
        let path_like = core.starts_with('/')
            || core.starts_with("~/")
            || core.starts_with("./")
            || core.starts_with("../")
            || core.contains('/')
            || core.contains('\\')
            || core
                .as_bytes()
                .get(1)
                .is_some_and(|separator| *separator == b':');
        let command_flag = core.starts_with('-');
        let label = lower.trim_end_matches([':', '=']);
        if matches!(label, "command" | "cmd" | "argv" | "executable") {
            redacted.push("[command]".to_string());
            pending = Some(PendingDenialRedaction::Command);
        } else if label == "authorization" {
            redacted.push("[secret]".to_string());
            pending = Some(PendingDenialRedaction::AuthorizationSchemeOrCredential);
        } else if matches!(label, "bearer" | "token" | "secret" | "password" | "passwd") {
            redacted.push("[secret]".to_string());
            pending = Some(PendingDenialRedaction::SecretValue);
        } else if matches!(label, "path" | "file" | "directory" | "cwd" | "workspace") {
            redacted.push("[path]".to_string());
            pending = Some(PendingDenialRedaction::Path);
        } else if secret_prefix {
            redacted.push("[secret]".to_string());
        } else if path_like {
            redacted.push("[path]".to_string());
        } else if command_flag {
            redacted.push("[argument]".to_string());
        } else {
            redacted.push(field);
        }
    }
    let rendered = sanitize_hook_line(&redacted.join(" "), HOOK_DENIAL_RECEIPT_MAX_CHARS);
    if rendered.is_empty() {
        "hook denied the action".to_string()
    } else {
        rendered
    }
}

/// Render a foreground hook's failure as a detail string that is safe to show.
///
/// The executor already writes generic errors, but this is the *boundary*, not
/// a restatement of that habit: only the shapes recognized here survive, and
/// each is re-rendered from parts rather than passed through. A future code
/// path that stuffs a command line, a resolved interpreter path, or hook
/// output into `HookResult::error` therefore cannot leak it into a receipt
/// merely by skipping the genericization at the producer — it degrades to the
/// catch-all instead, and the raw text is discarded.
pub(crate) fn generic_unavailable_detail(error: Option<&str>) -> String {
    const GENERIC: &str = "hook returned no verdict";
    let Some(error) = error else {
        return GENERIC.to_string();
    };
    if let Some(rest) = error.strip_prefix("Hook timed out after ") {
        let secs: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return if secs.is_empty() {
            "hook timed out".to_string()
        } else {
            format!("hook timed out after {secs}s")
        };
    }
    if let Some(rest) = error.strip_prefix("hook process could not be started (") {
        // Only the `std::io::ErrorKind` debug name, and only if it really is
        // one: bare ASCII letters, nothing else.
        let kind: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        return if kind.is_empty() {
            "hook process could not be started".to_string()
        } else {
            format!("hook process could not be started ({kind})")
        };
    }
    if error.starts_with("failed to contain hook process tree")
        || error.starts_with("failed to resume contained hook process")
    {
        return "hook process could not be contained".to_string();
    }
    if error.starts_with("hook executor did not run") {
        return "hook executor did not run".to_string();
    }
    if error.starts_with("Failed to submit background hook")
        || error.starts_with("background hook supervisor could not be started")
        || error.starts_with("background hook supervisor queue is full")
        || error.starts_with("background hook supervisor is unavailable")
    {
        return "hook could not be submitted".to_string();
    }
    if error.starts_with("Failed to wait for hook")
        || error.starts_with("hook could not be reaped")
        || error.starts_with("Failed to encode hook stdin")
        || error.starts_with("hook stdout reader could not be started")
        || error.starts_with("hook stderr reader could not be started")
        || error.starts_with("hook stdin writer could not be started")
        || error.starts_with("background hook process could not be started")
        || error.starts_with("background hook stdin writer could not be started")
        || error.starts_with("background hook setup")
    {
        return "hook did not complete cleanly".to_string();
    }
    tracing::debug!(target: "hooks", "hook failure had no recognized shape; reporting it generically");
    GENERIC.to_string()
}

/// [`sanitize_hook_text`], dropping the value entirely when nothing
/// meaningful survives.
fn sanitized_hook_field(text: &str) -> Option<String> {
    let cleaned = sanitize_hook_text(text, HOOK_TEXT_FIELD_MAX_CHARS);
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Parsed stdout from a `tool_call_before` hook (#3026).
///
/// Hooks may emit a JSON decision on stdout:
/// `{"decision": "allow"|"deny"|"ask", "reason": "...",
///   "updatedInput": {...}, "additionalContext": "..."}`
/// Non-JSON or empty stdout → legacy passthrough (allow).
///
/// `reason` and `additional_context` are sanitized and bounded here, at the
/// only door hook stdout comes through, so no downstream consumer has to
/// remember to do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallBeforeStdout {
    pub decision: Option<ToolCallDecision>,
    pub reason: Option<String>,
    pub updated_input: Option<serde_json::Value>,
    pub additional_context: Option<String>,
}

/// Decision a hook can return for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallDecision {
    Allow,
    Deny,
    Ask,
}

pub(crate) fn parse_tool_call_before_stdout(stdout: &str) -> ToolCallBeforeStdout {
    let passthrough = ToolCallBeforeStdout {
        decision: None,
        reason: None,
        updated_input: None,
        additional_context: None,
    };
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return passthrough;
    }
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        // Non-JSON stdout → legacy passthrough (allow).
        Err(_) => return passthrough,
    };
    let Some(obj) = value.as_object() else {
        tracing::warn!(
            "tool_call_before hook stdout is JSON but not an object; \
             ignoring it (legacy passthrough)"
        );
        return passthrough;
    };
    let decision = obj
        .get("decision")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "allow" => Some(ToolCallDecision::Allow),
            "deny" => Some(ToolCallDecision::Deny),
            "ask" => Some(ToolCallDecision::Ask),
            _ => {
                tracing::warn!(
                    "tool_call_before hook returned unrecognized decision \
                     (expected allow|deny|ask); treating as allow"
                );
                None
            }
        });
    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .and_then(sanitized_hook_field);
    let updated_input = obj.get("updatedInput").cloned().filter(|v| {
        if !v.is_object() {
            tracing::warn!("tool_call_before hook updatedInput must be a JSON object; ignoring");
            return false;
        }
        let serialized_len = serde_json::to_vec(v).map_or(usize::MAX, |bytes| bytes.len());
        if serialized_len > HOOK_UPDATED_INPUT_MAX_BYTES {
            tracing::warn!(
                serialized_len,
                max_bytes = HOOK_UPDATED_INPUT_MAX_BYTES,
                "tool_call_before hook updatedInput exceeded the size limit; ignoring"
            );
            return false;
        }
        true
    });
    let additional_context = obj
        .get("additionalContext")
        .and_then(|v| v.as_str())
        .and_then(sanitized_hook_field);
    ToolCallBeforeStdout {
        decision,
        reason,
        updated_input,
        additional_context,
    }
}

/// Post-turn accumulated totals included in the `turn_end` observer payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnEndTotals {
    pub session_tokens: u32,
    pub conversation_tokens: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Input used to build the structured `turn_end` observer payload.
pub struct TurnEndPayloadInput<'a> {
    pub context: &'a HookContext,
    pub created_at: DateTime<Utc>,
    pub model_backed: bool,
    pub provider: Option<&'a str>,
    pub billing_surface: Option<&'a str>,
    pub model: Option<&'a str>,
    pub turn_id: &'a str,
    pub status: &'a str,
    pub error: Option<&'a str>,
    pub duration: Duration,
    pub usage: &'a codewhale_models::Usage,
    pub totals: TurnEndTotals,
    pub tool_count: usize,
    pub queued_message_count: usize,
}

/// Kill a hook's whole process tree (see [`crate::process_tree`]): hooks run
/// through a shell, so killing only the immediate `sh`/`cmd.exe` child can
/// leave the actual hook runtime alive. Falls back to `taskkill /T` on Windows
/// and to the immediate child everywhere.
fn terminate_tree(process_tree: &ProcessTree, child: &mut Child) {
    let result = process_tree.kill();
    #[cfg(windows)]
    let result = result.or_else(|_| kill_windows_process_tree(child.id()));
    if let Err(error) = result {
        tracing::warn!(
            ?error,
            "failed to terminate hook process tree; killing immediate child"
        );
        let _ = child.kill();
    }
}

#[cfg(windows)]
fn kill_windows_process_tree(pid: u32) -> std::io::Result<()> {
    let mut command = Command::new("taskkill");
    crate::utils::suppress_console_window(&mut command);
    let pid = pid.to_string();
    let mut child = command
        .args(["/F", "/T", "/PID", pid.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let status = wait_for_helper_status(&mut child, WINDOWS_TASKKILL_TIMEOUT)?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "taskkill exited with {status}"
        )))
    }
}

#[cfg(any(windows, test))]
fn wait_for_helper_status(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    match child.wait_timeout(timeout)? {
        Some(status) => Ok(status),
        None => {
            let _ = kill_and_reap_immediate_child(child, HOOK_REAP_TIMEOUT);
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "hook helper did not finish within its timeout",
            ))
        }
    }
}

#[cfg(any(windows, test))]
fn kill_and_reap_immediate_child(child: &mut Child, timeout: Duration) -> bool {
    let _ = child.kill();
    matches!(child.wait_timeout(timeout), Ok(Some(_)))
}

/// Spawn a contained hook child.
///
/// Errors returned here are deliberately free of the hook command, the
/// resolved interpreter path, and the OS message: the caller turns them into a
/// user-visible "hook could not answer" receipt, and on Windows a raw spawn
/// error echoes the whole command line back. The detail is logged instead.
fn spawn_hook_child(command: &mut Command) -> std::io::Result<(Child, ProcessTree)> {
    crate::process_tree::spawn_contained_std(command).map_err(|error| {
        tracing::warn!(target: "hooks", %error, "failed to start contained hook process");
        std::io::Error::new(error.kind(), "failed to contain hook process tree")
    })
}

/// A spawn failure rendered without the command, the path, or the OS message.
///
/// The error kind is the useful, non-identifying part (`NotFound`,
/// `PermissionDenied`, …); everything else is logged, not surfaced.
fn spawn_failure_message(error: &std::io::Error) -> String {
    format!("hook process could not be started ({:?})", error.kind())
}

const OBSERVER_DISPATCH_QUEUE_CAPACITY: usize = 32;
const OBSERVER_DISPATCH_WORKERS: usize = 2;

#[derive(Debug, Clone, Copy)]
enum ObserverDispatchFailure {
    Full,
    Disconnected,
}

/// Bounded, persistent submission path for observer-only events.
///
/// The terminal loop never creates a thread per event. Two long-lived workers
/// drain a fixed-capacity channel, and `try_send` makes saturation observable
/// without ever parking the caller.
#[derive(Clone)]
struct ObserverDispatcher {
    sender: Option<SyncSender<ObserverJob>>,
    #[cfg(test)]
    held_receiver: Option<Arc<Mutex<Receiver<ObserverJob>>>>,
}

impl fmt::Debug for ObserverDispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObserverDispatcher")
            .field("available", &self.sender.is_some())
            .finish_non_exhaustive()
    }
}

impl ObserverDispatcher {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(OBSERVER_DISPATCH_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));

        for worker_index in 0..OBSERVER_DISPATCH_WORKERS {
            let worker_receiver = Arc::clone(&receiver);
            let spawned = std::thread::Builder::new()
                .name(format!("hook-observer-{worker_index}"))
                .spawn(move || observer_worker_loop(worker_receiver));
            if let Err(error) = spawned {
                tracing::warn!(
                    target: "hooks",
                    worker_index,
                    error_kind = ?error.kind(),
                    "failed to start observer hook dispatcher"
                );
                // Dropping the only sender disconnects any workers that did
                // start. A partially-created pool is not presented as healthy.
                drop(sender);
                return Self {
                    sender: None,
                    #[cfg(test)]
                    held_receiver: None,
                };
            }
        }

        Self {
            sender: Some(sender),
            #[cfg(test)]
            held_receiver: None,
        }
    }

    fn submit(&self, event: HookEvent, job: ObserverJob) -> Result<(), String> {
        let Some(sender) = &self.sender else {
            return Err(observer_dispatch_failure_message(
                event,
                ObserverDispatchFailure::Disconnected,
            ));
        };
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(observer_dispatch_failure_message(
                event,
                ObserverDispatchFailure::Full,
            )),
            Err(TrySendError::Disconnected(_)) => Err(observer_dispatch_failure_message(
                event,
                ObserverDispatchFailure::Disconnected,
            )),
        }
    }
}

fn observer_dispatch_failure_message(event: HookEvent, failure: ObserverDispatchFailure) -> String {
    match failure {
        ObserverDispatchFailure::Full => format!(
            "{} observer hook queue is full; event was not submitted",
            event.as_str()
        ),
        ObserverDispatchFailure::Disconnected => format!(
            "{} observer hook dispatcher is unavailable; event was not submitted",
            event.as_str()
        ),
    }
}

enum ObserverJob {
    Environment {
        hooks: HookExecutor,
        event: HookEvent,
        context: HookContext,
    },
    Json {
        hooks: HookExecutor,
        event: HookEvent,
        context: HookContext,
        payload: serde_json::Value,
    },
}

impl ObserverJob {
    fn run(self) {
        let policy = match &self {
            Self::Environment { hooks, .. } | Self::Json { hooks, .. } => hooks.native_policy,
        };
        let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
        match self {
            Self::Environment {
                hooks,
                event,
                context,
            } => {
                let _ = hooks.execute(event, &context);
            }
            Self::Json {
                hooks,
                event,
                context,
                payload,
            } => {
                let _ = hooks.execute_json_observer(event, &context, &payload);
            }
        }
    }
}

fn observer_worker_loop(receiver: Arc<Mutex<Receiver<ObserverJob>>>) {
    loop {
        let received = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => {
                tracing::warn!(target: "hooks", "observer hook dispatcher lock was poisoned");
                return;
            }
        };
        match received {
            Ok(job) => job.run(),
            Err(_) => return,
        }
    }
}

const BACKGROUND_SUPERVISOR_QUEUE_CAPACITY: usize = 32;
const BACKGROUND_SUPERVISOR_WORKERS: usize = 2;

#[derive(Debug, Clone, Copy)]
enum BackgroundSupervisorFailure {
    Full,
    Disconnected,
}

/// Bounded pool that owns background-child setup, timeout, tree kill, and
/// reap. Observer workers enqueue here instead of creating one detached
/// supervisor thread per invocation.
#[derive(Clone)]
struct BackgroundSupervisor {
    sender: Option<SyncSender<BackgroundHookJob>>,
    #[cfg(test)]
    held_receiver: Option<Arc<Mutex<Receiver<BackgroundHookJob>>>>,
}

impl fmt::Debug for BackgroundSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackgroundSupervisor")
            .field("available", &self.sender.is_some())
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisor {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(BACKGROUND_SUPERVISOR_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));

        for worker_index in 0..BACKGROUND_SUPERVISOR_WORKERS {
            let worker_receiver = Arc::clone(&receiver);
            let spawned = std::thread::Builder::new()
                .name(format!("hook-supervisor-{worker_index}"))
                .spawn(move || background_supervisor_worker_loop(worker_receiver));
            if let Err(error) = spawned {
                tracing::warn!(
                    target: "hooks",
                    worker_index,
                    error_kind = ?error.kind(),
                    "failed to start background hook supervisor pool"
                );
                drop(sender);
                return Self {
                    sender: None,
                    #[cfg(test)]
                    held_receiver: None,
                };
            }
        }

        Self {
            sender: Some(sender),
            #[cfg(test)]
            held_receiver: None,
        }
    }

    fn submit(&self, job: BackgroundHookJob) -> Result<(), BackgroundSupervisorFailure> {
        let Some(sender) = &self.sender else {
            return Err(BackgroundSupervisorFailure::Disconnected);
        };
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(BackgroundSupervisorFailure::Full),
            Err(TrySendError::Disconnected(_)) => Err(BackgroundSupervisorFailure::Disconnected),
        }
    }
}

struct AdmittedBackground {
    executor: HookExecutor,
    hook: Hook,
    env: HashMap<String, String>,
    input: Option<serde_json::Value>,
}
struct BackgroundHookJob {
    admitted: Option<AdmittedBackground>,
    command: String,
    env: HashMap<String, String>,
    working_dir: PathBuf,
    stdin_bytes: Option<Vec<u8>>,
    label: String,
    timeout: Duration,
    plugin_authority: Option<crate::plugins::types::PluginAuthority>,
    project_authority: Option<super::authority::ProjectHookAuthority>,
}

impl BackgroundHookJob {
    fn run(mut self) {
        if let Some(job) = self.admitted.take() {
            let result = job
                .executor
                .execute_sync_inner(&job.hook, &job.env, job.input.as_ref());
            if !result.success {
                tracing::warn!(target:"hooks",hook=%self.label,"admitted background hook failed");
            }
            return;
        }
        if crate::plugins::activation::extension_host_policy_enabled() {
            tracing::warn!(target:"hooks",hook=%self.label,"legacy background hook was withdrawn by enabled host policy");
            return;
        }
        let Self {
            admitted: _,
            command: command_text,
            env,
            working_dir,
            stdin_bytes,
            label,
            timeout,
            plugin_authority,
            project_authority,
        } = self;
        if let Err(error) = super::authority::verify_hook_authorities(
            plugin_authority.as_ref(),
            project_authority.as_ref(),
        ) {
            tracing::warn!(
                target: "hooks",
                hook = %label,
                error = %error,
                "denied queued hook after authority changed"
            );
            return;
        }
        let timeout_secs = timeout.as_secs();
        let mut command = HookExecutor::build_shell_command(&command_text);
        command
            .current_dir(&working_dir)
            .envs(&env)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Always pipe stdin so dropping it delivers EOF through shell
            // wrappers even when there is no structured payload.
            .stdin(Stdio::piped());

        let (mut child, process_tree) = match spawn_hook_child(&mut command) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    error_kind = ?error.kind(),
                    "failed to start background hook"
                );
                return;
            }
        };

        let _stdin_writer = match (stdin_bytes, child.stdin.take()) {
            (Some(bytes), Some(stdin)) => match spawn_stdin_writer(stdin, bytes) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    tracing::warn!(
                        target: "hooks",
                        hook = %label,
                        error_kind = ?error.kind(),
                        "failed to start background hook stdin writer"
                    );
                    terminate_and_reap(Some(label.as_str()), &mut child, process_tree);
                    return;
                }
            },
            _ => None,
        };

        match child.wait_timeout(timeout) {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::warn!(
                        target: "hooks",
                        hook = %label,
                        exit_code = ?status.code(),
                        "background hook exited non-zero"
                    );
                }
            }
            Ok(None) => {
                let reaped = terminate_and_reap(Some(label.as_str()), &mut child, process_tree);
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    timeout_secs,
                    reaped,
                    "background hook timed out; process tree killed"
                );
            }
            Err(error) => {
                terminate_and_reap(Some(label.as_str()), &mut child, process_tree);
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    ?error,
                    "failed to wait for background hook; process tree killed"
                );
            }
        }
    }
}

fn background_supervisor_worker_loop(receiver: Arc<Mutex<Receiver<BackgroundHookJob>>>) {
    loop {
        let received = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => {
                tracing::warn!(target: "hooks", "background supervisor lock was poisoned");
                return;
            }
        };
        match received {
            Ok(job) => job.run(),
            Err(_) => return,
        }
    }
}

/// Executor for running hooks
#[derive(Clone)]
pub struct HookExecutor {
    config: HooksConfig,
    default_working_dir: PathBuf,
    session_id: String,
    observer_dispatcher: ObserverDispatcher,
    background_supervisor: BackgroundSupervisor,
    caller: Option<HookCaller>,
    fire_context: Option<Arc<HookContext>>,
    cancel: Option<tokio_util::sync::CancellationToken>,
    host_manager: Arc<crate::extension_host::ExtensionHostManager>,
    engine_handle: Option<tokio::runtime::Handle>,
    native_policy: bool,
    #[cfg(test)]
    lose_message_submit_executor: bool,
}

impl fmt::Debug for HookExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookExecutor")
            .field("config", &self.config)
            .field("default_working_dir", &self.default_working_dir)
            .field("caller", &self.caller)
            .finish_non_exhaustive()
    }
}
impl HookExecutor {
    /// Foreground hooks run under this executor stop, and are reaped, when
    /// `token` is cancelled; hooks not yet started are skipped.
    #[must_use]
    pub(crate) fn with_cancel(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    }

    /// One Engine scheduler, propagated through the existing std workers.
    pub(crate) fn bind_caller(&self, caller: HookCaller) -> Self {
        let mut bound = self.clone();
        bound.caller = Some(caller);
        bound.native_policy = crate::plugins::activation::extension_host_policy_enabled();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            bound.engine_handle = Some(handle);
        }
        bound
    }
    fn event_hooks(&self, event: HookEvent, context: &HookContext) -> Vec<Hook> {
        let mut hooks: Vec<_> = self
            .config
            .hooks_for_event(event)
            .into_iter()
            .cloned()
            .collect();
        if self.native_policy
            && let Some(caller) = context.caller.as_ref().or(self.caller.as_ref())
        {
            hooks.extend(self.host_manager.shell_hooks(caller, event));
        }
        hooks
    }
    fn for_context(&self, context: &HookContext) -> Self {
        let mut bound = context
            .caller
            .clone()
            .map_or_else(|| self.clone(), |caller| self.bind_caller(caller));
        if let Some(caller) = bound.caller.as_mut() {
            caller.origin_call_id = context
                .tool_call_id
                .clone()
                .or(caller.origin_call_id.clone());
        }
        let mut fire = context.clone();
        fire.caller = bound.caller.clone();
        bound.fire_context = Some(Arc::new(fire));
        bound
    }

    fn build_shell_command(command: &str) -> Command {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            let mut cmd = Command::new("cmd");
            const CREATE_SUSPENDED: u32 = 0x0000_0004;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
            // raw_arg: cmd.exe does not parse the CRT-style \" escapes that
            // Command::arg would insert, so pass the command line verbatim.
            cmd.arg("/C").raw_arg(command);
            // Only this call's context may supply a receipt. In particular,
            // a Codewhale launched from another hook must not inherit one.
            cmd.env_remove("DEEPSEEK_TOOL_EXECUTION_RECEIPT");
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                cmd.process_group(0);
            }
            // Only this call's context may supply a receipt. In particular,
            // a Codewhale launched from another hook must not inherit one.
            cmd.env_remove("DEEPSEEK_TOOL_EXECUTION_RECEIPT");
            cmd
        }
    }

    /// Create a new `HookExecutor` with configuration.
    ///
    /// This mints the hook session identity for the whole TUI session. Call it
    /// **once per launch**; every later reload (workspace switch, trust
    /// onboarding) must go through [`Self::rebind`] so the id every hook has
    /// already seen stays valid. Regenerating it mid-session would break
    /// correlation for anything that grouped records by `CODEWHALE_SESSION_ID`
    /// or its `DEEPSEEK_SESSION_ID` compatibility alias.
    pub fn new(config: HooksConfig, default_working_dir: PathBuf) -> Self {
        // Generate a session ID
        let session_id = format!("sess_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        Self {
            config,
            default_working_dir,
            session_id,
            observer_dispatcher: ObserverDispatcher::new(),
            background_supervisor: BackgroundSupervisor::new(),
            caller: None,
            fire_context: None,
            cancel: None,
            host_manager: crate::extension_host::manager(),
            engine_handle: tokio::runtime::Handle::try_current().ok(),
            native_policy: crate::plugins::activation::extension_host_policy_enabled(),
            #[cfg(test)]
            lose_message_submit_executor: false,
        }
    }

    /// Rebuild the executor with new configuration and working directory while
    /// preserving the session identity minted at launch.
    ///
    /// Used when the workspace changes or a trust decision makes project hooks
    /// eligible: the hook set may change, the session does not.
    #[must_use]
    pub fn rebind(&self, config: HooksConfig, default_working_dir: PathBuf) -> Self {
        Self {
            config,
            default_working_dir: default_working_dir.clone(),
            session_id: self.session_id.clone(),
            observer_dispatcher: self.observer_dispatcher.clone(),
            background_supervisor: self.background_supervisor.clone(),
            caller: self
                .caller
                .clone()
                .filter(|c| c.workspace == default_working_dir),
            fire_context: None,
            cancel: self.cancel.clone(),
            host_manager: Arc::clone(&self.host_manager),
            engine_handle: self.engine_handle.clone(),
            native_policy: self.native_policy,
            #[cfg(test)]
            lose_message_submit_executor: self.lose_message_submit_executor,
        }
    }

    /// Create a disabled `HookExecutor` (no hooks will run)
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            config: HooksConfig {
                enabled: false,
                ..Default::default()
            },
            default_working_dir: PathBuf::from("."),
            session_id: String::new(),
            observer_dispatcher: ObserverDispatcher::new(),
            background_supervisor: BackgroundSupervisor::new(),
            caller: None,
            fire_context: None,
            cancel: None,
            host_manager: crate::extension_host::manager(),
            engine_handle: tokio::runtime::Handle::try_current().ok(),
            native_policy: crate::plugins::activation::extension_host_policy_enabled(),
            #[cfg(test)]
            lose_message_submit_executor: false,
        }
    }

    /// Check if hooks are enabled
    #[cfg(test)]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the session ID
    /// Read-only access to the underlying configuration. Used by
    /// `/hooks` (#460 read-only MVP) so the user can list configured
    /// hooks without reaching for `cat ~/.deepseek/config.toml`.
    pub fn config(&self) -> &HooksConfig {
        &self.config
    }

    /// The workspace hooks run in unless a hook names its own directory.
    #[must_use]
    pub fn default_working_dir(&self) -> &std::path::Path {
        &self.default_working_dir
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Cheap pre-check: are there any enabled hooks for this event?
    /// Lets call sites avoid building a [`HookContext`] (which allocates
    /// for `workspace`, `model`, `session_id`, …) on every tool call
    /// when the user hasn't configured any hooks. The cost matters
    /// because `ToolCallBefore` / `ToolCallAfter` fire from
    /// `tool_routing.rs` on every tool dispatch (#455).
    #[must_use]
    pub fn has_hooks_for_event(&self, event: HookEvent) -> bool {
        self.config.enabled
            && (self.config.hooks.iter().any(|h| h.event == event)
                || (self.native_policy && self.host_manager.has_shell_hooks(event)))
    }

    /// Check if there are any background hooks configured for a specific event.
    ///
    /// Background hooks fire and forget — their `exit_code` is always `None`,
    /// so they cannot deny tool calls. This is a known limitation; the check
    /// is used to warn operators when a `ToolCallBefore` hook is configured
    /// as background but expects to block a tool.
    #[must_use]
    pub fn has_background_hooks_for_event(&self, event: HookEvent) -> bool {
        if !self.config.enabled {
            return false;
        }
        self.config
            .hooks
            .iter()
            .any(|h| h.event == event && h.background)
    }

    /// Sanitized labels of the strict foreground gates that *would* run for
    /// this event and context.
    ///
    /// "Strict" is `continue_on_error = false` on a foreground hook: an
    /// operator instruction that the action must not proceed without this
    /// hook's answer. The caller collects these **before** dispatching the
    /// executor so it can still honor them if the execution itself is lost —
    /// a panicked or cancelled `spawn_blocking` returns no results at all, and
    /// an empty result set is indistinguishable from "every hook allowed it".
    ///
    /// Condition matching is the same predicate [`Self::execute`] uses, so
    /// this never names a hook that would not have run: a strict `write_file`
    /// gate has no say over an `exec_shell` call it never matched.
    #[must_use]
    pub fn matched_strict_gate_labels(
        &self,
        event: HookEvent,
        context: &HookContext,
    ) -> Vec<String> {
        if !self.config.enabled {
            return Vec::new();
        }
        self.event_hooks(event, context)
            .into_iter()
            .filter(|hook| {
                // A background hook is never awaited, so it is not a gate no
                // matter what `continue_on_error` says.
                let foreground = !hook.background || !hook.event.honors_background();
                foreground && !hook.continue_on_error && self.matches_condition(hook, context)
            })
            .map(|hook| sanitize_hook_label(hook.name.as_deref()))
            .collect()
    }

    /// Run configured `message_submit` hooks as a mutable submit pipeline.
    ///
    /// This is deliberately separate from [`Self::execute`]: most hook events
    /// are observer-only, while `message_submit` has a narrow stdout JSON
    /// contract that can replace or block the submitted text.
    pub fn execute_message_submit_transform(
        &self,
        context: &HookContext,
        original_text: &str,
    ) -> MessageSubmitOutcome {
        let bound = self.for_context(context);
        let this = &bound;
        if !self.config.enabled {
            return MessageSubmitOutcome::unchanged();
        }

        let hooks = this.event_hooks(HookEvent::MessageSubmit, context);
        if hooks.is_empty() {
            return MessageSubmitOutcome::unchanged();
        }

        let mut current_text = original_text.to_string();
        let mut warning = None;

        for hook in &hooks {
            let hook_context = context.clone().with_message(&current_text);
            if !self.matches_condition(hook, &hook_context) {
                continue;
            }

            let env_vars = hook_context.to_env_vars();
            let payload = message_submit_payload(&hook_context, &current_text);
            if hook.background {
                // A background `message_submit` hook cannot steer, but it must
                // still receive the documented stdin payload — the contract is
                // the same JSON, only the steering is dropped.
                let submitted = this.execute_background_with_stdin(hook, &env_vars, &payload);
                // Submission itself can fail (thread spawn refused, payload not
                // encodable). Discarding that silently is the one outcome an
                // operator cannot debug: the hook is configured, nothing runs,
                // and nothing says so. Still non-blocking — the submit proceeds.
                if !submitted.success {
                    tracing::warn!(
                        target: "hooks",
                        hook = %sanitize_hook_label(submitted.name.as_deref()),
                        event = "message_submit",
                        error = %generic_unavailable_detail(submitted.error.as_deref()),
                        "background message_submit hook was not submitted; it will not run"
                    );
                }
                continue;
            }

            let result = this.execute_sync_with_stdin(hook, &env_vars, &payload);

            if result.exit_code == Some(2) {
                return MessageSubmitOutcome::Blocked {
                    reason: message_submit_block_reason(
                        &result,
                        "message_submit hook blocked submission",
                    ),
                };
            }

            if !result.success {
                let label = sanitize_hook_label(result.name.as_deref());
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    event = "message_submit",
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    detail = %generic_unavailable_detail(result.error.as_deref()),
                    "message_submit hook failed"
                );

                if hook.continue_on_error {
                    warning = message_submit_continue_warning(&result).or(warning);
                    continue;
                }

                return MessageSubmitOutcome::Blocked {
                    reason: message_submit_block_reason(
                        &result,
                        "message_submit hook failed and blocked submission",
                    ),
                };
            }

            match parse_message_submit_stdout(&result.stdout) {
                MessageSubmitStdout::Blocked(reason) => {
                    return MessageSubmitOutcome::Blocked { reason };
                }
                MessageSubmitStdout::Unchanged => {}
                MessageSubmitStdout::Replaced(text) => {
                    current_text = text;
                }
                MessageSubmitStdout::Invalid(reason) => {
                    tracing::warn!(
                        target: "hooks",
                        hook = %sanitize_hook_label(result.name.as_deref()),
                        event = "message_submit",
                        reason = %reason,
                        "ignored invalid message_submit hook stdout"
                    );
                }
            }
        }

        if current_text == original_text {
            MessageSubmitOutcome::unchanged().with_warning(warning)
        } else {
            MessageSubmitOutcome::replaced(current_text).with_warning(warning)
        }
    }

    /// Dispatch-bound entry point for the mutable submit gate.
    ///
    /// Keeping this wrapper distinct gives the production dispatch path a
    /// deterministic test seam for a lost blocking task. Normal hook tests use
    /// [`Self::execute_message_submit_transform`] directly.
    pub(crate) fn execute_message_submit_transform_for_dispatch(
        &self,
        context: &HookContext,
        original_text: &str,
    ) -> MessageSubmitOutcome {
        #[cfg(test)]
        if self.lose_message_submit_executor {
            panic!("injected message_submit executor loss");
        }
        self.execute_message_submit_transform(context, original_text)
    }

    #[cfg(test)]
    pub(crate) fn inject_message_submit_executor_loss_for_test(&mut self) {
        self.lose_message_submit_executor = true;
    }

    #[cfg(test)]
    pub(crate) fn inject_observer_dispatch_full_for_test(&mut self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.observer_dispatcher.sender = Some(sender);
        // Keep the receiver connected but deliberately leave no worker waiting
        // on it. The production `try_send` path therefore returns `Full`.
        self.observer_dispatcher.held_receiver = Some(Arc::new(Mutex::new(receiver)));
    }

    #[cfg(test)]
    pub(crate) fn inject_observer_dispatch_disconnect_for_test(&mut self) {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        self.observer_dispatcher.sender = Some(sender);
        self.observer_dispatcher.held_receiver = None;
    }

    #[cfg(test)]
    fn inject_background_supervisor_full_for_test(&mut self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.background_supervisor.sender = Some(sender);
        self.background_supervisor.held_receiver = Some(Arc::new(Mutex::new(receiver)));
    }

    /// Run every `ShellEnv` hook for this context and merge their stdout
    /// (`KEY=VALUE\n` lines) into a single env-var map. Used by the
    /// `exec_shell` tool to inject ephemeral credentials, per-skill PATH
    /// adjustments, etc. (#456). Failures don't abort the shell call —
    /// the hook simply contributes no vars and a `tracing::warn!` lands.
    ///
    /// Each successful hook's keys (NOT values) are written to the audit
    /// log so a session can be reconciled later without leaking the
    /// secret material itself.
    pub fn collect_shell_env(&self, context: &HookContext) -> HashMap<String, String> {
        let bound = self.for_context(context);
        let this = &bound;
        let mut merged: HashMap<String, String> = HashMap::new();
        if !self.config.enabled {
            return merged;
        }
        let hooks = this.event_hooks(HookEvent::ShellEnv, context);
        if hooks.is_empty() {
            return merged;
        }
        let env_vars = context.to_env_vars();
        for hook in &hooks {
            if !self.matches_condition(hook, context) {
                continue;
            }
            // ShellEnv hooks must be synchronous — their stdout is the contract.
            let result = this.execute_sync(hook, &env_vars);
            if !result.success {
                tracing::warn!(
                    target: "hooks",
                    hook = %sanitize_hook_label(result.name.as_deref()),
                    event = "shell_env",
                    exit_code = ?result.exit_code,
                    detail = %generic_unavailable_detail(result.error.as_deref()),
                    "shell_env hook failed; contributing no env vars"
                );
                continue;
            }
            let parsed = parse_env_lines(&result.stdout);
            if parsed.is_empty() {
                continue;
            }
            // Audit-log the *keys* — never the values.
            crate::audit::log_sensitive_event(
                "shell_env_hook",
                serde_json::json!({
                    // Bounded and de-fanged like every other rendering of a
                    // hook name: an audit record is read by a person, often
                    // through `tail`, where a raw escape sequence still acts.
                    "hook": sanitize_hook_label(result.name.as_deref()),
                    "tool": context.tool_name,
                    "keys": parsed.keys().cloned().collect::<Vec<_>>(),
                }),
            );
            // Later hooks override earlier ones. Documented behavior.
            merged.extend(parsed);
        }
        merged
    }

    /// Execute all hooks for an event
    pub fn execute(&self, event: HookEvent, context: &HookContext) -> Vec<HookResult> {
        let bound = self.for_context(context);
        let this = &bound;
        if !self.config.enabled {
            return Vec::new();
        }

        let hooks = this.event_hooks(event, context);
        if hooks.is_empty() {
            // Fast path: no hooks for this event → skip the
            // `context.to_env_vars()` HashMap allocation. With
            // `tool_call_before` / `tool_call_after` firing per-tool
            // (#455) this allocation would otherwise happen on every
            // tool dispatch even for users with zero hooks configured.
            return Vec::new();
        }
        if event == HookEvent::ToolCallAfter
            && let Some(payload) = context.tool_after_payload()
        {
            return self.execute_json_observer(event, context, &payload);
        }
        let env_vars = context.to_env_vars();
        let mut results = Vec::new();

        for hook in &hooks {
            if this.cancelled() {
                break;
            }
            if !self.matches_condition(hook, context) {
                continue;
            }

            let result = if hook.background {
                this.execute_background(hook, &env_vars)
            } else {
                this.execute_sync(hook, &env_vars)
            };

            // Log failures via tracing so operators tailing
            // `deepseek` with `RUST_LOG=warn` can see hook errors
            // without instrumenting each call site. Successful runs
            // log nothing (would be too noisy on per-tool events).
            if !result.success {
                let label = sanitize_hook_label(result.name.as_deref());
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    event = event.as_str(),
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    detail = %generic_unavailable_detail(result.error.as_deref()),
                    "hook failed"
                );
            }

            let should_continue = result.success || hook.continue_on_error;
            results.push(result);

            if !should_continue {
                break;
            }
        }

        results
    }

    /// Execute observer hooks with a structured JSON stdin payload.
    ///
    /// Unlike `message_submit`, stdout is deliberately ignored by callers:
    /// these hooks are lifecycle observers and cannot mutate or block the
    /// underlying action.
    pub fn execute_json_observer(
        &self,
        event: HookEvent,
        context: &HookContext,
        payload: &serde_json::Value,
    ) -> Vec<HookResult> {
        let bound = self.for_context(context);
        let this = &bound;
        if !self.config.enabled {
            return Vec::new();
        }

        let hooks = this.event_hooks(event, context);
        if hooks.is_empty() {
            return Vec::new();
        }

        let env_vars = context.to_env_vars();
        let mut results = Vec::new();
        for hook in &hooks {
            if !self.matches_condition(hook, context) {
                continue;
            }

            let result = if hook.background {
                this.execute_background_with_stdin(hook, &env_vars, payload)
            } else {
                this.execute_sync_with_stdin(hook, &env_vars, payload)
            };

            if !result.success {
                let label = sanitize_hook_label(result.name.as_deref());
                tracing::warn!(
                    target: "hooks",
                    hook = %label,
                    event = event.as_str(),
                    exit_code = ?result.exit_code,
                    duration_ms = result.duration.as_millis() as u64,
                    detail = %generic_unavailable_detail(result.error.as_deref()),
                    "observer hook failed"
                );
            }

            results.push(result);
        }

        results
    }

    /// Submit an observer event without waiting on foreground child processes
    /// from the caller's thread. The outer worker is fallible and the failure
    /// is returned to the UI; silently dropping a configured observer is not a
    /// truthful fire-and-forget contract.
    pub fn submit_observer(&self, event: HookEvent, context: HookContext) -> Result<(), String> {
        if !self.has_hooks_for_event(event) {
            return Ok(());
        }
        if event == HookEvent::ToolCallAfter
            && let Some(payload) = context.tool_after_payload()
        {
            return self.submit_json_observer(event, context, payload);
        }
        self.observer_dispatcher.submit(
            event,
            ObserverJob::Environment {
                hooks: self.clone(),
                event,
                context: context.bounded_for_observer(),
            },
        )
    }

    /// Structured-payload counterpart to [`Self::submit_observer`].
    pub fn submit_json_observer(
        &self,
        event: HookEvent,
        context: HookContext,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        if !self.has_hooks_for_event(event) {
            return Ok(());
        }
        self.observer_dispatcher.submit(
            event,
            ObserverJob::Json {
                hooks: self.clone(),
                event,
                context: context.bounded_for_observer(),
                payload,
            },
        )
    }

    /// Check whether a tool name matches a condition pattern with `*` glob support.
    ///
    /// DOCS-04: MCP-scoped patterns match on the owning MCP server, not on the
    /// `mcp_` name prefix. The model calls a server tool by
    /// [`crate::mcp::McpPool::mcp_model_tool_name`] (`mcp_<server>_<tool>`),
    /// while the documented spelling is `mcp__<server>__<tool>`; both are
    /// accepted. Any glob starting with `mcp_`, and any `mcp__` pattern, only
    /// ever selects tools a server owns, so the built-in MCP helpers such as
    /// `mcp_read_resource` are reachable by exact name only.
    ///
    /// Known limit: ownership is read from the model name, not the live pool,
    /// so `mcp__<server>__…` splits at the first `__` (a server whose name
    /// contains `__` needs the `mcp_<server>_…` spelling), and a server tool
    /// whose model name collides with a helper name is treated as the helper.
    fn tool_name_matches_condition(tool_name: &str, pattern: &str) -> bool {
        if tool_name == pattern {
            return true;
        }
        // The shell tool is spelled `bash` / `Bash` on the model surface, and
        // `exec_shell` is still stamped for the `shell_env` event and lives
        // on in older hook configs. Treat the three as one tool, matching
        // `tool_category_for`, so a condition written with any spelling fires.
        if is_shell_tool_name(tool_name) && is_shell_tool_name(pattern) {
            return true;
        }
        if let Some(rest) = pattern.strip_prefix("mcp_") {
            let documented = rest.strip_prefix('_');
            if documented.is_some() || pattern.contains('*') {
                if !is_mcp_server_tool(tool_name) {
                    return false;
                }
                let model_pattern = match documented {
                    Some(rest) => match rest.split_once("__") {
                        Some((server, tool)) => {
                            crate::mcp::McpPool::mcp_model_tool_name(server, tool)
                        }
                        None => format!("mcp_{rest}"),
                    },
                    None => pattern.to_string(),
                };
                return Self::glob_matches(tool_name, &model_pattern);
            }
        }
        Self::glob_matches(tool_name, pattern)
    }

    fn glob_matches(tool_name: &str, pattern: &str) -> bool {
        if !pattern.contains('*') {
            return tool_name == pattern;
        }
        // #6208: the pattern is fixed by configuration while this runs once per
        // hook per tool-call/stop event, so compile it once and reuse it rather
        // than building a fresh `Regex` on every event.
        codewhale_execpolicy::matcher::compiled_glob(pattern)
            .is_some_and(|re| re.is_match(tool_name))
    }

    /// Check if a hook's condition matches the context
    #[allow(clippy::only_used_in_recursion)]
    fn matches_condition(&self, hook: &Hook, context: &HookContext) -> bool {
        match &hook.condition {
            None | Some(HookCondition::Always) => true,
            Some(HookCondition::ToolName { name }) => {
                // #3026: Support `*` globs in tool_name conditions so
                // `mcp__*` matches every tool an MCP server owns (DOCS-04:
                // not the built-in `mcp_*` helpers). Exact names keep working.
                context
                    .tool_name
                    .as_ref()
                    .is_some_and(|n| Self::tool_name_matches_condition(n, name))
            }
            Some(HookCondition::ToolCategory { category }) => {
                let tool_category = context
                    .tool_name
                    .as_deref()
                    .map(|name| tool_category_for(name, context.tool_args.as_deref()));
                tool_category.is_some_and(|c| c == category.as_str())
            }
            Some(HookCondition::Mode { mode }) => context
                .mode
                .as_ref()
                .is_some_and(|m| m.eq_ignore_ascii_case(mode)),
            Some(HookCondition::ExitCode { code }) => context.tool_exit_code == Some(*code),
            Some(HookCondition::All { conditions }) => conditions.iter().all(|c| {
                self.matches_condition(
                    &Hook {
                        condition: Some(c.clone()),
                        ..hook.clone()
                    },
                    context,
                )
            }),
            Some(HookCondition::Any { conditions }) => conditions.iter().any(|c| {
                self.matches_condition(
                    &Hook {
                        condition: Some(c.clone()),
                        ..hook.clone()
                    },
                    context,
                )
            }),
        }
    }

    /// Execute a hook synchronously
    fn execute_sync(&self, hook: &Hook, env_vars: &HashMap<String, String>) -> HookResult {
        self.execute_sync_inner(hook, env_vars, None)
    }

    /// Execute a hook synchronously with a structured JSON stdin payload.
    ///
    /// Used by mutable `message_submit` hooks. Existing observer hooks keep the
    /// stdin-less [`Self::execute_sync`] path so their behavior is unchanged.
    fn execute_sync_with_stdin(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: &serde_json::Value,
    ) -> HookResult {
        self.execute_sync_inner(hook, env_vars, Some(stdin_json))
    }

    fn execute_sync_inner(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: Option<&serde_json::Value>,
    ) -> HookResult {
        let _policy = crate::plugins::activation::PolicyScope::propagate(self.native_policy);
        if !self.native_policy {
            if hook.native_shell.is_some() {
                return unavailable_hook(hook, "Native shell hook is disabled");
            }
            return self.execute_sync_legacy(hook, env_vars, stdin_json, self.cancel.as_ref());
        }
        let Some(caller) = self.caller.clone() else {
            return unavailable_hook(hook, "hook caller receipt is unavailable");
        };
        let Some(handle) = self.engine_handle.clone() else {
            return unavailable_hook(hook, "hook Engine scheduler is unavailable");
        };
        let manager = Arc::clone(&self.host_manager);
        manager.bind_engine_handle(handle.clone());
        let executor = self.clone();
        let owned_hook = hook.clone();
        let env = env_vars.clone();
        let input = if let Some(native) = hook.native_shell.as_ref() {
            let Some(context) = self.fire_context.as_ref() else {
                return unavailable_hook(hook, "Native hook firepoint context is missing");
            };
            match context.native_payload(&native.point, stdin_json) {
                Ok(input) => Some(input),
                Err(reason) => return unavailable_hook(hook, &reason),
            }
        } else {
            stdin_json.cloned()
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let policy = self.native_policy;
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        let query = if matches!(
            hook.event,
            HookEvent::SubagentSpawn | HookEvent::SubagentComplete
        ) {
            "general-purpose".into()
        } else if hook.event == HookEvent::SessionStart {
            "startup".into()
        } else {
            env_vars
                .get("DEEPSEEK_TOOL_NAME")
                .cloned()
                .unwrap_or_default()
        };
        let turn_cancel = self.cancel.clone();
        let work = handle.spawn(async move {
            let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
            #[cfg(test)]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let timeout = Duration::from_secs(executor.effective_timeout_secs(&owned_hook));
            let hook_for_run = owned_hook.clone();
            let run = manager.execute_hook(caller, owned_hook, timeout, query, move |cancel| {
                let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
                executor.execute_sync_legacy(&hook_for_run, &env, input.as_ref(), Some(&cancel))
            });
            let result = match turn_cancel {
                Some(turn_cancel) => tokio::select! {
                    result = run => result,
                    () = turn_cancel.cancelled() => Err("hook cancelled with its turn".into()),
                },
                None => run.await,
            };
            let _ = sender.send(result);
        });
        // Never block an Engine/UI runtime worker. Its synchronous callsites already use
        // existing observer/blocking dispatch; ShellEnv's async caller is migrated below.
        let _engine_task = work;
        receiver
            .recv()
            .unwrap_or_else(|_| Err("hook Engine task was lost".into()))
            .unwrap_or_else(|reason| unavailable_hook(hook, &reason))
    }

    fn execute_sync_legacy(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: Option<&serde_json::Value>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> HookResult {
        let started = Instant::now();
        if let Err(reason) = super::authority::verify_hook(hook) {
            return HookResult {
                name: hook.name.clone(),
                background: false,
                strict: !hook.continue_on_error,
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration: started.elapsed(),
                error: Some(format!("Hook authority was denied: {reason}")),
            };
        }
        let working_dir = self
            .config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.default_working_dir.clone());

        let timeout_secs = self.effective_timeout_secs(hook);
        let timeout = Duration::from_secs(timeout_secs);
        // This path always runs the hook in the foreground and awaits it, so
        // `continue_on_error = false` is a live "do not proceed without my
        // answer" for whichever call this result belongs to.
        let strict = !hook.continue_on_error;

        let stdin_bytes = match stdin_json.map(serde_json::to_vec).transpose() {
            Ok(bytes) => bytes,
            Err(e) => {
                return HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Failed to encode hook stdin: {e}")),
                };
            }
        };

        let mut command = Self::build_shell_command(&hook.command);
        if hook
            .native_shell
            .as_ref()
            .is_some_and(|n| n.dialect == "claude-code")
        {
            command.env("CLAUDE_PROJECT_DIR", &self.default_working_dir);
        }
        command
            .current_dir(&working_dir)
            .envs(env_vars)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A closed pipe is a portable EOF signal through shell layers.
            // Windows cmd/PowerShell can reopen console input when handed
            // NUL, so Stdio::null() is not sufficient when the parent test or
            // terminal still owns a live stdin handle.
            .stdin(Stdio::piped());

        if self.native_policy {
            crate::child_env::apply_to_command(
                &mut command,
                crate::child_env::string_map_env(env_vars),
            );
            if hook
                .native_shell
                .as_ref()
                .is_some_and(|native| native.dialect == "claude-code")
            {
                command.env("CLAUDE_PROJECT_DIR", &self.default_working_dir);
            }
            if !env_vars.contains_key("DEEPSEEK_TOOL_EXECUTION_RECEIPT") {
                command.env_remove("DEEPSEEK_TOOL_EXECUTION_RECEIPT");
            }
        }
        let (mut child, process_tree) = match spawn_hook_child(&mut command) {
            Ok(child) => child,
            Err(e) => {
                // Generic on purpose: this string reaches the deny receipt and
                // the TUI, and a spawn error can otherwise echo the resolved
                // command line or interpreter path back to the transcript.
                tracing::warn!(
                    target: "hooks",
                    hook = %sanitize_hook_label(hook.name.as_deref()),
                    error = %e,
                    "failed to start hook process"
                );
                return HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(spawn_failure_message(&e)),
                };
            }
        };

        let stdout_reader = match child
            .stdout
            .take()
            .map(|pipe| spawn_pipe_reader(pipe, "hook-stdout-reader"))
            .transpose()
        {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(
                    target: "hooks",
                    hook = %sanitize_hook_label(hook.name.as_deref()),
                    error_kind = ?error.kind(),
                    "failed to start hook stdout reader"
                );
                terminate_and_reap(hook.name.as_deref(), &mut child, process_tree);
                return HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some("hook stdout reader could not be started".to_string()),
                };
            }
        };
        let stderr_reader = match child
            .stderr
            .take()
            .map(|pipe| spawn_pipe_reader(pipe, "hook-stderr-reader"))
            .transpose()
        {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(
                    target: "hooks",
                    hook = %sanitize_hook_label(hook.name.as_deref()),
                    error_kind = ?error.kind(),
                    "failed to start hook stderr reader"
                );
                terminate_and_reap(hook.name.as_deref(), &mut child, process_tree);
                let _ = collect_reader(stdout_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                return HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some("hook stderr reader could not be started".to_string()),
                };
            }
        };
        let _stdin_writer = match (stdin_bytes, child.stdin.take()) {
            (Some(bytes), Some(stdin)) => match spawn_stdin_writer(stdin, bytes) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    tracing::warn!(
                        target: "hooks",
                        hook = %sanitize_hook_label(hook.name.as_deref()),
                        error_kind = ?error.kind(),
                        "failed to start hook stdin writer"
                    );
                    terminate_and_reap(hook.name.as_deref(), &mut child, process_tree);
                    let _ = collect_reader(stdout_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                    let _ = collect_reader(stderr_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                    return HookResult {
                        name: hook.name.clone(),
                        background: false,
                        strict,
                        success: false,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration: started.elapsed(),
                        error: Some("hook stdin writer could not be started".to_string()),
                    };
                }
            },
            _ => None,
        };

        let deadline = Instant::now().checked_add(timeout);
        let waited = loop {
            if cancel.is_some_and(|token| token.is_cancelled()) {
                break Ok(None);
            }
            let remaining = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            if remaining.is_zero() {
                break Ok(None);
            }
            match child.wait_timeout(remaining.min(Duration::from_millis(25))) {
                Ok(None) => continue,
                result => break result,
            }
        };
        match waited {
            Ok(Some(status)) => {
                drop(process_tree);
                HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: status.success(),
                    exit_code: status.code(),
                    stdout: collect_reader(stdout_reader, HOOK_PIPE_DRAIN_TIMEOUT),
                    stderr: collect_reader(stderr_reader, HOOK_PIPE_DRAIN_TIMEOUT),
                    duration: started.elapsed(),
                    error: None,
                }
            }
            Ok(None) => {
                let reaped = terminate_and_reap(hook.name.as_deref(), &mut child, process_tree);
                let _ = collect_reader(stdout_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                let _ = collect_reader(stderr_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(
                        if reaped && cancel.is_some_and(|token| token.is_cancelled()) {
                            "Hook cancelled with its turn".to_string()
                        } else if reaped {
                            format!("Hook timed out after {timeout_secs}s")
                        } else {
                            // The gate still did not answer, and now we also cannot
                            // prove the process is gone. Say the weaker thing.
                            "hook could not be reaped after its timeout".to_string()
                        },
                    ),
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "hooks",
                    hook = %sanitize_hook_label(hook.name.as_deref()),
                    error = %e,
                    "failed to wait for hook process"
                );
                terminate_and_reap(hook.name.as_deref(), &mut child, process_tree);
                let _ = collect_reader(stdout_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                let _ = collect_reader(stderr_reader, HOOK_PIPE_SHUTDOWN_TIMEOUT);
                HookResult {
                    name: hook.name.clone(),
                    background: false,
                    strict,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    // Generic on purpose, like the spawn path: an OS wait error
                    // can name the child and reaches the deny receipt.
                    error: Some("Failed to wait for hook".to_string()),
                }
            }
        }
    }

    /// Execute a hook in the background (non-blocking)
    fn execute_background(&self, hook: &Hook, env_vars: &HashMap<String, String>) -> HookResult {
        self.execute_background_inner(hook, env_vars, None)
    }

    fn execute_background_with_stdin(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: &serde_json::Value,
    ) -> HookResult {
        self.execute_background_inner(hook, env_vars, Some(stdin_json))
    }

    fn execute_background_inner(
        &self,
        hook: &Hook,
        env_vars: &HashMap<String, String>,
        stdin_json: Option<&serde_json::Value>,
    ) -> HookResult {
        let started = Instant::now();
        if let Err(reason) = super::authority::verify_hook(hook) {
            return HookResult {
                name: hook.name.clone(),
                background: true,
                strict: false,
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration: started.elapsed(),
                error: Some(format!("Hook authority was denied: {reason}")),
            };
        }
        let working_dir = self
            .config
            .working_dir
            .clone()
            .unwrap_or_else(|| self.default_working_dir.clone());

        let stdin_bytes = match stdin_json.map(serde_json::to_vec).transpose() {
            Ok(bytes) => bytes,
            Err(e) => {
                return HookResult {
                    name: hook.name.clone(),
                    background: true,
                    strict: false,
                    success: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: started.elapsed(),
                    error: Some(format!("Failed to encode hook stdin: {e}")),
                };
            }
        };
        let submission = self.background_supervisor.submit(BackgroundHookJob {
            admitted: self.native_policy.then(|| AdmittedBackground {
                executor: self.clone(),
                hook: hook.clone(),
                env: env_vars.clone(),
                input: stdin_json.cloned(),
            }),
            command: hook.command.clone(),
            env: env_vars.clone(),
            working_dir,
            stdin_bytes,
            label: sanitize_hook_label(hook.name.as_deref()),
            timeout: Duration::from_secs(self.effective_timeout_secs(hook)),
            plugin_authority: hook.plugin_authority.clone(),
            project_authority: hook.project_authority.clone(),
        });

        // The result describes the bounded submission, not the run: no caller
        // can mistake "queued" for "exited 0".
        HookResult {
            name: hook.name.clone(),
            background: true,
            strict: false,
            success: submission.is_ok(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration: started.elapsed(),
            error: submission.err().map(|failure| match failure {
                BackgroundSupervisorFailure::Full => {
                    "background hook supervisor queue is full".to_string()
                }
                BackgroundSupervisorFailure::Disconnected => {
                    "background hook supervisor is unavailable".to_string()
                }
            }),
        }
    }

    /// The timeout actually applied to a hook, foreground or background.
    ///
    /// `[hooks].default_timeout_secs` *replaces* the per-hook value when set;
    /// that is the shipped behavior and is documented as such in
    /// `docs/HOOKS.md`.
    fn effective_timeout_secs(&self, hook: &Hook) -> u64 {
        self.config.effective_timeout_secs(hook)
    }
}

fn unavailable_hook(hook: &Hook, reason: &str) -> HookResult {
    HookResult {
        name: hook.name.clone(),
        success: false,
        background: false,
        strict: !hook.continue_on_error,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        duration: Duration::ZERO,
        error: Some(reason.into()),
    }
}

/// Whether `name` is a tool some MCP server owns, as opposed to one of the
/// built-in MCP helpers the TUI itself registers (`McpPool::is_mcp_tool`
/// counts both). Server tools are named by `McpPool::mcp_model_tool_name`.
fn is_mcp_server_tool(name: &str) -> bool {
    name.starts_with("mcp_")
        && !matches!(
            name,
            "mcp_read_resource"
                | "mcp_get_prompt"
                | "list_mcp_resources"
                | "list_mcp_resource_templates"
                | "read_mcp_resource"
        )
}

/// The spellings of the one shell tool (see `tool_category_for`).
fn is_shell_tool_name(name: &str) -> bool {
    matches!(name, "bash" | "Bash" | "exec_shell")
}

/// Classify a tool call for `condition = { type = "tool_category", … }`.
///
/// Categories are `shell`, `file_write`, `safe`, and `other`, as documented in
/// `docs/HOOKS.md`. This must be kept in step with the names the registry
/// actually registers: before 2026-08-04 the map knew only the retired
/// `exec_shell`/`write_file`/`read_file` spellings, so EVERY live call fell
/// through to `other` and a `tool_category` **deny** hook silently never
/// fired — the exact failure `docs/HOOKS.md` warns about ("a deny gate the
/// operator believes is armed").
///
/// `File`, `Git`, and `Run` are multi-action, so the action decides the
/// category: a `File` read is `safe` while a `File` write is `file_write`.
/// An unparseable or absent argument blob is treated as the tool's most
/// dangerous action, because a gate that cannot see the action must not
/// assume the harmless one.
fn tool_category_for(tool_name: &str, tool_args: Option<&str>) -> &'static str {
    let action = tool_args
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase)
        });

    match tool_name {
        // The shell surface. `exec_shell` is retired but kept here because
        // `shell.rs` still stamps it for the `shell_env` hook event.
        name if is_shell_tool_name(name) => "shell",
        // The lowercase primitives ship without an action envelope.
        "read" | "todo_write" => "safe",
        "write" | "edit" => "file_write",
        "File" | "file" => match action.as_deref() {
            Some("read" | "list" | "search_name" | "search_content") => "safe",
            // write/edit/patch, and the unknown-action case, are writes.
            _ => "file_write",
        },
        "apply_patch" => "file_write",
        "Git" | "git" => match action.as_deref() {
            // Every shipped Git action is read-only today; classify by action
            // anyway so adding a mutating one cannot silently inherit `safe`.
            Some("status" | "diff" | "log" | "show" | "blame" | "commit_plan") => "safe",
            _ => "other",
        },
        // `Run` executes test/verifier commands — closer to shell than safe.
        "Run" | "run" => "shell",
        _ => "other",
    }
}

const HOOK_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const HOOK_PIPE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
/// How long the timeout path waits for the killed child to be reaped.
///
/// The wait after a kill is *bounded* rather than unbounded: `child.wait()`
/// blocks forever if the kill did not take (a `SIGKILL`-immune uninterruptible
/// state on Unix, a `TerminateJobObject` that a protected process survived on
/// Windows), and that turned "this hook has a 30s budget" into a hung turn.
const HOOK_REAP_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const WINDOWS_TASKKILL_TIMEOUT: Duration = Duration::from_secs(2);

/// Kill the hook's process tree and wait, briefly, for the corpse.
///
/// Termination is best-effort by nature — the OS owns whether a kill lands.
/// What is guaranteed here is that *this* thread stops waiting: the
/// containment guard is dropped first (which re-signals the Unix process group
/// and closes the kill-on-close Windows Job Object), then the reap gets one
/// bounded window. Returns `false` when the child could not be confirmed dead,
/// so the caller can report the weaker claim instead of asserting cleanup.
fn terminate_and_reap(
    hook_name: Option<&str>,
    child: &mut Child,
    process_tree: ProcessTree,
) -> bool {
    terminate_tree(&process_tree, child);
    // Drop before the wait, not after: on Windows this closes the Job Object
    // and is itself a kill, and on Unix it re-signals the group. Waiting first
    // would delay the very thing meant to make the wait short.
    drop(process_tree);
    match child.wait_timeout(HOOK_REAP_TIMEOUT) {
        Ok(Some(_)) => true,
        Ok(None) => {
            tracing::warn!(
                target: "hooks",
                hook = %sanitize_hook_label(hook_name),
                reap_timeout_secs = HOOK_REAP_TIMEOUT.as_secs(),
                "hook process did not exit after its tree was killed; abandoning the reap"
            );
            false
        }
        Err(error) => {
            tracing::warn!(
                target: "hooks",
                hook = %sanitize_hook_label(hook_name),
                %error,
                "failed to reap killed hook process"
            );
            false
        }
    }
}

fn spawn_pipe_reader(
    mut pipe: impl Read + Send + 'static,
    worker_name: &str,
) -> std::io::Result<Receiver<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(worker_name.to_string())
        .spawn(move || {
            let mut retained = Vec::with_capacity(HOOK_PIPE_CAPTURE_MAX_BYTES.min(8 * 1024));
            let mut chunk = [0_u8; 8 * 1024];
            let mut truncated = false;
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let remaining = HOOK_PIPE_CAPTURE_MAX_BYTES.saturating_sub(retained.len());
                        let keep = remaining.min(read);
                        retained.extend_from_slice(&chunk[..keep]);
                        truncated |= keep < read;
                    }
                    Err(error) => {
                        tracing::warn!(target: "hooks", %error, "failed while draining hook pipe");
                        break;
                    }
                }
            }
            let mut output = String::from_utf8_lossy(&retained).into_owned();
            if truncated {
                output.push_str("…[truncated]");
            }
            let _ = tx.send(output);
        })
        .map(|_| rx)
}

fn collect_reader(reader: Option<Receiver<String>>, timeout: Duration) -> String {
    let Some(reader) = reader else {
        return String::new();
    };
    match reader.recv_timeout(timeout) {
        Ok(output) => output,
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!(
                ?timeout,
                "hook pipe reader did not finish after process cleanup"
            );
            String::new()
        }
        Err(RecvTimeoutError::Disconnected) => String::new(),
    }
}

fn spawn_stdin_writer(
    mut stdin: std::process::ChildStdin,
    mut bytes: Vec<u8>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("hook-stdin-writer".to_string())
        .spawn(move || {
            bytes.push(b'\n');
            let _ = stdin.write_all(&bytes);
            let _ = stdin.flush();
        })
}

fn bounded_message_submit_metadata(value: Option<&str>, max_bytes: usize) -> Option<String> {
    value.map(|value| truncate_env_value(value, max_bytes))
}

fn build_message_submit_payload(
    context: &HookContext,
    text: &str,
    original_bytes: usize,
    truncated: bool,
    metadata_max_bytes: Option<usize>,
) -> serde_json::Value {
    let mut payload = json!({
        "event": HookEvent::MessageSubmit.as_str(),
        "text": text,
        "text_bytes": text.len(),
        "text_original_bytes": original_bytes,
        "text_truncated": truncated,
    });
    if let Some(max_bytes) = metadata_max_bytes {
        let object = payload
            .as_object_mut()
            .expect("message_submit payload is an object");
        object.insert(
            "session_id".to_string(),
            json!(bounded_message_submit_metadata(
                context.session_id.as_deref(),
                max_bytes
            )),
        );
        object.insert(
            "workspace".to_string(),
            json!(bounded_message_submit_metadata(
                context.workspace.as_ref().and_then(|path| path.to_str()),
                max_bytes
            )),
        );
        object.insert(
            "mode".to_string(),
            json!(bounded_message_submit_metadata(
                context.mode.as_deref(),
                max_bytes
            )),
        );
        object.insert(
            "model".to_string(),
            json!(bounded_message_submit_metadata(
                context.model.as_deref(),
                max_bytes
            )),
        );
        object.insert("total_tokens".to_string(), json!(context.total_tokens));
    }
    payload
}

fn encoded_message_submit_payload_fits(payload: &serde_json::Value) -> bool {
    serde_json::to_vec(payload)
        .is_ok_and(|bytes| bytes.len() <= HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES)
}

fn finalize_message_submit_payload(
    payload: serde_json::Value,
    original_bytes: usize,
) -> serde_json::Value {
    if encoded_message_submit_payload_fits(&payload) {
        return payload;
    }

    // Serialization of a `Value` is infallible in practice, but the size
    // boundary is security-sensitive. If an invariant above ever regresses,
    // discard all user text and diagnostics rather than handing an oversized
    // document to a hook process.
    tracing::error!(target: "hooks", "message_submit payload fitter exceeded its hard byte cap");
    let fail_closed = build_message_submit_payload(
        &HookContext::new(),
        "",
        original_bytes,
        original_bytes != 0,
        None,
    );
    assert!(
        encoded_message_submit_payload_fits(&fail_closed),
        "minimal message_submit payload must fit the hard byte cap"
    );
    fail_closed
}

/// Build the one canonical `message_submit` stdin document.
///
/// Every producer — immediate input, restored queue entries, merged steers,
/// and hook-to-hook replacements — crosses this serialization boundary. The
/// largest UTF-8-safe text prefix that keeps the *serialized JSON* within the
/// byte ceiling is retained, and explicit metadata tells the hook exactly
/// what was clipped.
pub(crate) fn message_submit_payload(context: &HookContext, text: &str) -> serde_json::Value {
    // Diagnostic metadata is useful but never allowed to crowd the actual
    // gate input out of the hard byte budget. Control-heavy strings can grow
    // sixfold when JSON-escaped, so try progressively smaller snapshots and
    // finally omit diagnostics altogether.
    let metadata_max_bytes = [
        Some(HOOK_MESSAGE_SUBMIT_METADATA_MAX_BYTES),
        Some(1_024),
        Some(256),
        None,
    ]
    .into_iter()
    .find(|metadata_max_bytes| {
        encoded_message_submit_payload_fits(&build_message_submit_payload(
            context,
            "",
            text.len(),
            !text.is_empty(),
            *metadata_max_bytes,
        ))
    })
    .unwrap_or(None);

    if text.len() <= HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES {
        let complete =
            build_message_submit_payload(context, text, text.len(), false, metadata_max_bytes);
        if encoded_message_submit_payload_fits(&complete) {
            return finalize_message_submit_payload(complete, text.len());
        }
    }

    // No candidate can retain more raw bytes than the full JSON budget. Build
    // at most that many UTF-8 boundaries even if a restored queue entry is
    // unexpectedly enormous.
    let raw_prefix_cap = text.len().min(HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES);
    let mut utf8_ends = Vec::with_capacity(raw_prefix_cap.saturating_add(1));
    utf8_ends.push(0);
    utf8_ends.extend(
        text.char_indices()
            .map(|(index, ch)| index + ch.len_utf8())
            .take_while(|end| *end <= raw_prefix_cap),
    );

    let mut lower = 0usize;
    let mut upper = utf8_ends.len();
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let end = utf8_ends[middle];
        let candidate = build_message_submit_payload(
            context,
            &text[..end],
            text.len(),
            true,
            metadata_max_bytes,
        );
        let fits = encoded_message_submit_payload_fits(&candidate);
        if fits {
            lower = middle + 1;
        } else {
            upper = middle;
        }
    }

    let retained_end = utf8_ends[lower.saturating_sub(1)];
    finalize_message_submit_payload(
        build_message_submit_payload(
            context,
            &text[..retained_end],
            text.len(),
            true,
            metadata_max_bytes,
        ),
        text.len(),
    )
}

pub fn turn_end_payload(input: TurnEndPayloadInput<'_>) -> serde_json::Value {
    let bounded_error = input
        .error
        .map(|error| sanitize_hook_text(error, HOOK_TURN_ERROR_MAX_CHARS));
    json!({
        "event": HookEvent::TurnEnd.as_str(),
        "session_id": input.context.session_id.as_deref(),
        "workspace": input.context.workspace.as_ref().map(|path| path.display().to_string()),
        "mode": input.context.mode.as_deref(),
        "created_at": input.created_at.to_rfc3339(),
        "model_backed": input.model_backed,
        "provider": input.provider,
        "billing_surface": input.billing_surface,
        "model": input.model.or(input.context.model.as_deref()),
        "turn_id": input.turn_id,
        "status": input.status,
        "error": bounded_error,
        "duration_ms": duration_ms_saturating(input.duration),
        "usage": {
            "input_tokens": input.usage.input_tokens,
            "output_tokens": input.usage.output_tokens,
            "prompt_cache_hit_tokens": input.usage.prompt_cache_hit_tokens,
            "prompt_cache_miss_tokens": input.usage.prompt_cache_miss_tokens,
            "prompt_cache_write_tokens": input.usage.prompt_cache_write_tokens,
            "reasoning_tokens": input.usage.reasoning_tokens,
            "reasoning_replay_tokens": input.usage.reasoning_replay_tokens,
        },
        "totals": {
            "session_tokens": input.totals.session_tokens,
            "conversation_tokens": input.totals.conversation_tokens,
            "input_tokens": input.totals.input_tokens,
            "output_tokens": input.totals.output_tokens,
        },
        "tool_count": input.tool_count,
        "queued_message_count": input.queued_message_count,
        "stop_hook_active": false,
    })
}

fn duration_ms_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn parse_message_submit_stdout(stdout: &str) -> MessageSubmitStdout {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return MessageSubmitStdout::Unchanged;
    }

    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(e) => return MessageSubmitStdout::Invalid(format!("invalid JSON: {e}")),
    };

    let Some(object) = value.as_object() else {
        return MessageSubmitStdout::Invalid("stdout JSON must be an object".to_string());
    };

    if object.get("block").and_then(serde_json::Value::as_bool) == Some(true) {
        let reason = object
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("message_submit hook blocked submission");
        return MessageSubmitStdout::Blocked(sanitize_hook_denial_reason(reason));
    }
    match object.get("text") {
        Some(serde_json::Value::String(text)) if !text.is_empty() => {
            if text.chars().count() > HOOK_MESSAGE_REPLACEMENT_MAX_CHARS {
                MessageSubmitStdout::Invalid(format!(
                    "stdout `text` field exceeds {HOOK_MESSAGE_REPLACEMENT_MAX_CHARS} characters"
                ))
            } else {
                MessageSubmitStdout::Replaced(text.clone())
            }
        }
        Some(serde_json::Value::String(_)) => {
            MessageSubmitStdout::Invalid("stdout `text` field must not be empty".to_string())
        }
        Some(_) => MessageSubmitStdout::Invalid("stdout `text` field must be a string".to_string()),
        None => MessageSubmitStdout::Unchanged,
    }
}

fn message_submit_continue_warning(result: &HookResult) -> Option<String> {
    message_submit_stdout_reason(&result.stdout)
        .or_else(|| {
            Some(generic_unavailable_detail(result.error.as_deref()))
                .filter(|detail| detail != "hook returned no verdict")
        })
        .or_else(|| {
            result
                .observed_exit_code()
                .map(|code| format!("message_submit hook exited with code {code}"))
        })
}

fn message_submit_block_reason(result: &HookResult, fallback: &str) -> String {
    if let Some(reason) = message_submit_stdout_reason(&result.stdout) {
        return reason;
    }
    let detail = generic_unavailable_detail(result.error.as_deref());
    if detail != "hook returned no verdict" {
        return detail;
    }
    fallback.to_string()
}

fn message_submit_stdout_reason(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(sanitize_hook_denial_reason)
}

/// Largest single `shell_env` value that is accepted, in bytes.
const SHELL_ENV_VALUE_MAX_BYTES: usize = 32 * 1024;
/// Largest total `shell_env` contribution from one hook, in bytes of
/// `KEY` + `VALUE`. Past this, later entries from that hook are dropped.
const SHELL_ENV_TOTAL_MAX_BYTES: usize = 256 * 1024;

/// Whether a parsed name is usable as an environment variable name.
///
/// `Command::env` **panics** on a key containing a NUL byte or `=`, and an
/// empty key is meaningless, so an entry that fails this check is dropped
/// rather than carried into `exec_shell`'s environment. A `shell_env` hook is
/// a normal process whose stdout can contain anything — including a NUL
/// straight out of a binary — and "the hook printed something odd" must never
/// become "Codewhale aborted the tool call".
fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains('=')
        && !key.chars().any(|c| c == '\0' || c.is_control() || c == ' ')
}

/// Parse `KEY=VALUE\n` lines from a `shell_env` hook's stdout into a map.
///
/// Tolerated: blank lines, leading whitespace, `#` comment lines (ignored),
/// `export KEY=VALUE` (the `export ` prefix is dropped), surrounding quotes
/// on the value. Lines without `=` are silently dropped — easier than
/// failing the whole hook for one stray line of human-friendly output.
/// Values are otherwise taken verbatim; we don't run them through a shell
/// for variable expansion to avoid surprises.
///
/// Rejected: entries whose key is unusable ([`is_valid_env_key`]), values
/// containing a NUL byte, values over [`SHELL_ENV_VALUE_MAX_BYTES`], and
/// anything past [`SHELL_ENV_TOTAL_MAX_BYTES`] of accumulated output. Each
/// drop is logged by key name only — never by value.
fn parse_env_lines(stdout: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut total_bytes = 0usize;
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_valid_env_key(key) {
            tracing::warn!(
                target: "hooks",
                "shell_env hook produced an unusable variable name; dropping the entry"
            );
            continue;
        }
        let value = value.trim();
        let stripped = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if stripped.contains('\0') {
            tracing::warn!(
                target: "hooks",
                key,
                "shell_env value contains a NUL byte; dropping the entry"
            );
            continue;
        }
        if stripped.len() > SHELL_ENV_VALUE_MAX_BYTES {
            tracing::warn!(
                target: "hooks",
                key,
                limit = SHELL_ENV_VALUE_MAX_BYTES,
                "shell_env value exceeds the per-value limit; dropping the entry"
            );
            continue;
        }
        let entry_bytes = key.len() + stripped.len();
        if total_bytes.saturating_add(entry_bytes) > SHELL_ENV_TOTAL_MAX_BYTES {
            tracing::warn!(
                target: "hooks",
                key,
                limit = SHELL_ENV_TOTAL_MAX_BYTES,
                "shell_env output exceeds the total limit; dropping the remaining entries"
            );
            break;
        }
        total_bytes += entry_bytes;
        out.insert(key.to_string(), stripped.to_string());
    }
    out
}

/// Metadata a settled tool call reported, whether it succeeded or failed.
///
/// `bash` reports a nonzero exit, timeout, or kill as an error, so the error
/// carries the metadata then; reading only `Ok` results lost the exit code of
/// every failing command.
fn reported_tool_metadata(
    result: &Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
) -> Option<&serde_json::Value> {
    match result {
        Ok(output) => output.metadata.as_ref(),
        Err(error) => error.metadata(),
    }
}

/// Read how a process-backed tool ended, for `DEEPSEEK_TOOL_STATUS`.
///
/// Only the shell statuses the tools record count; anything else stays `None`
/// rather than passing an arbitrary metadata string into a hook's environment.
fn reported_tool_status(
    result: &Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
) -> Option<&'static str> {
    match reported_tool_metadata(result)?.get("status")?.as_str()? {
        "Completed" => Some("completed"),
        "Failed" => Some("failed"),
        "TimedOut" => Some("timed_out"),
        "Killed" => Some("killed"),
        "Running" => Some("running"),
        _ => None,
    }
}

/// Read the post-admission execution receipt a shell tool recorded (#6689),
/// serialized for `DEEPSEEK_TOOL_EXECUTION_RECEIPT`.
///
/// Only a schema-1 object within the size bound counts. The receipt is built
/// by the shell tool from what its process manager recorded at spawn; it is
/// never reconstructed here from the before-hook input, which can differ from
/// what actually ran.
fn reported_tool_execution_receipt(
    result: &Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
) -> Option<String> {
    let receipt = reported_tool_metadata(result)?.get("execution_receipt")?;
    if receipt.get("schema_version")?.as_u64()? != 1 {
        return None;
    }
    let encoded = serde_json::to_string(receipt).ok()?;
    (encoded.len() <= HOOK_EXECUTION_RECEIPT_MAX_BYTES).then_some(encoded)
}

/// Read the process exit code a tool reported, when it reported one.
///
/// The one source for `DEEPSEEK_TOOL_EXIT_CODE`: the TUI and the Runtime API
/// thread path both reach it through [`HookContext::with_tool_outcome`].
///
/// Only process-backed tools (`exec_shell`, `bash`, task runners) carry one,
/// on a successful result or a failed one, and only a real, integer-valued
/// `exit_code` counts. Everything else stays `None` so
/// an `exit_code` condition never matches on a fabricated value.
/// Reported as `i64`, not `i32`: a Windows crash code such as `3221225477`
/// (`0xC0000005`) is a real value the shell tool records in its metadata, and
/// narrowing it dropped exactly those codes — the hook saw no exit code at all
/// for the crashes it most wanted to catch.
fn reported_tool_exit_code(
    result: &Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
) -> Option<i64> {
    let code = reported_tool_metadata(result)?.get("exit_code")?;
    if code.is_null() {
        return None;
    }
    code.as_i64()
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn trust_workspace_for_project_hooks(workspace: &Path, config_path: &Path) -> EnvVarGuard {
        let guard = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path);
        crate::config::save_workspace_trust(workspace).expect("save workspace trust");
        guard
    }

    /// #455 — `exit_code` conditions must only ever see a real, reported exit
    /// code. `tool_call_after` used to hard-code `None`, which made every
    /// `{ type = "exit_code" }` condition permanently unmatchable.
    #[test]
    fn reported_tool_exit_code_reads_only_real_metadata_codes() {
        use crate::tools::spec::{ToolError, ToolResult};

        let with_code = Ok(ToolResult {
            content: "boom".to_string(),
            success: false,
            metadata: Some(serde_json::json!({ "exit_code": 127 })),
        });
        assert_eq!(reported_tool_exit_code(&with_code), Some(127));

        // Zero is a real code, not a missing one.
        let zero = Ok(ToolResult {
            content: "ok".to_string(),
            success: true,
            metadata: Some(serde_json::json!({ "exit_code": 0 })),
        });
        assert_eq!(reported_tool_exit_code(&zero), Some(0));

        // Tools that report no exit code stay `None` — never synthesized from
        // the success flag.
        let no_metadata = Ok(ToolResult::error("failed"));
        assert_eq!(reported_tool_exit_code(&no_metadata), None);

        let null_code = Ok(ToolResult {
            content: String::new(),
            success: true,
            metadata: Some(serde_json::json!({ "exit_code": serde_json::Value::Null })),
        });
        assert_eq!(reported_tool_exit_code(&null_code), None);

        let wrong_type = Ok(ToolResult {
            content: String::new(),
            success: false,
            metadata: Some(serde_json::json!({ "exit_code": "127" })),
        });
        assert_eq!(reported_tool_exit_code(&wrong_type), None);

        // A Windows crash code does not fit in an `i32`, but it is a real code
        // and a hook scoped to it must be able to see it.
        let windows_crash = Ok(ToolResult {
            content: String::new(),
            success: false,
            metadata: Some(serde_json::json!({ "exit_code": 3_221_225_477_i64 })),
        });
        assert_eq!(reported_tool_exit_code(&windows_crash), Some(3_221_225_477));

        // A transport-level tool error has no metadata at all.
        let errored: Result<ToolResult, ToolError> =
            Err(ToolError::execution_failed("no such tool"));
        assert_eq!(reported_tool_exit_code(&errored), None);
        assert_eq!(reported_tool_status(&errored), None);

        // A failed command reported as an error still carries its code and
        // status.
        let failed_command: Result<ToolResult, ToolError> =
            Err(ToolError::execution_failed_with_metadata(
                "Command exited with code 127",
                serde_json::json!({ "exit_code": 127, "status": "Failed" }),
            ));
        assert_eq!(reported_tool_exit_code(&failed_command), Some(127));
        assert_eq!(reported_tool_status(&failed_command), Some("failed"));

        // A timeout has a status but no exit code; the code is not invented.
        let timed_out: Result<ToolResult, ToolError> =
            Err(ToolError::execution_failed_with_metadata(
                "Command timed out after 1 seconds",
                serde_json::json!({ "exit_code": null, "status": "TimedOut" }),
            ));
        assert_eq!(reported_tool_exit_code(&timed_out), None);
        assert_eq!(reported_tool_status(&timed_out), Some("timed_out"));

        // An unknown status string is not passed through.
        let odd_status = Ok(ToolResult {
            content: String::new(),
            success: true,
            metadata: Some(serde_json::json!({ "status": "$(boom)" })),
        });
        assert_eq!(reported_tool_status(&odd_status), None);
    }

    #[test]
    fn config_types_are_available_from_config_module() {
        let hook = crate::hooks::config::Hook::new(
            crate::hooks::config::HookEvent::SessionStart,
            "echo ready",
        );
        let config = crate::hooks::config::HooksConfig {
            enabled: true,
            hooks: vec![hook],
            ..Default::default()
        };

        let hooks = config.hooks_for_event(crate::hooks::config::HookEvent::SessionStart);

        assert_eq!(hooks.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn plugin_hook_runs_after_restart_and_process_spawn_rechecks_revocation() {
        let _lock = lock_test_env();
        let fixture = crate::plugins::test_fixture::DeclarativePluginFixture::new();
        let config = HooksConfig::load_with_project_and_plugins(
            HooksConfig {
                enabled: true,
                ..HooksConfig::default()
            },
            &fixture.workspace,
            Some(&fixture.registry),
        );
        assert!(config.problems.is_empty(), "{:?}", config.problems);
        assert_eq!(config.hooks.len(), 1);
        assert!(config.hooks[0].plugin_authority.is_some());
        let executor = HookExecutor::new(config, fixture.workspace.clone());
        let context = HookContext::new().with_workspace(fixture.workspace.clone());

        let ran = executor.execute(HookEvent::SessionStart, &context);
        assert_eq!(ran.len(), 1);
        assert!(ran[0].success, "{:?}", ran[0].error);
        assert_eq!(
            std::fs::read_to_string(&fixture.marker).expect("plugin hook marker"),
            "plugin-hook-ran"
        );
        std::fs::remove_file(&fixture.marker).expect("clear marker");

        let inactive = fixture.revoke_from_fresh_registry();
        let denied = executor.execute(HookEvent::SessionStart, &context);
        assert_eq!(denied.len(), 1);
        assert!(!denied[0].success);
        assert!(
            denied[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("authority was denied")),
            "{:?}",
            denied[0].error
        );
        assert!(
            !fixture.marker.exists(),
            "revoked hook must be denied before process spawn"
        );

        let reloaded = HooksConfig::load_with_project_and_plugins(
            HooksConfig {
                enabled: true,
                ..HooksConfig::default()
            },
            &fixture.workspace,
            Some(&inactive),
        );
        assert!(
            reloaded.hooks.is_empty(),
            "reload removes the revoked plugin Hook"
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_plugin_hook_rechecks_revocation_at_dequeue() {
        let _lock = lock_test_env();
        let fixture = crate::plugins::test_fixture::DeclarativePluginFixture::new();
        let blocker_one = Hook::new(HookEvent::SessionStart, "sleep 1").background();
        let blocker_two = Hook::new(HookEvent::SessionStart, "sleep 1").background();
        let mut config = HooksConfig::load_with_project_and_plugins(
            HooksConfig {
                enabled: true,
                hooks: vec![blocker_one, blocker_two],
                ..HooksConfig::default()
            },
            &fixture.workspace,
            Some(&fixture.registry),
        );
        assert_eq!(config.hooks.len(), 3);
        config.hooks[2].background = true;
        let executor = HookExecutor::new(config, fixture.workspace.clone());

        let submitted = executor.execute(
            HookEvent::SessionStart,
            &HookContext::new().with_workspace(fixture.workspace.clone()),
        );
        assert_eq!(submitted.len(), 3);
        assert!(submitted.iter().all(|result| result.background));
        fixture.revoke_from_fresh_registry();

        std::thread::sleep(Duration::from_millis(1_500));
        assert!(
            !fixture.marker.exists(),
            "queued hook must recheck authority after the preceding job finishes"
        );
    }

    #[test]
    fn executor_type_is_available_from_executor_module() {
        let executor = crate::hooks::executor::HookExecutor::disabled();

        assert!(!executor.is_enabled());
    }

    /// #456 — `parse_env_lines` covers the formats users actually emit from
    /// shell hooks: bare `KEY=VAL`, `export KEY=VAL`, quoted values, comments,
    /// blank lines. Lines without `=` are dropped; values are taken verbatim
    /// (no shell expansion).
    #[test]
    fn parse_env_lines_handles_realistic_hook_output() {
        let stdout = r#"
# Aux comment line, ignored
AWS_ACCESS_KEY_ID=AKIAEXAMPLE
export GITHUB_TOKEN=ghp_examplevalue
QUOTED="value with spaces"
SINGLE='also valid'

= empty key dropped
NOEQUAL line dropped
"#;
        let parsed = super::parse_env_lines(stdout);
        assert_eq!(
            parsed.get("AWS_ACCESS_KEY_ID"),
            Some(&"AKIAEXAMPLE".to_string())
        );
        assert_eq!(
            parsed.get("GITHUB_TOKEN"),
            Some(&"ghp_examplevalue".to_string())
        );
        assert_eq!(parsed.get("QUOTED"), Some(&"value with spaces".to_string()));
        assert_eq!(parsed.get("SINGLE"), Some(&"also valid".to_string()));
        assert!(!parsed.contains_key(""));
        assert!(!parsed.contains_key("NOEQUAL line dropped"));
        // 4 valid entries above; nothing else.
        assert_eq!(parsed.len(), 4);
    }

    /// #456 — empty stdout (or only blank/comments) yields an empty map.
    #[test]
    fn parse_env_lines_empty_when_no_assignments() {
        let parsed = super::parse_env_lines("# nothing\n\n  \n");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_message_submit_stdout_replaces_text() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"text":"changed"}"#),
            MessageSubmitStdout::Replaced("changed".to_string())
        );
    }

    #[test]
    fn parse_message_submit_stdout_empty_is_unchanged() {
        assert_eq!(
            super::parse_message_submit_stdout(" \n\t "),
            MessageSubmitStdout::Unchanged
        );
    }

    #[test]
    fn parse_message_submit_stdout_without_text_is_unchanged() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"reason":"only used for blocks"}"#),
            MessageSubmitStdout::Unchanged
        );
    }

    #[test]
    fn message_submit_payload_is_byte_bounded_after_json_escaping() {
        let original = "用户\"\\\n".repeat(20_000);
        let payload = super::message_submit_payload(
            &HookContext::new()
                .with_session_id("sess_test")
                .with_model("model"),
            &original,
        );
        let encoded = serde_json::to_vec(&payload).expect("serialize bounded payload");

        assert!(
            encoded.len() <= super::HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES,
            "serialized payload was {} bytes",
            encoded.len()
        );
        assert_eq!(payload["text_truncated"], true);
        assert_eq!(payload["text_original_bytes"], original.len());
        let retained = payload["text"].as_str().expect("text string");
        assert_eq!(payload["text_bytes"], retained.len());
        assert!(original.starts_with(retained));
        assert!(std::str::from_utf8(retained.as_bytes()).is_ok());
    }

    #[test]
    fn message_submit_payload_omits_hostile_diagnostics_before_exceeding_cap() {
        let hostile = "\u{0}\u{1}\u{1f}\"\\".repeat(8_000);
        let original = "\u{0}\"\\用户".repeat(20_000);
        let context = HookContext::new()
            .with_session_id(&hostile)
            .with_workspace(PathBuf::from(&hostile))
            .with_mode(&hostile)
            .with_model(&hostile);
        let payload = super::message_submit_payload(&context, &original);
        let encoded = serde_json::to_vec(&payload).expect("serialize hostile payload");

        assert!(
            encoded.len() <= super::HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES,
            "serialized payload was {} bytes",
            encoded.len()
        );
        assert_eq!(payload["text_truncated"], true);
        assert_eq!(payload["text_original_bytes"], original.len());
        assert!(original.starts_with(payload["text"].as_str().expect("text")));

        for key in ["session_id", "workspace", "mode", "model"] {
            if let Some(value) = payload.get(key).and_then(serde_json::Value::as_str) {
                assert!(
                    value.len() <= super::HOOK_MESSAGE_SUBMIT_METADATA_MAX_BYTES + 16,
                    "{key} was not bounded before serialization"
                );
            }
        }
    }

    #[test]
    fn short_message_submit_payload_carries_explicit_untruncated_metadata() {
        let payload = super::message_submit_payload(&HookContext::new(), "hello 用户");
        assert_eq!(payload["text"], "hello 用户");
        assert_eq!(payload["text_bytes"], "hello 用户".len());
        assert_eq!(payload["text_original_bytes"], "hello 用户".len());
        assert_eq!(payload["text_truncated"], false);
    }

    #[test]
    fn parse_message_submit_stdout_rejects_malformed_json() {
        assert!(matches!(
            super::parse_message_submit_stdout("not json"),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn parse_message_submit_stdout_rejects_non_string_text() {
        assert!(matches!(
            super::parse_message_submit_stdout(r#"{"text":123}"#),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn parse_message_submit_stdout_rejects_empty_text() {
        assert_eq!(
            super::parse_message_submit_stdout(r#"{"text":""}"#),
            MessageSubmitStdout::Invalid("stdout `text` field must not be empty".to_string())
        );
    }

    #[test]
    fn parse_message_submit_stdout_rejects_non_object_json() {
        assert!(matches!(
            super::parse_message_submit_stdout(r#"["not", "an", "object"]"#),
            MessageSubmitStdout::Invalid(_)
        ));
        assert!(matches!(
            super::parse_message_submit_stdout(r#""not an object""#),
            MessageSubmitStdout::Invalid(_)
        ));
    }

    #[test]
    fn test_hook_event_as_str() {
        assert_eq!(HookEvent::SessionStart.as_str(), "session_start");
        assert_eq!(HookEvent::ToolCallAfter.as_str(), "tool_call_after");
        assert_eq!(HookEvent::ModeChange.as_str(), "mode_change");
        assert_eq!(HookEvent::TurnEnd.as_str(), "turn_end");
        assert_eq!(HookEvent::SubagentSpawn.as_str(), "subagent_spawn");
        assert_eq!(HookEvent::SubagentComplete.as_str(), "subagent_complete");
    }

    #[test]
    fn turn_end_payload_contains_post_turn_observer_fields() {
        let context = HookContext::new()
            .with_session_id("sess_test")
            .with_workspace(PathBuf::from("/tmp/codewhale"))
            .with_mode("agent")
            .with_model("deepseek-v4")
            .with_tokens(125);
        let usage = codewhale_models::Usage {
            input_tokens: 40,
            output_tokens: 9,
            prompt_cache_hit_tokens: Some(10),
            prompt_cache_miss_tokens: Some(30),
            prompt_cache_write_tokens: None,
            reasoning_tokens: Some(4),
            reasoning_replay_tokens: Some(2),
            server_tool_use: None,

            input_audio_tokens: None,
            output_audio_tokens: None,

            request_usages: Vec::new(),
        };

        let payload = super::turn_end_payload(TurnEndPayloadInput {
            context: &context,
            created_at: "2026-07-12T10:30:00Z".parse().expect("timestamp"),
            model_backed: true,
            provider: Some("deepseek"),
            billing_surface: Some("test-payg"),
            model: Some("deepseek-v4-pro"),
            turn_id: "turn_123",
            status: "completed",
            error: None,
            duration: Duration::from_millis(321),
            usage: &usage,
            totals: TurnEndTotals {
                session_tokens: 125,
                conversation_tokens: 100,
                input_tokens: 100,
                output_tokens: 25,
            },
            tool_count: 2,
            queued_message_count: 1,
        });

        assert_eq!(payload["event"], "turn_end");
        assert_eq!(payload["session_id"], "sess_test");
        assert_eq!(payload["workspace"], "/tmp/codewhale");
        assert_eq!(payload["mode"], "agent");
        assert_eq!(payload["created_at"], "2026-07-12T10:30:00+00:00");
        assert_eq!(payload["model_backed"], true);
        assert_eq!(payload["provider"], "deepseek");
        assert_eq!(payload["billing_surface"], "test-payg");
        assert!(payload.get("base_url").is_none());
        assert_eq!(payload["model"], "deepseek-v4-pro");
        assert_eq!(payload["turn_id"], "turn_123");
        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["error"], serde_json::Value::Null);
        assert_eq!(payload["duration_ms"], 321);
        assert_eq!(payload["usage"]["input_tokens"], 40);
        assert_eq!(payload["usage"]["output_tokens"], 9);
        assert_eq!(payload["usage"]["prompt_cache_hit_tokens"], 10);
        assert_eq!(payload["usage"]["prompt_cache_miss_tokens"], 30);
        assert_eq!(payload["usage"]["reasoning_tokens"], 4);
        assert_eq!(payload["usage"]["reasoning_replay_tokens"], 2);
        assert_eq!(payload["totals"]["session_tokens"], 125);
        assert_eq!(payload["totals"]["conversation_tokens"], 100);
        assert_eq!(payload["totals"]["input_tokens"], 100);
        assert_eq!(payload["totals"]["output_tokens"], 25);
        assert_eq!(payload["tool_count"], 2);
        assert_eq!(payload["queued_message_count"], 1);
        assert_eq!(payload["stop_hook_active"], false);
    }

    #[test]
    fn test_hook_context_to_env_vars() {
        let ctx = HookContext::new()
            .with_tool_name("exec_shell")
            .with_mode("agent")
            .with_workspace(PathBuf::from("/tmp"));

        let env = ctx.to_env_vars();

        assert_eq!(
            env.get("DEEPSEEK_TOOL_NAME"),
            Some(&"exec_shell".to_string())
        );
        assert_eq!(env.get("DEEPSEEK_MODE"), Some(&"agent".to_string()));
        assert_eq!(env.get("DEEPSEEK_WORKSPACE"), Some(&"/tmp".to_string()));
    }

    #[test]
    fn test_hook_condition_always() {
        let hook = Hook::new(HookEvent::SessionStart, "echo test");
        let executor = HookExecutor::disabled();
        let context = HookContext::new();

        assert!(executor.matches_condition(&hook, &context));
    }

    #[test]
    fn test_hook_condition_tool_name() {
        let hook = Hook::new(HookEvent::ToolCallBefore, "echo test").with_condition(
            HookCondition::ToolName {
                name: "exec_shell".to_string(),
            },
        );

        let executor = HookExecutor::disabled();

        let context_match = HookContext::new().with_tool_name("exec_shell");
        let context_no_match = HookContext::new().with_tool_name("write_file");

        assert!(executor.matches_condition(&hook, &context_match));
        assert!(!executor.matches_condition(&hook, &context_no_match));
    }

    #[test]
    fn test_hook_condition_mode() {
        let hook =
            Hook::new(HookEvent::ModeChange, "echo test").with_condition(HookCondition::Mode {
                mode: "agent".to_string(),
            });

        let executor = HookExecutor::disabled();

        let context_match = HookContext::new().with_mode("AGENT"); // Case insensitive
        let context_no_match = HookContext::new().with_mode("normal");

        assert!(executor.matches_condition(&hook, &context_match));
        assert!(!executor.matches_condition(&hook, &context_no_match));
    }

    #[test]
    fn test_hooks_config_for_event() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo start"),
                Hook::new(HookEvent::SessionEnd, "echo end"),
                Hook::new(HookEvent::SessionStart, "echo start2"),
            ],
            ..Default::default()
        };

        let start_hooks = config.hooks_for_event(HookEvent::SessionStart);
        assert_eq!(start_hooks.len(), 2);

        let end_hooks = config.hooks_for_event(HookEvent::SessionEnd);
        assert_eq!(end_hooks.len(), 1);
    }

    #[test]
    fn test_hooks_config_disabled() {
        let config = HooksConfig {
            enabled: false,
            hooks: vec![Hook::new(HookEvent::SessionStart, "echo start")],
            ..Default::default()
        };

        let hooks = config.hooks_for_event(HookEvent::SessionStart);
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_hook_builder() {
        let hook = Hook::new(HookEvent::ToolCallAfter, "notify.sh")
            .with_name("notify_tool")
            .with_timeout(60)
            .background()
            .with_condition(HookCondition::ToolCategory {
                category: "shell".to_string(),
            });

        assert_eq!(hook.name, Some("notify_tool".to_string()));
        assert_eq!(hook.timeout_secs, 60);
        assert!(hook.background);
        assert!(matches!(
            hook.condition,
            Some(HookCondition::ToolCategory { .. })
        ));
    }

    #[test]
    fn test_hook_timeout_enforced() {
        let command = if cfg!(windows) {
            "ping -n 3 127.0.0.1 > nul"
        } else {
            "sleep 2"
        };
        let hook = Hook::new(HookEvent::SessionStart, command).with_timeout(1);
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let env_vars = HashMap::new();

        let result = executor.execute_sync(&hook, &env_vars);
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|e| e.contains("timed out"))
        );
    }

    #[test]
    fn observer_hook_receives_eof_instead_of_inheriting_terminal_stdin() {
        const INNER_ENV: &str = "CODEWHALE_TEST_HOOK_EOF_INNER";
        const TEST_NAME: &str =
            "hooks::tests::observer_hook_receives_eof_instead_of_inheriting_terminal_stdin";

        if std::env::var_os(INNER_ENV).is_some() {
            let dir = tempfile::tempdir().expect("tempdir");
            #[cfg(not(windows))]
            let command = write_hook_script(
                &dir,
                "read_to_eof.sh",
                r#"#!/bin/sh
payload=$(cat)
printf 'stdin-bytes=%s\n' "${#payload}"
"#,
            );
            #[cfg(windows)]
            let command = "powershell -NoProfile -Command \"$value = [Console]::In.ReadToEnd(); [Console]::Out.WriteLine(('stdin-bytes=' + $value.Length))\"".to_string();
            // A cold PowerShell process can take several seconds to start on a
            // contended Windows CI runner. Keep the hook timeout finite so the
            // regression still detects an inherited live stdin pipe, while
            // allowing enough startup time for the EOF assertion itself.
            let hook_timeout_secs = if cfg!(windows) { 10 } else { 2 };
            let hook =
                Hook::new(HookEvent::ToolCallBefore, &command).with_timeout(hook_timeout_secs);
            let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());

            let result = executor.execute_sync(&hook, &HashMap::new());
            assert!(result.success, "stdin-less hook should finish: {result:?}");
            assert_eq!(result.stdout.trim(), "stdin-bytes=0");
            return;
        }

        // Keep this subprocess's stdin pipe deliberately open. Before #4489,
        // the hook inherited that live pipe and blocked instead of receiving
        // EOF. The inner test can only finish when HookExecutor closes the
        // child's stdin write end.
        let mut child = Command::new(std::env::current_exe().expect("current test binary"))
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env(INNER_ENV, "1")
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn isolated hook EOF test");
        let held_open_stdin = child.stdin.take().expect("piped child stdin");
        // Leave headroom around the inner hook timeout so a cold Windows test
        // process can start, without weakening the held-open-pipe regression.
        let isolated_timeout_secs = if cfg!(windows) { 25 } else { 10 };
        let status = match child
            .wait_timeout(Duration::from_secs(isolated_timeout_secs))
            .expect("wait for isolated hook EOF test")
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated hook EOF test hung with parent stdin open");
            }
        };
        drop(held_open_stdin);
        assert!(status.success(), "isolated hook EOF test failed: {status}");
    }

    #[cfg(not(windows))]
    #[test]
    fn timed_out_hook_kills_descendant_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("descendant-survived");
        let command = write_hook_script(
            &dir,
            "spawn_descendant.sh",
            &format!(
                "#!/bin/sh\n(sleep 2; printf leaked > '{}') &\nsleep 5\n",
                marker.display()
            ),
        );
        let hook = Hook::new(HookEvent::ToolCallBefore, &command).with_timeout(1);
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());

        let result = executor.execute_sync(&hook, &HashMap::new());
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|error| error.contains("timed out")),
            "hook should time out: {result:?}"
        );
        std::thread::sleep(Duration::from_millis(1_500));
        assert!(
            !marker.exists(),
            "the timed-out hook's descendant escaped its process group"
        );
    }

    #[cfg(windows)]
    #[test]
    fn timed_out_hook_kills_windows_descendant_job() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("descendant-started.txt");
        let survived = dir.path().join("descendant-survived.txt");
        let descendant = dir.path().join("descendant.cmd");
        std::fs::write(
            &descendant,
            "@echo off\r\necho started>descendant-started.txt\r\nping -n 5 127.0.0.1 > nul\r\necho survived>descendant-survived.txt\r\n",
        )
        .expect("write descendant script");
        let parent = dir.path().join("parent.cmd");
        std::fs::write(
            &parent,
            "@echo off\r\nstart \"\" /b cmd.exe /d /c descendant.cmd\r\n:wait_for_child\r\nif exist descendant-started.txt goto child_started\r\nping -n 2 127.0.0.1 > nul\r\ngoto wait_for_child\r\n:child_started\r\nping -n 10 127.0.0.1 > nul\r\n",
        )
        .expect("write parent script");
        let hook = Hook::new(HookEvent::ToolCallBefore, "call parent.cmd").with_timeout(3);
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());

        let result = executor.execute_sync(&hook, &HashMap::new());
        assert!(
            result
                .error
                .as_ref()
                .is_some_and(|error| error.contains("timed out")),
            "hook should time out: {result:?}"
        );
        assert!(
            started.exists(),
            "descendant never reached its start handshake"
        );
        std::thread::sleep(Duration::from_secs(5));
        assert!(
            !survived.exists(),
            "the timed-out hook's descendant escaped its Job Object"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_stdin_write_does_not_deadlock_when_hook_writes_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "write_before_read.sh",
            r#"#!/bin/sh
dd if=/dev/zero bs=1024 count=256 2>/dev/null | tr '\000' x
dd if=/dev/zero bs=1024 count=256 2>/dev/null | tr '\000' e >&2
payload=$(cat)
printf '\ndone:%s\n' "${#payload}"
"#,
        );
        let hook = Hook::new(HookEvent::MessageSubmit, &command).with_timeout(5);
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());
        let env_vars = HashMap::new();
        let payload = json!({
            "event": "message_submit",
            "text": "x".repeat(256 * 1024),
        });

        let result = executor.execute_sync_with_stdin(&hook, &env_vars, &payload);

        assert!(result.success, "hook should complete: {result:?}");
        assert!(result.stdout.ends_with("…[truncated]"));
        assert!(result.stderr.ends_with("…[truncated]"));
        assert!(result.stdout.len() <= HOOK_PIPE_CAPTURE_MAX_BYTES + 16);
        assert!(result.stderr.len() <= HOOK_PIPE_CAPTURE_MAX_BYTES + 16);
    }

    #[test]
    fn test_executor_session_id() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));

        assert!(executor.session_id().starts_with("sess_"));
        assert_eq!(executor.session_id().len(), 13); // "sess_" + 8 chars
    }

    #[cfg(not(windows))]
    fn write_hook_script(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write hook script");
        format!("sh {}", path.display())
    }

    #[cfg(not(windows))]
    fn submit_context(dir: &tempfile::TempDir) -> HookContext {
        HookContext::new()
            .with_session_id("sess_test")
            .with_workspace(dir.path().to_path_buf())
            .with_mode("agent")
            .with_model("deepseek-test")
            .with_tokens(42)
    }

    #[cfg(not(windows))]
    #[test]
    fn json_observer_hook_receives_structured_stdin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("payload.json");
        let command = write_hook_script(
            &dir,
            "capture_observer.sh",
            &format!(
                r#"#!/bin/sh
cat > "{}"
"#,
                out.display()
            ),
        );
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::SubagentSpawn, &command)],
                ..Default::default()
            },
            dir.path().to_path_buf(),
        );
        let payload = json!({
            "event": "subagent_spawn",
            "agent_id": "agent_123",
            "prompt_preview": "inspect this",
            "prompt_truncated": false,
        });

        let results = executor.execute_json_observer(
            HookEvent::SubagentSpawn,
            &submit_context(&dir),
            &payload,
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        let captured: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out).expect("payload written"))
                .expect("valid JSON payload");
        assert_eq!(captured["event"], "subagent_spawn");
        assert_eq!(captured["agent_id"], "agent_123");
        assert_eq!(captured["prompt_preview"], "inspect this");
        assert_eq!(captured["prompt_truncated"], false);
    }

    #[cfg(not(windows))]
    #[test]
    fn turn_end_observer_hook_receives_stdin_json_and_ignores_stdout_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("turn_end.json");
        let command = write_hook_script(
            &dir,
            "capture_turn_end.sh",
            &format!(
                r#"#!/bin/sh
cat > "{}"
printf '%s\n' '{{"text":"stdout is not a mutation contract"}}'
"#,
                out.display()
            ),
        );
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::TurnEnd, &command)],
                ..Default::default()
            },
            dir.path().to_path_buf(),
        );
        let usage = codewhale_models::Usage {
            input_tokens: 12,
            output_tokens: 3,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            prompt_cache_write_tokens: None,
            reasoning_tokens: None,
            reasoning_replay_tokens: None,
            server_tool_use: None,

            input_audio_tokens: None,
            output_audio_tokens: None,

            request_usages: Vec::new(),
        };
        let context = submit_context(&dir).with_tokens(15);
        let payload = super::turn_end_payload(TurnEndPayloadInput {
            context: &context,
            created_at: "2026-07-12T10:30:00Z".parse().expect("timestamp"),
            model_backed: true,
            provider: Some("openai"),
            billing_surface: None,
            model: Some("gpt-5.5"),
            turn_id: "turn_observed",
            status: "completed",
            error: None,
            duration: Duration::from_millis(7),
            usage: &usage,
            totals: TurnEndTotals {
                session_tokens: 15,
                conversation_tokens: 15,
                input_tokens: 12,
                output_tokens: 3,
            },
            tool_count: 0,
            queued_message_count: 0,
        });

        let results = executor.execute_json_observer(HookEvent::TurnEnd, &context, &payload);

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert!(
            results[0]
                .stdout
                .contains("stdout is not a mutation contract"),
            "stdout is still captured for diagnostics"
        );
        let captured: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out).expect("payload written"))
                .expect("valid JSON payload");
        assert_eq!(captured["event"], "turn_end");
        assert_eq!(captured["created_at"], "2026-07-12T10:30:00+00:00");
        assert_eq!(captured["provider"], "openai");
        assert_eq!(captured["model"], "gpt-5.5");
        assert_eq!(captured["turn_id"], "turn_observed");
        assert_eq!(captured["totals"]["input_tokens"], 12);
        assert_eq!(captured["totals"]["output_tokens"], 3);
    }

    #[cfg(not(windows))]
    #[test]
    fn json_observer_hook_failure_does_not_stop_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("later-ran");
        let failing = write_hook_script(
            &dir,
            "failing_observer.sh",
            r#"#!/bin/sh
echo boom >&2
exit 1
"#,
        );
        let later = write_hook_script(
            &dir,
            "later_observer.sh",
            &format!(
                r#"#!/bin/sh
cat > "{}"
"#,
                marker.display()
            ),
        );
        let mut first = Hook::new(HookEvent::SubagentComplete, &failing);
        first.continue_on_error = false;
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![first, Hook::new(HookEvent::SubagentComplete, &later)],
                ..Default::default()
            },
            dir.path().to_path_buf(),
        );
        let payload = json!({
            "event": "subagent_complete",
            "agent_id": "agent_456",
            "status": "completed",
        });

        let results = executor.execute_json_observer(
            HookEvent::SubagentComplete,
            &submit_context(&dir),
            &payload,
        );

        assert_eq!(results.len(), 2);
        assert!(!results[0].success);
        assert!(results[1].success);
        assert!(
            marker.exists(),
            "observer failures must be warn-only and non-blocking"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_applies_hooks_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_hook_script(
            &dir,
            "first.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"first"}'
"#,
        );
        let second = write_hook_script(
            &dir,
            "second.sh",
            r#"#!/bin/sh
payload=$(cat)
case "$payload" in
  *'"text":"first"'*) printf '%s\n' '{"text":"first second"}' ;;
  *) printf '%s\n' '{"text":"wrong"}' ;;
esac
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &first),
                Hook::new(HookEvent::MessageSubmit, &second),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("first second".to_string())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_exit_two_blocks_submission() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "block.sh",
            r#"#!/bin/sh
printf '%s\n' '{"reason":"policy blocked this prompt"}'
exit 2
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::MessageSubmit, &command)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::Blocked {
                reason: "policy blocked this prompt".to_string()
            }
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn background_message_submit_hook_is_observer_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "background.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"ignored"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::MessageSubmit, &command).background()],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[test]
    fn message_submit_transform_without_configured_hooks_is_unchanged() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));

        assert_eq!(
            executor.execute_message_submit_transform(&HookContext::new(), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_transform_skips_non_matching_condition() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "replace.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"should not apply"}'
"#,
        );
        let hook =
            Hook::new(HookEvent::MessageSubmit, &command).with_condition(HookCondition::Mode {
                mode: "plan".into(),
            });
        let config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::unchanged()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_continue_on_error_true_keeps_text_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let failing = write_hook_script(
            &dir,
            "fail_continue.sh",
            r#"#!/bin/sh
printf '%s\n' 'soft failure' >&2
exit 9
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_failure.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"recovered"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &failing),
                Hook::new(HookEvent::MessageSubmit, &replacing),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("recovered".to_string())
                .with_warning(Some("message_submit hook exited with code 9".to_string()))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_timeout_continue_surfaces_warning_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slow = write_hook_script(
            &dir,
            "slow_continue.sh",
            r#"#!/bin/sh
sleep 2
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_timeout.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"after timeout"}'
"#,
        );
        let mut slow_hook = Hook::new(HookEvent::MessageSubmit, &slow).with_timeout(1);
        slow_hook.continue_on_error = true;
        let config = HooksConfig {
            enabled: true,
            hooks: vec![slow_hook, Hook::new(HookEvent::MessageSubmit, &replacing)],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("after timeout".to_string())
                .with_warning(Some("hook timed out after 1s".to_string()))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_invalid_stdout_keeps_text_and_runs_later_hooks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let invalid = write_hook_script(
            &dir,
            "invalid_stdout.sh",
            r#"#!/bin/sh
printf '%s\n' 'not json'
"#,
        );
        let replacing = write_hook_script(
            &dir,
            "replace_after_invalid.sh",
            r#"#!/bin/sh
printf '%s\n' '{"text":"valid later"}'
"#,
        );
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::MessageSubmit, &invalid),
                Hook::new(HookEvent::MessageSubmit, &replacing),
            ],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::replaced("valid later".to_string())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn message_submit_continue_on_error_false_blocks_on_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = write_hook_script(
            &dir,
            "fail.sh",
            r#"#!/bin/sh
printf '%s\n' 'hard failure' >&2
exit 7
"#,
        );
        let mut hook = Hook::new(HookEvent::MessageSubmit, &command);
        hook.continue_on_error = false;
        let config = HooksConfig {
            enabled: true,
            hooks: vec![hook],
            working_dir: Some(dir.path().to_path_buf()),
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, dir.path().to_path_buf());

        assert_eq!(
            executor.execute_message_submit_transform(&submit_context(&dir), "original"),
            MessageSubmitOutcome::Blocked {
                reason: "message_submit hook failed and blocked submission".to_string()
            }
        );
    }

    #[test]
    fn has_hooks_for_event_fast_path_returns_false_for_empty_config() {
        let executor = HookExecutor::disabled();
        // No hooks configured AT ALL — every event is a fast skip.
        for event in [
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::MessageSubmit,
            HookEvent::ToolCallBefore,
            HookEvent::ToolCallAfter,
            HookEvent::ModeChange,
            HookEvent::OnError,
            HookEvent::TurnEnd,
            HookEvent::SubagentSpawn,
            HookEvent::SubagentComplete,
        ] {
            assert!(
                !executor.has_hooks_for_event(event),
                "empty config must short-circuit for {event:?}"
            );
        }
    }

    #[test]
    fn has_hooks_for_event_returns_false_when_globally_disabled() {
        let config = HooksConfig {
            enabled: false,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo blocked")],
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, PathBuf::from("."));
        assert!(
            !executor.has_hooks_for_event(HookEvent::ToolCallBefore),
            "globally-disabled hooks must report no fires even when one is configured"
        );
    }

    #[test]
    fn has_hooks_for_event_distinguishes_event_types() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![
                Hook::new(HookEvent::SessionStart, "echo start"),
                Hook::new(HookEvent::ToolCallBefore, "echo before"),
            ],
            ..HooksConfig::default()
        };
        let executor = HookExecutor::new(config, PathBuf::from("."));
        // Configured events return true.
        assert!(executor.has_hooks_for_event(HookEvent::SessionStart));
        assert!(executor.has_hooks_for_event(HookEvent::ToolCallBefore));
        // Unconfigured events return false even when other events are present.
        assert!(!executor.has_hooks_for_event(HookEvent::ToolCallAfter));
        assert!(!executor.has_hooks_for_event(HookEvent::OnError));
        assert!(!executor.has_hooks_for_event(HookEvent::ModeChange));
    }

    // ── #3026: tool_call_before stdout decision contract ──────────────────

    #[test]
    fn tool_call_before_stdout_parses_deny_with_reason() {
        let parsed =
            parse_tool_call_before_stdout(r#"{"decision":"deny","reason":"blocked by policy"}"#);
        assert_eq!(parsed.decision, Some(ToolCallDecision::Deny));
        assert_eq!(parsed.reason.as_deref(), Some("blocked by policy"));
        assert!(parsed.updated_input.is_none());
        assert!(parsed.additional_context.is_none());
    }

    #[test]
    fn tool_call_before_stdout_parses_ask_and_allow() {
        let ask = parse_tool_call_before_stdout(r#"{"decision":"ask"}"#);
        assert_eq!(ask.decision, Some(ToolCallDecision::Ask));

        let allow = parse_tool_call_before_stdout(r#"{"decision":"allow"}"#);
        assert_eq!(allow.decision, Some(ToolCallDecision::Allow));
    }

    #[test]
    fn tool_call_before_stdout_parses_updated_input_object() {
        let parsed =
            parse_tool_call_before_stdout(r#"{"updatedInput":{"command":"ls -la","timeout":5}}"#);
        assert!(parsed.decision.is_none());
        assert_eq!(
            parsed.updated_input,
            Some(serde_json::json!({"command":"ls -la","timeout":5}))
        );
    }

    #[test]
    fn tool_call_before_stdout_rejects_non_object_updated_input() {
        let parsed = parse_tool_call_before_stdout(r#"{"updatedInput":"rm -rf /"}"#);
        assert!(
            parsed.updated_input.is_none(),
            "updatedInput must be a JSON object"
        );
        let parsed = parse_tool_call_before_stdout(r#"{"updatedInput":[1,2]}"#);
        assert!(parsed.updated_input.is_none());
    }

    #[test]
    fn tool_call_before_stdout_parses_additional_context() {
        let parsed =
            parse_tool_call_before_stdout(r#"{"additionalContext":"remember the style guide"}"#);
        assert_eq!(
            parsed.additional_context.as_deref(),
            Some("remember the style guide")
        );
    }

    #[test]
    fn tool_call_before_stdout_empty_and_non_json_are_passthrough() {
        for stdout in ["", "   \n  ", "ok, proceeding", "exit code zero"] {
            let parsed = parse_tool_call_before_stdout(stdout);
            assert!(parsed.decision.is_none(), "stdout {stdout:?}");
            assert!(parsed.reason.is_none());
            assert!(parsed.updated_input.is_none());
            assert!(parsed.additional_context.is_none());
        }
    }

    #[test]
    fn tool_call_before_stdout_json_without_decision_is_passthrough() {
        let parsed = parse_tool_call_before_stdout(r#"{"status":"fine"}"#);
        assert!(parsed.decision.is_none());
    }

    #[test]
    fn tool_call_before_stdout_non_object_json_is_passthrough() {
        for stdout in [r#""deny""#, "[1,2,3]", "42", "true"] {
            let parsed = parse_tool_call_before_stdout(stdout);
            assert!(parsed.decision.is_none(), "stdout {stdout:?}");
        }
    }

    #[test]
    fn tool_call_before_stdout_unknown_decision_treated_as_allow() {
        let parsed = parse_tool_call_before_stdout(r#"{"decision":"block"}"#);
        assert!(parsed.decision.is_none());
    }

    // ── #3026: glob matchers for tool_name conditions ──────────────────────

    /// DOCS-04: the documented `mcp__*` glob must match the name the model
    /// actually calls (built by `McpPool::mcp_model_tool_name`, which is
    /// `mcp_<server>_<tool>`), and must not catch the built-in MCP helpers.
    #[test]
    fn mcp_glob_matches_real_model_tool_names_by_owning_server() {
        let served = crate::mcp::McpPool::mcp_model_tool_name("github", "create_issue");
        let other = crate::mcp::McpPool::mcp_model_tool_name("wiki", "lookup");
        let matches = HookExecutor::tool_name_matches_condition;

        assert!(matches(&served, "mcp__*"), "{served} must match mcp__*");
        assert!(matches(&served, "mcp_*"), "{served} must match mcp_*");
        assert!(matches(&served, "mcp__github__*"));
        assert!(!matches(&other, "mcp__github__*"));
        assert!(matches(&served, "mcp__github__create_issue"));
        assert!(matches(&served, "mcp__*__create_issue"));
        assert!(!matches(&other, "mcp__*__create_issue"));

        for helper in [
            "mcp_read_resource",
            "mcp_get_prompt",
            "list_mcp_resources",
            "list_mcp_resource_templates",
            "read_mcp_resource",
        ] {
            assert!(!matches(helper, "mcp__*"), "{helper} is built in");
            assert!(!matches(helper, "mcp_*"), "{helper} is built in");
            // Exact names still select a helper deliberately.
            assert!(matches(helper, helper));
        }
        assert!(!matches("read_file", "mcp__*"));
    }

    #[test]
    fn tool_name_exact_match_still_works() {
        assert!(HookExecutor::tool_name_matches_condition(
            "read_file",
            "read_file"
        ));
        assert!(!HookExecutor::tool_name_matches_condition(
            "read_files",
            "read_file"
        ));
    }

    #[test]
    fn tool_name_shell_spellings_match_each_other_in_both_directions() {
        let spellings = ["bash", "Bash", "exec_shell"];
        for tool in spellings {
            for pattern in spellings {
                assert!(
                    HookExecutor::tool_name_matches_condition(tool, pattern),
                    "tool {tool} should match condition {pattern}"
                );
            }
        }
        // The alias is exact: it does not widen to other shell-ish tools.
        assert!(!HookExecutor::tool_name_matches_condition(
            "task_shell_start",
            "bash"
        ));
        assert!(!HookExecutor::tool_name_matches_condition(
            "bash",
            "read_file"
        ));
        assert!(!HookExecutor::tool_name_matches_condition("BASH", "bash"));
    }

    #[test]
    fn tool_name_glob_escapes_regex_metacharacters() {
        // Without escaping, `.` would match any character.
        assert!(!HookExecutor::tool_name_matches_condition(
            "mcpXgithub",
            "mcp.git*"
        ));
        assert!(HookExecutor::tool_name_matches_condition(
            "mcp.github",
            "mcp.git*"
        ));
        // `+` and parens must be literal too.
        assert!(HookExecutor::tool_name_matches_condition(
            "weird+tool(name)",
            "weird+tool(*)"
        ));
    }

    #[test]
    fn tool_name_glob_supports_infix_and_suffix_positions() {
        assert!(HookExecutor::tool_name_matches_condition(
            "task_shell_start",
            "*_shell_start"
        ));
        assert!(!HookExecutor::tool_name_matches_condition(
            "task_shell_wait",
            "*_shell_start"
        ));
    }

    // ── #3026: project-local hooks ─────────────────────────────────────────

    #[test]
    fn load_with_project_missing_file_keeps_global() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo global")],
            ..HooksConfig::default()
        };

        let merged = HooksConfig::load_with_project(global.clone(), dir.path());
        assert_eq!(merged.hooks.len(), 1);
        assert_eq!(merged.hooks[0].command, "echo global");
    }

    #[test]
    fn load_with_project_appends_project_hooks_after_global() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("user-config.toml");
        let _config = trust_workspace_for_project_hooks(dir.path(), &config_path);
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let project_dir = dir.path().join(".codewhale");
        std::fs::create_dir_all(&project_dir).expect("mkdir .codewhale");
        std::fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
event = "tool_call_before"
command = "echo project"
"#,
        )
        .expect("write hooks.toml");

        let global = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo global")],
            ..HooksConfig::default()
        };

        let (authority, _) = super::super::authority::review_project_hooks(dir.path()).unwrap();
        super::super::authority::approve_project_hooks(dir.path(), &authority.digest).unwrap();
        let merged = HooksConfig::load_with_project(global, dir.path());
        assert_eq!(merged.hooks.len(), 2);
        assert_eq!(
            merged.hooks[0].command, "echo global",
            "global hooks run first"
        );
        assert_eq!(
            merged.hooks[1].command, "echo project",
            "project hooks are appended after global"
        );
    }

    #[test]
    fn load_with_project_ignores_project_hooks_until_workspace_trusted() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", dir.path().join("config.toml"));
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let project_dir = dir.path().join(".codewhale");
        std::fs::create_dir_all(&project_dir).expect("mkdir .codewhale");
        std::fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
event = "tool_call_before"
command = "echo project"
"#,
        )
        .expect("write hooks.toml");

        let global = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo global")],
            ..HooksConfig::default()
        };

        let merged = HooksConfig::load_with_project(global, dir.path());
        assert_eq!(merged.hooks.len(), 1);
        assert_eq!(merged.hooks[0].command, "echo global");
    }

    #[test]
    fn load_with_project_ignores_project_local_legacy_trust_marker() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", dir.path().join("config.toml"));
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let project_dir = dir.path().join(".codewhale");
        let legacy_trust_dir = dir.path().join(".deepseek");
        std::fs::create_dir_all(&project_dir).expect("mkdir .codewhale");
        std::fs::create_dir_all(&legacy_trust_dir).expect("mkdir .deepseek");
        std::fs::write(legacy_trust_dir.join("trusted"), "").expect("write legacy trust marker");
        std::fs::write(
            project_dir.join("hooks.toml"),
            r#"
[[hooks]]
event = "tool_call_before"
command = "echo project"
"#,
        )
        .expect("write hooks.toml");

        let global = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo global")],
            ..HooksConfig::default()
        };

        let merged = HooksConfig::load_with_project(global, dir.path());
        assert_eq!(merged.hooks.len(), 1);
        assert_eq!(merged.hooks[0].command, "echo global");
    }

    #[test]
    fn load_with_project_malformed_file_falls_back_to_global() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("user-config.toml");
        let _config = trust_workspace_for_project_hooks(dir.path(), &config_path);
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let project_dir = dir.path().join(".codewhale");
        std::fs::create_dir_all(&project_dir).expect("mkdir .codewhale");
        std::fs::write(project_dir.join("hooks.toml"), "this is [ not toml")
            .expect("write hooks.toml");

        let global = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, "echo global")],
            ..HooksConfig::default()
        };

        let merged = HooksConfig::load_with_project(global, dir.path());
        assert_eq!(merged.hooks.len(), 1, "malformed project file is ignored");
        assert_eq!(merged.hooks[0].command, "echo global");
    }

    #[cfg(unix)]
    #[test]
    fn project_hooks_require_exact_review_at_load_and_every_spawn() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().unwrap();
        let _config = trust_workspace_for_project_hooks(dir.path(), &dir.path().join("user.toml"));
        let _legacy = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        std::fs::create_dir(dir.path().join(".codewhale")).unwrap();
        let hook_path = dir.path().join(".codewhale/hooks.toml");
        let contents = "[[hooks]]\nevent = \"session_start\"\ncommand = \"touch hook-ran\"\n";
        std::fs::write(&hook_path, contents).unwrap();
        let load = || {
            HooksConfig::load_with_project(
                HooksConfig {
                    enabled: true,
                    ..Default::default()
                },
                dir.path(),
            )
        };
        assert!(load().hooks.is_empty(), "folder trust is not hook approval");
        let (authority, _) = super::super::authority::review_project_hooks(dir.path()).unwrap();
        assert!(super::super::authority::approve_project_hooks(dir.path(), "bad-digest").is_err());
        super::super::authority::approve_project_hooks(dir.path(), &authority.digest).unwrap();
        let config = load();
        let hook = config.hooks[0].clone();
        let executor = HookExecutor::new(config, dir.path().to_path_buf());
        let good = executor.execute_sync(&hook, &HashMap::new());
        assert!(good.success, "{good:?}");
        std::fs::remove_file(dir.path().join("hook-ran")).unwrap();
        std::fs::write(&hook_path, format!("{contents}# changed\n")).unwrap();
        assert!(load().hooks.is_empty());
        assert!(!executor.execute_sync(&hook, &HashMap::new()).success);
        assert!(
            !executor
                .execute_background_inner(&hook, &HashMap::new(), None)
                .success
        );
        let queued = BackgroundHookJob {
            admitted: None,
            command: hook.command.clone(),
            env: HashMap::new(),
            working_dir: dir.path().to_path_buf(),
            stdin_bytes: None,
            label: "project".into(),
            timeout: Duration::from_secs(2),
            plugin_authority: None,
            project_authority: hook.project_authority.clone(),
        };
        queued.run();
        assert!(
            !dir.path().join("hook-ran").exists(),
            "queued work must revalidate"
        );
        std::fs::write(&hook_path, contents).unwrap();
        crate::config::save_workspace_hook_receipt(dir.path(), "").unwrap();
        assert!(!executor.execute_sync(&hook, &HashMap::new()).success);
        assert!(!dir.path().join("hook-ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn project_hook_approval_rejects_symlinks_and_repository_receipts() {
        let _lock = lock_test_env();
        let dir = tempfile::tempdir().unwrap();
        let _config = trust_workspace_for_project_hooks(dir.path(), &dir.path().join("user.toml"));
        let _legacy = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        std::fs::create_dir(dir.path().join(".codewhale")).unwrap();
        let target = dir.path().join("hook-source.toml");
        std::fs::write(
            &target,
            "[[hooks]]\nevent = \"session_start\"\ncommand = \"true\"\n",
        )
        .unwrap();
        let hook_path = dir.path().join(".codewhale/hooks.toml");
        std::os::unix::fs::symlink(&target, &hook_path).unwrap();
        assert!(super::super::authority::review_project_hooks(dir.path()).is_err());
        std::fs::remove_file(&hook_path).unwrap();
        std::fs::copy(&target, &hook_path).unwrap();
        let (authority, _) = super::super::authority::review_project_hooks(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(".codewhale/config.toml"),
            format!("hooks_sha256 = \"{}\"", authority.digest),
        )
        .unwrap();
        assert!(
            HooksConfig::load_with_project(HooksConfig::default(), dir.path())
                .hooks
                .is_empty()
        );
    }

    // === v0.9.2 hooks contract regression tests ===============================
    //
    // Each of these pins a claim that `docs/HOOKS.md` makes, so the docs cannot
    // drift ahead of the runtime again. All of them are provider-free: they
    // spawn `sh`, never a model.

    #[test]
    fn background_result_is_a_submission_not_an_observed_exit_code() {
        // `background` is the flag that keeps "queued" from reading as
        // "exited 0". Steering paths gate on `observed_exit_code`.
        let submitted = HookResult {
            background: true,
            success: true,
            exit_code: None,
            ..HookResult::default()
        };
        assert!(submitted.background);
        assert_eq!(submitted.observed_exit_code(), None);

        // A foreground deny still reads through.
        let denied = HookResult {
            background: false,
            success: false,
            exit_code: Some(2),
            ..HookResult::default()
        };
        assert_eq!(denied.observed_exit_code(), Some(2));

        // A foreground timeout has no exit code either, but it is *not* a
        // background submission — callers must be able to tell them apart.
        let timed_out = HookResult {
            background: false,
            success: false,
            exit_code: None,
            error: Some("Hook timed out after 1s".to_string()),
            ..HookResult::default()
        };
        assert!(!timed_out.background);
        assert_eq!(timed_out.observed_exit_code(), None);
    }

    #[test]
    fn default_timeout_secs_replaces_per_hook_timeout() {
        // Documented as-implemented: the global value overrides, it does not
        // merely fill in for hooks that omit one.
        let hook = Hook::new(HookEvent::SessionStart, "true").with_timeout(90);
        let overridden = HookExecutor::new(
            HooksConfig {
                default_timeout_secs: Some(5),
                ..HooksConfig::default()
            },
            PathBuf::from("."),
        );
        assert_eq!(overridden.effective_timeout_secs(&hook), 5);

        let per_hook = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        assert_eq!(per_hook.effective_timeout_secs(&hook), 90);
    }

    #[test]
    fn foreground_timeout_result_is_bounded_and_carries_no_payload() {
        // The timeout result must not leak the stdin payload, the environment,
        // or partial output back to the caller.
        let command = if cfg!(windows) {
            "ping -n 4 127.0.0.1 > nul"
        } else {
            "echo secret-stdout; sleep 5"
        };
        let hook = Hook::new(HookEvent::MessageSubmit, command).with_timeout(1);
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let payload = serde_json::json!({ "text": "super secret user text" });

        let result = executor.execute_sync_with_stdin(&hook, &HashMap::new(), &payload);

        assert!(!result.success);
        assert!(!result.background);
        assert_eq!(result.exit_code, None);
        assert!(result.stdout.is_empty(), "stdout leaked: {}", result.stdout);
        assert!(result.stderr.is_empty(), "stderr leaked: {}", result.stderr);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("timed out"), "{error}");
        assert!(!error.contains("super secret"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn background_hook_timeout_kills_and_reaps_its_process_tree() {
        // The claim under test: "There is no path on which a timed-out hook
        // keeps running." A background hook used to be waited on with an
        // unbounded `child.wait()`, so a runaway command outlived the session.
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("survived.txt");
        // The inner `sh -c ... &` is a grandchild: killing only the immediate
        // shell would leave it alive, so this also covers process-group kill.
        let command = format!(
            "sh -c 'sleep 4; echo survived > {}' & wait",
            marker.display()
        );
        let hook = Hook::new(HookEvent::SessionStart, &command).with_timeout(1);
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());

        let result = executor.execute_background(&hook, &HashMap::new());
        assert!(result.background, "background submission must be flagged");
        assert_eq!(result.observed_exit_code(), None);

        // Well past the hook's 1s budget, and past the 4s the command wanted.
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !marker.exists(),
            "background hook outlived its timeout and kept running"
        );
    }

    #[cfg(unix)]
    #[test]
    fn background_hook_receives_the_same_stdin_payload_as_foreground() {
        // Background changes scheduling, not the payload contract.
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("bg-stdin.json");
        let command = write_hook_script(
            &dir,
            "capture_bg_stdin.sh",
            &format!("#!/bin/sh\ncat > {}\n", out.display()),
        );
        let hook = Hook::new(HookEvent::MessageSubmit, &command)
            .with_name("bg")
            .background();
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![hook],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let context = submit_context(&dir);
        let outcome = executor.execute_message_submit_transform(&context, "hello world");
        // Background hooks cannot steer.
        assert_eq!(outcome, MessageSubmitOutcome::unchanged());

        let raw = wait_for_captured_output(&out);
        let payload: serde_json::Value = serde_json::from_str(raw.trim()).expect("valid JSON");
        assert_eq!(payload["event"], "message_submit");
        assert_eq!(payload["text"], "hello world");
        assert_eq!(payload["session_id"], "sess_test");
        assert_eq!(payload["mode"], "agent");
        assert_eq!(payload["model"], "deepseek-test");
        assert_eq!(payload["total_tokens"], 42);
    }

    #[cfg(unix)]
    #[test]
    fn background_hook_receives_the_documented_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("bg-env.txt");
        let command = write_hook_script(
            &dir,
            "capture_bg_env.sh",
            &format!(
                "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$DEEPSEEK_SESSION_ID\" \"$DEEPSEEK_MODE\" \
                 \"$DEEPSEEK_TOOL_NAME\" > {}\n",
                out.display()
            ),
        );
        let hook = Hook::new(HookEvent::ToolCallAfter, &command)
            .with_name("bg-env")
            .background();
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![hook],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let context = submit_context(&dir).with_tool_name("exec_shell");
        let results = executor.execute(HookEvent::ToolCallAfter, &context);
        assert_eq!(results.len(), 1);
        assert!(results[0].background);

        let captured = wait_for_captured_output(&out);
        assert_eq!(captured.trim(), "sess_test|agent|exec_shell");
    }

    /// Wait for a background hook's capture file to hold real bytes.
    ///
    /// The capture scripts redirect with `> out`, so the shell creates the
    /// file — empty — before `cat`/`printf` writes the payload. Polling for
    /// existence alone can win that race under load and read an empty capture,
    /// which surfaced in CI as `valid JSON: EOF while parsing a value` (#5929).
    /// Both captures are single small writes, so waiting for non-empty bytes
    /// means the write has landed without weakening what the tests assert.
    #[cfg(unix)]
    fn wait_for_captured_output(path: &std::path::Path) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(raw) = std::fs::read_to_string(path)
                && !raw.trim().is_empty()
            {
                return raw;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background hook wrote no output to {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn session_id_is_stable_across_every_event_and_survives_a_rebind() {
        // One TUI session, one `CODEWHALE_SESSION_ID`. The legacy
        // `DEEPSEEK_SESSION_ID` alias carries the same value so existing hook
        // records stay correlatable; assert both names over every event.
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let session_id = executor.session_id().to_string();
        assert!(
            session_id.starts_with("sess_"),
            "unexpected session id shape: {session_id}"
        );

        for event in crate::hooks::ALL_HOOK_EVENTS {
            let context = HookContext::new()
                .with_session_id(executor.session_id())
                .with_tool_name(event.as_str());
            let env = context.to_env_vars();
            assert_eq!(
                env.get("CODEWHALE_SESSION_ID"),
                Some(&session_id),
                "event `{}` reported a different Codewhale session id",
                event.as_str()
            );
            assert_eq!(
                env.get("DEEPSEEK_SESSION_ID"),
                Some(&session_id),
                "event `{}` reported a different legacy session id",
                event.as_str()
            );
        }

        // A workspace switch or trust decision reloads the hook set. It must
        // not mint a new identity.
        let rebound = executor.rebind(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::SessionStart, "true")],
                ..HooksConfig::default()
            },
            PathBuf::from("/tmp"),
        );
        assert_eq!(rebound.session_id(), session_id);
        assert_eq!(rebound.config().hooks.len(), 1);

        // A genuinely new executor is a genuinely new session.
        let fresh = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        assert_ne!(fresh.session_id(), session_id);
    }

    #[test]
    fn exit_code_condition_matches_only_a_real_exit_code() {
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let hook = Hook::new(HookEvent::ToolCallAfter, "true")
            .with_condition(HookCondition::ExitCode { code: 1 });

        // No exit code reported at all: must not match. Notably it must not be
        // satisfied by the failure flag either.
        let no_code = HookContext::new()
            .with_tool_name("read_file")
            .with_tool_result("boom", false, None);
        assert!(!executor.matches_condition(&hook, &no_code));

        // A different exit code: no match.
        let other_code = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_result("boom", false, Some(127));
        assert!(!executor.matches_condition(&hook, &other_code));

        // The real thing.
        let exact = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_result("boom", false, Some(1));
        assert!(executor.matches_condition(&hook, &exact));

        // Exit code 0 on a successful call is a real code and matches a
        // `code = 0` predicate.
        let zero_hook = Hook::new(HookEvent::ToolCallAfter, "true")
            .with_condition(HookCondition::ExitCode { code: 0 });
        let zero = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_result("ok", true, Some(0));
        assert!(executor.matches_condition(&zero_hook, &zero));
        assert!(!executor.matches_condition(&zero_hook, &no_code));
    }

    #[test]
    fn tool_call_id_is_exported_for_correlation() {
        let env = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_call_id("call_abc123")
            .to_env_vars();
        assert_eq!(
            env.get("CODEWHALE_TOOL_CALL_ID"),
            Some(&"call_abc123".to_string())
        );
        assert_eq!(
            env.get("DEEPSEEK_TOOL_CALL_ID"),
            Some(&"call_abc123".to_string())
        );

        // Absent when unknown — never synthesized.
        let without = HookContext::new()
            .with_tool_name("exec_shell")
            .to_env_vars();
        assert!(!without.contains_key("CODEWHALE_TOOL_CALL_ID"));
        assert!(!without.contains_key("DEEPSEEK_TOOL_CALL_ID"));
    }

    #[test]
    fn tool_exit_code_env_var_is_absent_when_the_tool_reported_none() {
        let with_code = HookContext::new()
            .with_tool_result("out", false, Some(3))
            .to_env_vars();
        assert_eq!(
            with_code.get("DEEPSEEK_TOOL_EXIT_CODE"),
            Some(&"3".to_string())
        );
        assert_eq!(
            with_code.get("DEEPSEEK_TOOL_SUCCESS"),
            Some(&"false".to_string())
        );

        let without_code = HookContext::new()
            .with_tool_result("out", false, None)
            .to_env_vars();
        assert!(!without_code.contains_key("DEEPSEEK_TOOL_EXIT_CODE"));
        assert_eq!(
            without_code.get("DEEPSEEK_TOOL_SUCCESS"),
            Some(&"false".to_string())
        );
    }

    #[test]
    fn payload_env_vars_are_bounded() {
        // Errors used to be the one unbounded field; a failed `exec_shell`
        // could push its whole output into `DEEPSEEK_ERROR`.
        let long = "x".repeat(20_000);
        let env = HookContext::new()
            .with_error(&long)
            .with_message(&long)
            .with_tool_result(&long, false, None)
            .to_env_vars();

        for key in ["DEEPSEEK_ERROR", "DEEPSEEK_MESSAGE", "DEEPSEEK_TOOL_RESULT"] {
            let value = env.get(key).unwrap_or_else(|| panic!("{key} missing"));
            assert!(value.len() < 20_000, "{key} was not truncated");
            assert!(value.ends_with("...[truncated]"), "{key} lost its marker");
        }
    }

    #[test]
    fn truncate_env_value_respects_utf8_boundaries() {
        // 4-byte characters straddling the cap must not panic or split.
        let value = "🐋".repeat(100);
        let truncated = super::truncate_env_value(&value, 10);
        assert!(truncated.ends_with("...[truncated]"));
        let head = truncated.trim_end_matches("...[truncated]");
        assert!(head.chars().all(|c| c == '🐋'));
        assert!(head.len() <= 12);
    }

    #[cfg(unix)]
    #[test]
    fn collect_shell_env_merges_later_hooks_over_earlier_ones() {
        // The documented merge: parsed verbatim, later hooks win, failures
        // contribute nothing and do not abort.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_hook_script(
            &dir,
            "env_first.sh",
            "#!/bin/sh\necho SHARED=first\necho ONLY_FIRST=1\n",
        );
        let second = write_hook_script(
            &dir,
            "env_second.sh",
            "#!/bin/sh\necho SHARED=second\necho QUOTED=\"has spaces\"\n",
        );
        let failing = write_hook_script(&dir, "env_fail.sh", "#!/bin/sh\necho NEVER=1\nexit 1\n");

        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(HookEvent::ShellEnv, &first).with_name("first"),
                    Hook::new(HookEvent::ShellEnv, &second).with_name("second"),
                    Hook::new(HookEvent::ShellEnv, &failing).with_name("failing"),
                ],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let context = HookContext::new().with_tool_name("exec_shell");
        let merged = executor.collect_shell_env(&context);

        assert_eq!(merged.get("SHARED"), Some(&"second".to_string()));
        assert_eq!(merged.get("ONLY_FIRST"), Some(&"1".to_string()));
        assert_eq!(merged.get("QUOTED"), Some(&"has spaces".to_string()));
        assert!(
            !merged.contains_key("NEVER"),
            "a failing shell_env hook must contribute nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_env_ignores_the_background_flag_and_still_collects_stdout() {
        // `background` is not honored here: the stdout IS the contract, so the
        // hook runs in the foreground regardless of how it is configured.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_hook_script(&dir, "env_bg.sh", "#!/bin/sh\necho FROM_BG=yes\n");
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(HookEvent::ShellEnv, &script)
                        .with_name("bg-shell-env")
                        .background(),
                ],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let merged = executor.collect_shell_env(&HookContext::new().with_tool_name("exec_shell"));
        assert_eq!(merged.get("FROM_BG"), Some(&"yes".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn shell_env_hook_receives_only_the_narrow_documented_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("shell-env-context.txt");
        let script = write_hook_script(
            &dir,
            "env_context.sh",
            &format!(
                "#!/bin/sh\nprintf 'name=%s args=%s session=%s mode=%s\\n' \
                 \"$DEEPSEEK_TOOL_NAME\" \"$DEEPSEEK_TOOL_ARGS\" \"$DEEPSEEK_SESSION_ID\" \
                 \"$DEEPSEEK_MODE\" > {}\n",
                out.display()
            ),
        );
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::ShellEnv, &script)],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let context = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_args(&serde_json::json!({ "command": "ls" }));
        let _ = executor.collect_shell_env(&context);

        let captured = std::fs::read_to_string(&out).expect("shell_env hook wrote nothing");
        assert!(captured.contains("name=exec_shell"), "{captured}");
        assert!(captured.contains(r#""command":"ls""#), "{captured}");
        // No session id or mode is supplied for this event, which is why a
        // `mode` condition on `shell_env` is rejected at load.
        assert!(captured.contains("session= "), "{captured}");
        assert!(captured.trim_end().ends_with("mode="), "{captured}");
    }

    /// Strictness has to travel on the result, because only the results tell
    /// you which hooks actually *matched* this call.
    #[cfg(unix)]
    #[test]
    fn results_carry_the_strictness_of_the_hook_that_produced_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut strict = Hook::new(HookEvent::ToolCallBefore, "true")
            .with_name("strict")
            .with_condition(HookCondition::ToolName {
                name: "write_file".to_string(),
            });
        strict.continue_on_error = false;
        let lenient = Hook::new(HookEvent::ToolCallBefore, "true")
            .with_name("lenient")
            .with_condition(HookCondition::ToolName {
                name: "exec_shell".to_string(),
            });

        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![strict, lenient],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        // Only the lenient hook matches an `exec_shell` call, so nothing about
        // this call is strict — even though a strict hook exists in config.
        let shell = executor.execute(
            HookEvent::ToolCallBefore,
            &HookContext::new().with_tool_name("exec_shell"),
        );
        assert_eq!(shell.len(), 1);
        assert_eq!(shell[0].name.as_deref(), Some("lenient"));
        assert!(!shell[0].strict);

        // The `write_file` call is the one the strict gate guards.
        let write = executor.execute(
            HookEvent::ToolCallBefore,
            &HookContext::new().with_tool_name("write_file"),
        );
        assert_eq!(write.len(), 1);
        assert_eq!(write[0].name.as_deref(), Some("strict"));
        assert!(write[0].strict);
    }

    #[cfg(unix)]
    #[test]
    fn background_results_are_never_strict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hook = Hook::new(HookEvent::ToolCallBefore, "true")
            .with_name("bg-strict")
            .background();
        hook.continue_on_error = false;
        let executor = HookExecutor::new(HooksConfig::default(), dir.path().to_path_buf());

        let result = executor.execute_background(&hook, &HashMap::new());
        assert!(result.background);
        assert!(
            !result.strict,
            "nothing is awaited, so there is no answer to withhold"
        );
    }

    /// A background hook that never reads stdin used to hang the supervising
    /// thread forever: the payload was written synchronously *before*
    /// `wait_timeout`, so a payload larger than the pipe buffer blocked, and
    /// the timeout / kill / reap below it were never reached.
    #[cfg(unix)]
    #[test]
    fn oversized_background_stdin_still_times_out_and_kills_the_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("survived.txt");
        // Never reads stdin, and spawns a grandchild so this also covers the
        // process-group kill that the blocked write used to prevent.
        let command = write_hook_script(
            &dir,
            "ignores_stdin.sh",
            &format!(
                "#!/bin/sh\nsh -c 'sleep 6; echo survived > {}' &\nsleep 6\n",
                marker.display()
            ),
        );
        let hook = Hook::new(HookEvent::TurnEnd, &command)
            .with_name("deaf")
            .background()
            .with_timeout(1);
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![hook.clone()],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        // Far beyond any pipe buffer (64 KiB on Linux, 8–64 KiB on macOS).
        let payload = json!({ "event": "turn_end", "blob": "x".repeat(4 * 1024 * 1024) });

        let submitted = Instant::now();
        let result = executor.execute_background_with_stdin(&hook, &HashMap::new(), &payload);
        assert!(
            submitted.elapsed() < Duration::from_secs(2),
            "submission blocked on the stdin write: {:?}",
            submitted.elapsed()
        );
        assert!(result.background);
        assert!(result.success, "submission failed: {result:?}");

        // Past the hook's 1s budget and past the 6s the command wanted.
        std::thread::sleep(Duration::from_secs(8));
        assert!(
            !marker.exists(),
            "background hook with an unread oversized stdin outlived its timeout"
        );
    }

    #[test]
    fn spawn_failure_messages_carry_no_command_or_path() {
        let error = std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "'C:\\Users\\dev\\secret hooks\\gate.cmd' is not recognized",
        );
        let message = super::spawn_failure_message(&error);
        assert!(message.contains("NotFound"), "{message}");
        assert!(!message.contains("gate.cmd"), "{message}");
        assert!(!message.contains("C:\\"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn parse_env_lines_drops_nul_bearing_entries() {
        // `Command::env` panics on a NUL in a key or value, so a hook that
        // prints binary garbage must contribute nothing rather than take the
        // tool call down with it.
        let parsed = super::parse_env_lines("GOOD=fine\nBAD=tok\0en\nBA\0D2=x\nALSO_GOOD=2\n");
        assert_eq!(parsed.get("GOOD"), Some(&"fine".to_string()));
        assert_eq!(parsed.get("ALSO_GOOD"), Some(&"2".to_string()));
        assert!(!parsed.contains_key("BAD"), "{parsed:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        for (key, value) in &parsed {
            assert!(!key.contains('\0'));
            assert!(!value.contains('\0'));
            // The invariants `Command::env` asserts on.
            assert!(!key.is_empty() && !key.contains('='));
        }
    }

    #[test]
    fn parse_env_lines_bounds_values_and_the_aggregate() {
        let huge = "x".repeat(super::SHELL_ENV_VALUE_MAX_BYTES + 1);
        let parsed = super::parse_env_lines(&format!("OK=1\nHUGE={huge}\n"));
        assert_eq!(parsed.get("OK"), Some(&"1".to_string()));
        assert!(!parsed.contains_key("HUGE"), "over-long value was kept");

        // Many individually-legal values still cannot add up to an unbounded
        // environment.
        let chunk = "y".repeat(16 * 1024);
        let mut stdout = String::new();
        for i in 0..64 {
            stdout.push_str(&format!("K{i}={chunk}\n"));
        }
        let bulk = super::parse_env_lines(&stdout);
        let total: usize = bulk.iter().map(|(k, v)| k.len() + v.len()).sum();
        assert!(total <= super::SHELL_ENV_TOTAL_MAX_BYTES, "{total} bytes");
        assert!(!bulk.is_empty(), "the bound must not drop everything");
    }

    #[cfg(unix)]
    #[test]
    fn shell_env_hook_printing_nul_contributes_nothing_and_does_not_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_hook_script(
            &dir,
            "env_nul.sh",
            "#!/bin/sh\nprintf 'TOKEN=abc\\000def\\n'\nprintf 'SAFE=ok\\n'\n",
        );
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::ShellEnv, &script).with_name("nul")],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        let merged = executor.collect_shell_env(&HookContext::new().with_tool_name("exec_shell"));
        assert!(!merged.contains_key("TOKEN"), "{merged:?}");
        assert_eq!(merged.get("SAFE"), Some(&"ok".to_string()));
        // What `Command::env` would be handed must be panic-free.
        for (key, value) in &merged {
            assert!(!key.is_empty());
            assert!(!key.contains('=') && !key.contains('\0'));
            assert!(!value.contains('\0'));
        }
    }

    #[test]
    fn tool_call_before_text_fields_are_sanitized_and_bounded() {
        let long = "z".repeat(super::HOOK_TEXT_FIELD_MAX_CHARS * 3);
        let stdout = serde_json::json!({
            "decision": "deny",
            "reason": format!("blocked\u{1b}[31m {long}"),
            "additionalContext": format!("ctx\u{0}\r\nline {long}"),
        })
        .to_string();

        let parsed = super::parse_tool_call_before_stdout(&stdout);

        let reason = parsed.reason.expect("reason kept");
        assert!(reason.chars().count() <= super::HOOK_TEXT_FIELD_MAX_CHARS + 16);
        assert!(reason.ends_with("…[truncated]"), "{reason}");
        assert!(!reason.contains('\u{1b}'), "escape sequence survived");

        let context = parsed.additional_context.expect("context kept");
        assert!(context.chars().count() <= super::HOOK_TEXT_FIELD_MAX_CHARS + 16);
        assert!(!context.contains('\u{0}'));
        assert!(!context.contains('\r'));
        // Legitimate multi-line context still survives.
        assert!(
            context.contains('\n'),
            "{}",
            &context[..40.min(context.len())]
        );
    }

    #[test]
    fn hook_context_bounds_tool_args_environment_value() {
        let env = HookContext::new()
            .with_tool_args(&serde_json::json!({
                "command": "x".repeat(super::HOOK_TOOL_ARGS_ENV_MAX_BYTES * 3)
            }))
            .to_env_vars();
        let args = env.get("DEEPSEEK_TOOL_ARGS").expect("tool args env");
        assert!(
            args.len() <= super::HOOK_TOOL_ARGS_ENV_MAX_BYTES + "...[truncated]".len(),
            "{} bytes",
            args.len()
        );
        assert!(args.ends_with("...[truncated]"));
    }

    #[test]
    fn steering_objects_and_replacement_messages_have_independent_caps() {
        let oversized_input = serde_json::json!({
            "updatedInput": { "command": "x".repeat(super::HOOK_UPDATED_INPUT_MAX_BYTES * 2) }
        })
        .to_string();
        assert!(
            parse_tool_call_before_stdout(&oversized_input)
                .updated_input
                .is_none()
        );

        let oversized_message = serde_json::json!({
            "text": "x".repeat(super::HOOK_MESSAGE_REPLACEMENT_MAX_CHARS + 1)
        })
        .to_string();
        assert!(matches!(
            super::parse_message_submit_stdout(&oversized_message),
            super::MessageSubmitStdout::Invalid(reason)
                if reason.contains("exceeds")
        ));
    }

    #[test]
    fn turn_end_error_is_sanitized_and_bounded() {
        let context = HookContext::new();
        let usage = codewhale_models::Usage::default();
        let error = format!(
            "boom\u{1b}[2J{}",
            "x".repeat(super::HOOK_TURN_ERROR_MAX_CHARS * 2)
        );
        let payload = super::turn_end_payload(TurnEndPayloadInput {
            context: &context,
            created_at: chrono::Utc::now(),
            model_backed: true,
            provider: Some("test"),
            billing_surface: None,
            model: Some("test-model"),
            turn_id: "turn_test",
            status: "failed",
            error: Some(&error),
            duration: Duration::from_millis(1),
            usage: &usage,
            totals: TurnEndTotals {
                session_tokens: 0,
                conversation_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
            },
            tool_count: 0,
            queued_message_count: 0,
        });
        let rendered = payload["error"].as_str().expect("bounded error");
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.ends_with("…[truncated]"));
        assert!(
            rendered.chars().count() <= super::HOOK_TURN_ERROR_MAX_CHARS + 16,
            "{} chars",
            rendered.chars().count()
        );
    }

    #[test]
    fn denial_reason_redacts_paths_arguments_and_secret_assignments() {
        let rendered = super::sanitize_hook_denial_reason(
            "denied /Users/alice/private --command token=SUPERSECRET safe",
        );
        assert_eq!(rendered, "denied [path] [argument] [secret] safe");
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("SUPERSECRET"));
        assert!(!rendered.contains("--command"));

        let command = super::sanitize_hook_denial_reason("blocked command rm bearer abc123");
        assert_eq!(command, "blocked [command] [command] [secret] [secret]");
        assert!(!command.contains("rm"));
        assert!(!command.contains("abc123"));
    }

    #[test]
    fn denial_reason_redacts_adversarial_header_path_and_command_forms() {
        for reason in [
            r#"Denied Authorization: Bearer TOPSECRET path="/Users/alice/private key" command='rm -rf /tmp/private' safe"#,
            r#"Denied authorization:"Bearer TOPSECRET" path=../private command="curl --header secret" safe"#,
            r#"Denied (Authorization: Bearer TOPSECRET), path = C:\private command = "powershell -enc SECRET" safe"#,
        ] {
            let rendered = super::sanitize_hook_denial_reason(reason);
            for secret in [
                "TOPSECRET",
                "alice",
                "private key",
                "../private",
                "C:\\private",
                "curl",
                "powershell",
                "SECRET",
            ] {
                assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
            }
            assert!(rendered.contains("[secret]"), "{rendered}");
            assert!(rendered.contains("[path]"), "{rendered}");
            assert!(rendered.contains("[command]"), "{rendered}");
        }
    }

    #[test]
    fn denial_reason_redacts_auth_schemes_normalized_secrets_and_relative_paths() {
        for reason in [
            "Denied Authorization: Basic dXNlcjpwYXNz src/private/config.toml",
            "Denied Authorization=Digest deadbeef service.API-KEY=topsecret",
            "Denied authorization Negotiate kerberos AWS_SESSION_TOKEN=abc123",
            "Denied authorization NTLM credential internal_secret=hunter2",
            "Denied authorization Proprietary-Scheme opaque-credential src/private/key.txt",
        ] {
            let rendered = super::sanitize_hook_denial_reason(reason);
            for sensitive in [
                "dXNlcjpwYXNz",
                "deadbeef",
                "kerberos",
                "credential",
                "topsecret",
                "abc123",
                "hunter2",
                "src/private/config.toml",
                "opaque-credential",
                "src/private/key.txt",
            ] {
                assert!(
                    !rendered.contains(sensitive),
                    "leaked {sensitive}: {rendered}"
                );
            }
            assert!(rendered.contains("[secret]"), "{rendered}");
        }
    }

    #[test]
    fn observer_dispatch_failures_are_event_specific_and_fixed() {
        let config = HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::TurnEnd, "true")],
            ..HooksConfig::default()
        };
        let mut full = HookExecutor::new(config.clone(), PathBuf::from("."));
        full.inject_observer_dispatch_full_for_test();
        let error = full
            .submit_observer(HookEvent::TurnEnd, HookContext::new())
            .expect_err("full queue must be visible");
        assert_eq!(
            error,
            "turn_end observer hook queue is full; event was not submitted"
        );

        let mut disconnected = HookExecutor::new(config, PathBuf::from("."));
        disconnected.inject_observer_dispatch_disconnect_for_test();
        let error = disconnected
            .submit_observer(HookEvent::TurnEnd, HookContext::new())
            .expect_err("disconnected dispatcher must be visible");
        assert_eq!(
            error,
            "turn_end observer hook dispatcher is unavailable; event was not submitted"
        );
    }

    /// #6689: `DEEPSEEK_TOOL_EXECUTION_RECEIPT` is read from the metadata a
    /// shell tool recorded — on a failed call as well as a successful one —
    /// and is complete JSON or absent. The existing variables do not change.
    #[test]
    fn execution_receipt_env_is_complete_json_or_absent() {
        use crate::tools::spec::{ToolError, ToolResult};

        let receipt = json!({"schema_version": 1, "command": "printf effective",
            "cwd": "/tmp", "state": "completed", "scope": "local", "exit_code": 7,
            "stdout": "\u{1f40b}", "stderr": "", "stdout_truncated": false,
            "stderr_truncated": false, "output_kind": "separate"});
        let plain = HookContext::new()
            .with_tool_name("Bash")
            .with_tool_outcome(&Ok(ToolResult::success("out")));
        let legacy = plain.to_env_vars();
        assert!(!legacy.contains_key("DEEPSEEK_TOOL_EXECUTION_RECEIPT"));

        let with_receipt = HookContext::new()
            .with_tool_name("Bash")
            .with_tool_outcome(&Ok(
                ToolResult::success("out").with_metadata(json!({"execution_receipt": receipt}))
            ));
        let mut env = with_receipt.to_env_vars();
        let encoded = env
            .remove("DEEPSEEK_TOOL_EXECUTION_RECEIPT")
            .expect("receipt exported");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            receipt
        );
        assert_eq!(env, legacy, "existing variables are unchanged");

        let failed =
            HookContext::new().with_tool_outcome(&Err(ToolError::execution_failed_with_metadata(
                "boom",
                json!({"exit_code": 7, "execution_receipt": receipt}),
            )));
        assert!(
            failed
                .to_env_vars()
                .contains_key("DEEPSEEK_TOOL_EXECUTION_RECEIPT")
        );

        // An unknown schema or an oversized document is dropped, not cut.
        for bad in [
            json!({"schema_version": 2, "command": "x"}),
            json!({"command": "x"}),
            json!({"schema_version": 1,
                "stdout": "x".repeat(super::HOOK_EXECUTION_RECEIPT_MAX_BYTES)}),
        ] {
            let context = HookContext::new().with_tool_outcome(&Ok(
                ToolResult::success("out").with_metadata(json!({"execution_receipt": bad}))
            ));
            assert!(context.tool_execution_receipt.is_none());
        }
        let oversized = HookContext {
            tool_execution_receipt: Some("x".repeat(super::HOOK_EXECUTION_RECEIPT_MAX_BYTES + 1)),
            ..HookContext::new()
        };
        assert!(
            !oversized
                .to_env_vars()
                .contains_key("DEEPSEEK_TOOL_EXECUTION_RECEIPT")
        );
        assert!(
            oversized
                .bounded_for_observer()
                .tool_execution_receipt
                .is_none()
        );
    }

    fn shell_receipt_context() -> HookContext {
        HookContext::new()
            .with_tool_name("Bash")
            .with_session_id("session-receipt")
            .with_tool_call_id("call-receipt")
            .with_tool_args(&json!({"command": "requested, not executed", "cwd": "/wrong"}))
            .with_tool_outcome(&Ok(crate::tools::spec::ToolResult::success("preview")
                .with_metadata(json!({
                    "status": "Failed",
                    "exit_code": 7,
                    "execution_receipt": {
                        "schema_version": 1, "command": "printf effective; exit 7",
                        "cwd": std::env::temp_dir().to_str().unwrap(), "scope": "local",
                        "state": "completed", "exit_code": 7, "stdout": "effective",
                        "stderr": "diagnostic", "stdout_truncated": false,
                        "stderr_truncated": true, "output_kind": "separate"
                    }
                }))))
    }

    #[test]
    fn tool_after_stdin_uses_execution_evidence_and_truthful_bounds() {
        let mut context = shell_receipt_context();
        let original_env = context.to_env_vars();
        for name in ["bash", "Bash", "exec_shell"] {
            context.tool_name = Some(name.into());
            let payload = context.tool_after_payload().unwrap();
            assert_eq!(payload["schema_version"], 1);
            assert_eq!(payload["event"], "tool_call_after");
            assert_eq!(payload["tool_name"], name);
            let receipt = &payload["execution_receipt"];
            assert_eq!(receipt["command"], "printf effective; exit 7");
            assert_eq!(receipt["cwd"], std::env::temp_dir().to_str().unwrap());
            assert_eq!(receipt["completion"], "failed");
            assert_eq!(receipt["execution"], "started");
            assert_eq!(receipt["exit_code"], 7);
            assert_eq!(receipt["stderr_truncated"], true);
            assert_eq!(receipt["command_truncated"], false);
            assert_eq!(receipt["cwd_truncated"], false);
            assert_eq!(payload["session_id_truncated"], false);
            assert_eq!(payload["tool_call_id_truncated"], false);
            assert_eq!(payload["tool_name_truncated"], false);
        }
        context.tool_name = Some("Bash".into());
        assert_eq!(
            context.to_env_vars(),
            original_env,
            "legacy receipt is unchanged"
        );
        context.session_id = Some("用户\u{1}".repeat(20_000));
        context.tool_call_id = Some("鲸鱼".repeat(20_000));
        let payload = context.tool_after_payload().unwrap();
        assert_eq!(payload["session_id_truncated"], true);
        assert_eq!(payload["tool_call_id_truncated"], true);
        assert!(payload["session_id"].as_str().unwrap().len() <= 1_024 + 16);
        assert!(serde_json::to_vec(&payload).unwrap().len() <= 64 * 1024);

        for (completion, code) in [
            ("killed", json!(null)),
            ("timed_out", json!(null)),
            ("failed", json!(3_221_225_477_i64)),
            ("completed", json!(0)),
        ] {
            context.tool_status = Some(completion.into());
            let mut receipt: serde_json::Value =
                serde_json::from_str(context.tool_execution_receipt.as_deref().unwrap()).unwrap();
            receipt["exit_code"] = code.clone();
            context.tool_execution_receipt = Some(receipt.to_string());
            let payload = context.tool_after_payload().unwrap();
            assert_eq!(payload["execution_receipt"]["exit_code"], code);
            assert_eq!(payload["execution_receipt"]["completion"], completion);
        }
    }

    #[test]
    fn tool_after_stdin_has_no_receipt_for_unknown_or_unsupported_execution() {
        for name in ["mcp_shell", "task", "BASH"] {
            assert!(
                shell_receipt_context()
                    .with_tool_name(name)
                    .tool_after_payload()
                    .is_none()
            );
        }
        for status in [None, Some("running"), Some("unknown")] {
            let mut context = shell_receipt_context();
            context.tool_status = status.map(str::to_owned);
            assert!(context.tool_after_payload().is_none());
        }
        for receipt in [
            None,
            Some("not JSON".into()),
            Some("x".repeat(HOOK_EXECUTION_RECEIPT_MAX_BYTES + 1)),
        ] {
            let mut context = shell_receipt_context();
            context.tool_execution_receipt = receipt;
            assert!(context.tool_after_payload().is_none());
        }
        for (field, value) in [
            ("schema_version", json!(2)),
            ("scope", json!("remote")),
            ("state", json!("running")),
            ("command", json!("")),
            ("cwd", json!("relative")),
            ("exit_code", json!("0")),
            ("stdout_truncated", json!(null)),
            ("output_kind", json!("guessed")),
            ("output_kind", json!("combined")),
        ] {
            let mut context = shell_receipt_context();
            let mut receipt: serde_json::Value =
                serde_json::from_str(context.tool_execution_receipt.as_deref().unwrap()).unwrap();
            receipt[field] = value;
            context.tool_execution_receipt = Some(receipt.to_string());
            assert!(
                context.tool_after_payload().is_none(),
                "accepted invalid {field}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_after_stdin_delivers_real_shell_receipt_to_direct_and_queued_observers() {
        use crate::tools::spec::ToolSpec;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("payload.json");
        let command = write_hook_script(
            &dir,
            "capture.sh",
            &format!(
                "#!/bin/sh\ncat > '{}'\nprintf '%s' '{{\"decision\":\"deny\",\"updatedInput\":{{\"command\":\"false\"}}}}'\n",
                out.display()
            ),
        );
        for background in [false, true] {
            for queued in [false, true] {
                let mut hook = Hook::new(HookEvent::ToolCallAfter, &command);
                hook.background = background;
                let hooks = HookExecutor::new(
                    HooksConfig {
                        enabled: true,
                        hooks: vec![hook],
                        ..Default::default()
                    },
                    dir.path().to_owned(),
                );
                let mut tool_context = crate::tools::spec::ToolContext::new(dir.path())
                    .with_elevated_sandbox_policy(crate::sandbox::SandboxPolicy::DangerFullAccess);
                tool_context.auto_approve = true;
                tool_context.runtime.hook_executor = Some(Arc::new(hooks.clone()));
                let result = crate::tools::shell::BashTool::new("Bash")
                    .execute(
                        json!({"command": "printf actual; printf diagnostic >&2; exit 7"}),
                        &tool_context,
                    )
                    .await;
                let context = HookContext::new()
                    .with_tool_name("Bash")
                    .with_session_id("receipt-session")
                    .with_tool_call_id("receipt-call")
                    .with_tool_args(&json!({"command": "requested, not executed"}))
                    .with_tool_outcome(&result);
                if out.exists() {
                    std::fs::remove_file(&out).unwrap();
                }
                if queued {
                    hooks
                        .submit_observer(HookEvent::ToolCallAfter, context)
                        .unwrap();
                } else {
                    let results = hooks.execute(HookEvent::ToolCallAfter, &context);
                    assert_eq!(results.len(), 1);
                    assert!(results[0].success);
                }
                let payload: serde_json::Value =
                    serde_json::from_str(&wait_for_captured_output(&out)).unwrap();
                assert_eq!(payload["session_id"], "receipt-session");
                assert_eq!(payload["tool_call_id"], "receipt-call");
                let receipt = &payload["execution_receipt"];
                assert_eq!(
                    receipt["command"],
                    "printf actual; printf diagnostic >&2; exit 7"
                );
                assert_eq!(
                    receipt["cwd"],
                    dir.path().canonicalize().unwrap().to_str().unwrap()
                );
                assert_eq!(receipt["exit_code"], 7);
                assert_eq!(receipt["completion"], "failed");
                assert_eq!(receipt["stdout"], "actual");
                assert_eq!(receipt["stderr"], "diagnostic");
                assert_eq!(receipt["output_mode"], "separate");
                assert!(
                    !result.as_ref().unwrap().success,
                    "observer output cannot rewrite the settled call"
                );
            }
        }
    }

    /// An absent receipt must be absent in the actual child environment,
    /// even when a nested Codewhale inherited an outer hook's receipt.
    #[cfg(unix)]
    #[test]
    fn execution_receipt_never_inherits_another_calls_environment() {
        let _env = lock_test_env();
        let _stale = EnvVarGuard::set("DEEPSEEK_TOOL_EXECUTION_RECEIPT", "stale-outer-receipt");
        let current = r#"{"schema_version":1,"command":"current call"}"#;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("receipt-env.txt");
        let command = write_hook_script(
            &dir,
            "capture_receipt_env.sh",
            &format!(
                "#!/bin/sh\nprintf '%s' \"${{DEEPSEEK_TOOL_EXECUTION_RECEIPT-unset}}\" > {}\n",
                out.display()
            ),
        );
        for background in [false, true] {
            let mut hook = Hook::new(HookEvent::ToolCallAfter, &command);
            hook.background = background;
            let executor = HookExecutor::new(
                HooksConfig {
                    enabled: true,
                    hooks: vec![hook],
                    ..HooksConfig::default()
                },
                dir.path().to_path_buf(),
            );
            for (receipt, expected) in [
                (None, "unset"),
                (
                    Some("x".repeat(HOOK_EXECUTION_RECEIPT_MAX_BYTES + 1)),
                    "unset",
                ),
                (Some(current.to_string()), current),
            ] {
                if out.exists() {
                    std::fs::remove_file(&out).unwrap();
                }
                let context = HookContext {
                    tool_execution_receipt: receipt,
                    ..HookContext::new()
                };
                let results = executor.execute(HookEvent::ToolCallAfter, &context);
                assert_eq!(results.len(), 1);
                assert!(results[0].success);
                assert_eq!(wait_for_captured_output(&out), expected);
            }
        }
    }

    #[test]
    fn observer_context_is_bounded_before_enqueue() {
        let huge = "用户".repeat(20_000);
        let bounded = HookContext {
            tool_args: Some(huge.clone()),
            tool_result: Some(huge.clone()),
            error_message: Some(huge.clone()),
            message: Some(huge.clone()),
            model: Some(huge),
            ..HookContext::new()
        }
        .bounded_for_observer();

        assert!(bounded.tool_args.expect("args").len() <= super::HOOK_TOOL_ARGS_ENV_MAX_BYTES + 16);
        assert!(
            bounded.tool_result.expect("result").len()
                <= super::HOOK_TOOL_RESULT_CONTEXT_MAX_BYTES + 16
        );
        assert!(
            bounded.error_message.expect("error").len() <= super::HOOK_ERROR_CONTEXT_MAX_BYTES + 16
        );
        assert!(
            bounded.message.expect("message").len() <= super::HOOK_MESSAGE_CONTEXT_MAX_BYTES + 16
        );
        assert!(
            bounded.model.expect("model").len() <= super::HOOK_OBSERVER_METADATA_MAX_BYTES + 16
        );
    }

    #[test]
    fn background_supervisor_saturation_is_a_failed_submission() {
        let hook = Hook::new(HookEvent::TurnEnd, "true").background();
        let mut executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![hook],
                ..HooksConfig::default()
            },
            PathBuf::from("."),
        );
        executor.inject_background_supervisor_full_for_test();

        let results = executor.execute(HookEvent::TurnEnd, &HookContext::new());
        assert_eq!(results.len(), 1);
        assert!(results[0].background);
        assert!(!results[0].success);
        assert_eq!(
            results[0].error.as_deref(),
            Some("background hook supervisor queue is full")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn bounded_observer_dispatcher_executes_a_submitted_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = dir.path().join("observer-receipt.json");
        let command = write_hook_script(
            &dir,
            "persistent_observer.sh",
            &format!("#!/bin/sh\ncat > '{}'\n", receipt.display()),
        );
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::TurnEnd, &command)],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );
        executor
            .submit_json_observer(
                HookEvent::TurnEnd,
                HookContext::new(),
                serde_json::json!({"event": "turn_end", "turn_id": "turn_test"}),
            )
            .expect("bounded submission");

        let deadline = Instant::now() + Duration::from_secs(2);
        let payload = loop {
            if let Ok(raw) = std::fs::read_to_string(&receipt)
                && let Ok(payload) = serde_json::from_str::<serde_json::Value>(&raw)
            {
                break payload;
            }
            assert!(
                Instant::now() < deadline,
                "persistent worker did not finish a valid receipt"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(payload["turn_id"], "turn_test");
    }

    #[cfg(unix)]
    #[test]
    fn explicit_message_denial_never_copies_raw_process_diagnostics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = r#"printf '%s\n' '{"reason":"blocked /Users/alice/private --run token=SUPERSECRET"}'; printf '%s\n' 'stderr-secret /tmp/private' >&2; exit 2"#;
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![Hook::new(HookEvent::MessageSubmit, command)],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );
        let outcome = executor.execute_message_submit_transform(&HookContext::new(), "hello");
        let MessageSubmitOutcome::Blocked { reason } = outcome else {
            panic!("expected explicit block");
        };
        assert_eq!(reason, "blocked [path] [argument] [secret]");
        for secret in ["alice", "SUPERSECRET", "stderr-secret", "/tmp/private"] {
            assert!(!reason.contains(secret), "leaked {secret}: {reason}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn foreground_pipe_capture_is_bounded_while_verbose_child_is_drained() {
        let hook = Hook::new(
            HookEvent::SessionStart,
            "head -c 200000 /dev/zero | tr '\\0' o; head -c 200000 /dev/zero | tr '\\0' e >&2",
        )
        .with_timeout(5);
        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let result = executor.execute_sync(&hook, &HashMap::new());
        assert!(result.success, "{:?}", result.error);
        for output in [&result.stdout, &result.stderr] {
            assert!(output.ends_with("…[truncated]"));
            assert!(
                output.len() <= super::HOOK_PIPE_CAPTURE_MAX_BYTES + "…[truncated]".len(),
                "{} bytes",
                output.len()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn helper_wait_and_uncontained_reap_paths_are_bounded() {
        // These helpers reap one immediate child. A shell can fork `sleep`
        // and leave that descendant holding the test's output pipes.
        let mut helper = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn helper");
        let started = Instant::now();
        let error = super::wait_for_helper_status(&mut helper, Duration::from_millis(20))
            .expect_err("slow helper must time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < super::HOOK_REAP_TIMEOUT + Duration::from_secs(1));
        assert!(matches!(helper.try_wait(), Ok(Some(_))));

        let mut uncontained = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn uncontained child");
        assert!(super::kill_and_reap_immediate_child(
            &mut uncontained,
            Duration::from_secs(1)
        ));
        assert!(matches!(uncontained.try_wait(), Ok(Some(_))));
    }

    #[test]
    fn sanitize_hook_text_keeps_short_text_verbatim() {
        assert_eq!(
            super::sanitize_hook_text("plain reason", 100),
            "plain reason"
        );
        assert_eq!(super::sanitize_hook_text("a\tb\nc", 100), "a\tb\nc");
        assert_eq!(super::sanitize_hook_text("", 100), "");
    }

    #[test]
    fn sanitize_hook_line_flattens_structure_characters() {
        assert_eq!(super::sanitize_hook_line("a\tb\nc", 100), "a b c");
        assert_eq!(super::sanitize_hook_line("a\u{1b}[2Jb\r", 100), "a [2Jb");
    }

    #[test]
    fn sanitize_hook_label_bounds_and_defangs_operator_names() {
        let noisy = format!("\u{1b}[2Jgate\twith\nnoise{}", "x".repeat(1_000));
        let label = super::sanitize_hook_label(Some(&noisy));
        assert!(!label.contains('\u{1b}'), "{label}");
        assert!(!label.contains('\n') && !label.contains('\t'), "{label}");
        assert!(label.contains("gate"), "{label}");
        assert!(
            label.chars().count() <= super::HOOK_LABEL_MAX_CHARS + 16,
            "{} chars",
            label.chars().count()
        );

        assert_eq!(super::sanitize_hook_label(None), "(unnamed)");
        assert_eq!(super::sanitize_hook_label(Some("")), "(unnamed)");
        assert_eq!(super::sanitize_hook_label(Some(" \t ")), "(unnamed)");
        assert_eq!(super::sanitize_hook_label(Some(" gate ")), "gate");
    }

    /// The point of the boundary: recognized failures are re-rendered from
    /// parts, and anything else — including a string a future producer forgot
    /// to genericize — collapses instead of passing through.
    #[test]
    fn generic_unavailable_detail_is_an_allowlist_not_a_passthrough() {
        use super::generic_unavailable_detail as detail;

        assert_eq!(
            detail(Some("Hook timed out after 30s")),
            "hook timed out after 30s"
        );
        assert_eq!(
            detail(Some("hook process could not be started (NotFound)")),
            "hook process could not be started (NotFound)"
        );
        assert_eq!(
            detail(Some("Failed to wait for hook: os error 10")),
            "hook did not complete cleanly"
        );
        assert_eq!(
            detail(Some("hook could not be reaped after its timeout")),
            "hook did not complete cleanly"
        );
        assert_eq!(
            detail(Some("Failed to submit background hook: os error 11")),
            "hook could not be submitted"
        );
        assert_eq!(
            detail(Some("hook executor did not run")),
            "hook executor did not run"
        );
        assert_eq!(detail(None), "hook returned no verdict");

        // A hypothetical future producer that leaks.
        let leaky = "spawn failed: /Users/someone/.aws/credentials --token=SECRET";
        let rendered = detail(Some(leaky));
        assert_eq!(rendered, "hook returned no verdict");
        assert!(!rendered.contains("SECRET"));
        assert!(!rendered.contains('/'));

        // And a recognized prefix cannot be used to smuggle a tail along.
        let smuggled = detail(Some(
            "Hook timed out after 30s while running /usr/bin/leak --token=SECRET",
        ));
        assert_eq!(smuggled, "hook timed out after 30s");
        let smuggled = detail(Some(
            "hook process could not be started (NotFound) /usr/bin/leak",
        ));
        assert_eq!(smuggled, "hook process could not be started (NotFound)");
    }

    /// The gate set the caller has to fail closed on if the executor is lost.
    #[cfg(unix)]
    #[test]
    fn matched_strict_gate_labels_names_only_gates_that_would_run() {
        use crate::hooks::{Hook, HookCondition, HookEvent, HooksConfig};

        let strict_shell = {
            let mut hook = Hook::new(HookEvent::ToolCallBefore, "true")
                .with_name("shell-gate")
                .with_condition(HookCondition::ToolName {
                    name: "exec_shell".into(),
                });
            hook.continue_on_error = false;
            hook
        };
        let strict_write = {
            let mut hook = Hook::new(HookEvent::ToolCallBefore, "true")
                .with_name("write-gate")
                .with_condition(HookCondition::ToolName {
                    name: "write_file".into(),
                });
            hook.continue_on_error = false;
            hook
        };
        let lenient_shell = Hook::new(HookEvent::ToolCallBefore, "true").with_name("lenient");
        let background_strict = {
            let mut hook = Hook::new(HookEvent::ToolCallBefore, "true").with_name("bg-gate");
            hook.continue_on_error = false;
            hook.background = true;
            hook
        };
        let other_event = {
            let mut hook = Hook::new(HookEvent::ToolCallAfter, "true").with_name("after-gate");
            hook.continue_on_error = false;
            hook
        };

        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    strict_shell,
                    strict_write,
                    lenient_shell,
                    background_strict,
                    other_event,
                ],
                ..HooksConfig::default()
            },
            std::env::temp_dir(),
        );

        let labels = executor.matched_strict_gate_labels(
            HookEvent::ToolCallBefore,
            &HookContext::new().with_tool_name("exec_shell"),
        );
        assert_eq!(labels, vec!["shell-gate".to_string()], "{labels:?}");

        // Globally disabled hooks are not gates either.
        let disabled = HookExecutor::disabled();
        assert!(
            disabled
                .matched_strict_gate_labels(
                    HookEvent::ToolCallBefore,
                    &HookContext::new().with_tool_name("exec_shell"),
                )
                .is_empty()
        );
    }

    /// The reap after a kill is bounded. This asserts the ordinary case is
    /// still confirmed dead and, more importantly, that the call returns —
    /// the regression it guards is a hang, not a wrong value.
    #[cfg(unix)]
    #[test]
    fn timed_out_hook_is_killed_and_reaped_within_the_bound() {
        use crate::hooks::{Hook, HookEvent, HooksConfig};

        let hook = Hook::new(HookEvent::SessionStart, "sleep 30")
            .with_name("slow")
            .with_timeout(1);
        let executor = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![hook],
                ..HooksConfig::default()
            },
            std::env::temp_dir(),
        );

        let started = Instant::now();
        let results = executor.execute(HookEvent::SessionStart, &HookContext::new());
        let elapsed = started.elapsed();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].error.as_deref(),
            Some("Hook timed out after 1s"),
            "the child was reaped, so the stronger claim is the honest one"
        );
        assert!(
            elapsed < Duration::from_secs(1) + super::HOOK_REAP_TIMEOUT + Duration::from_secs(5),
            "timeout path took {elapsed:?}"
        );
    }

    #[test]
    fn tool_exit_code_env_var_survives_a_windows_crash_code() {
        // 0xC0000005 (access violation) does not fit in an `i32`. It used to
        // be dropped on the floor before the hook ever saw it.
        let env = HookContext::new()
            .with_tool_result("crashed", false, Some(3_221_225_477))
            .to_env_vars();
        assert_eq!(
            env.get("DEEPSEEK_TOOL_EXIT_CODE"),
            Some(&"3221225477".to_string())
        );

        let executor = HookExecutor::new(HooksConfig::default(), PathBuf::from("."));
        let hook =
            Hook::new(HookEvent::ToolCallAfter, "true").with_condition(HookCondition::ExitCode {
                code: 3_221_225_477,
            });
        let context = HookContext::new()
            .with_tool_name("exec_shell")
            .with_tool_result("crashed", false, Some(3_221_225_477));
        assert!(executor.matches_condition(&hook, &context));
    }

    /// 2026-08-04: the category map knew only retired tool names, so every
    /// live call fell through to `other` and a `tool_category` deny hook —
    /// the security control `docs/HOOKS.md` documents — silently never fired.
    #[test]
    fn tool_category_classifies_the_names_the_registry_actually_registers() {
        use super::tool_category_for;

        // Anchor to the real catalog. Everything below this pins hardcoded
        // names, which would stay green through a tool rename while the gate
        // quietly reclassified the renamed tool. `DEFAULT_ACTIVE_NATIVE_TOOLS`
        // is the list the engine actually puts on the wire, so if a name here
        // stops being a name the product ships, this fails first.
        //
        // Note the fallback is "other", not "safe" — asserting against "safe"
        // here would never fire. This table is checked in both directions, so
        // a rename fails on the missing entry and a classifier change fails on
        // the mismatched category.
        // Token diet: agent+workflow and get/update_goal are deferred by
        // default (discoverable via tool_search), so they left the eager
        // table; their classifications are pinned with the other
        // deferred tools below instead.
        const EXPECTED: &[(&str, &str)] = &[
            ("read", "safe"),
            ("write", "file_write"),
            ("edit", "file_write"),
            // Searches the workspace without caller-named write targets, so it
            // keeps the classification it already had as a deferred tool.
            ("file_search", "other"),
            ("bash", "shell"),
            ("todo_write", "safe"),
            ("create_goal", "other"),
            ("task_shell_wait", "other"),
            // Reads the skill registry, not caller-named paths, so it keeps
            // the classification it already had as a deferred tool. Making it
            // eager must not silently re-gate it.
            ("load_skill", "other"),
        ];
        for name in crate::core::engine::tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS {
            let expected = EXPECTED.iter().find(|(n, _)| n == name).map(|(_, c)| *c);
            assert_eq!(
                Some(tool_category_for(name, None)),
                expected,
                "default-active tool {name:?} is not covered by this test's \
                 table. It was renamed or added without updating the hook \
                 gate's classifier, so the gate now sees a shipped tool it \
                 does not recognise."
            );
        }
        for (name, _) in EXPECTED {
            assert!(
                crate::core::engine::tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS.contains(name),
                "{name:?} is pinned here but is no longer default-active; drop \
                 it so this table keeps describing what actually ships."
            );
        }

        // Deferred by default, still classified: the router touches
        // nothing a hook needs to gate, and the goal controls keep
        // their classification when promoted to eager.
        assert_eq!(tool_category_for("agent", None), "other");
        assert_eq!(tool_category_for("workflow", None), "other");
        assert_eq!(tool_category_for("get_goal", None), "other");
        assert_eq!(tool_category_for("update_goal", None), "other");

        // The shell surface.
        assert_eq!(tool_category_for("Bash", None), "shell");
        // Retained: shell.rs stamps this for the shell_env event.
        assert_eq!(tool_category_for("exec_shell", None), "shell");
        // Run executes commands, so it gates with shell rather than safe.
        assert_eq!(tool_category_for("Run", None), "shell");

        // File is multi-action: the action decides.
        let read = r#"{"action":"read","path":"a.rs"}"#;
        let write = r#"{"action":"write","path":"a.rs","content":"x"}"#;
        assert_eq!(tool_category_for("File", Some(read)), "safe");
        assert_eq!(tool_category_for("File", Some(write)), "file_write");
        assert_eq!(
            tool_category_for("File", Some(r#"{"action":"edit"}"#)),
            "file_write"
        );
        assert_eq!(
            tool_category_for("File", Some(r#"{"action":"search_content"}"#)),
            "safe"
        );

        // A gate that cannot see the action must assume the dangerous one.
        assert_eq!(tool_category_for("File", None), "file_write");
        assert_eq!(tool_category_for("File", Some("not json")), "file_write");

        assert_eq!(tool_category_for("apply_patch", None), "file_write");
        assert_eq!(
            tool_category_for("Git", Some(r#"{"action":"log"}"#)),
            "safe"
        );
        assert_eq!(
            tool_category_for("Git", Some(r#"{"action":"commit_plan"}"#)),
            "safe"
        );
        assert_eq!(tool_category_for("web.run", None), "other");
    }
}

pub(crate) fn shell_env_keys(stdout: &str) -> Vec<String> {
    parse_env_lines(stdout).into_keys().collect()
}

#[cfg(test)]
mod admitted_hook_tests {
    use super::*;
    #[test]
    fn native_payload_uses_actual_caller_and_bounded_canonical_input() {
        let caller = HookCaller {
            workspace: PathBuf::from("/actual"),
            plugins: None,
            session_id: Some("actual-session".into()),
            agent_id: Some("child".into()),
            origin_turn_id: Some("turn".into()),
            origin_call_id: Some("call".into()),
        };
        let context = HookContext::new()
            .with_session_id("legacy-alias")
            .with_caller(caller)
            .with_tool_name("write")
            .with_tool_call_id("call")
            .with_tool_args(&json!({"text":"你好"}));
        let payload = context.native_payload("PreToolUse", None).unwrap();
        assert_eq!(payload["session_id"], "actual-session");
        assert_eq!(payload["cwd"], "/actual");
        assert_eq!(payload["tool_input"], json!({"text":"你好"}));
        let oversized = context.with_tool_args(&json!({"text":"x".repeat(32769)}));
        assert!(oversized.native_payload("PreToolUse", None).is_err());
    }
    #[test]
    fn native_submit_block_preserves_real_process_exit_semantics() {
        assert_eq!(
            parse_message_submit_stdout(r#"{"block":true,"reason":"stop"}"#),
            MessageSubmitStdout::Blocked("stop".into())
        );
        assert_eq!(
            parse_message_submit_stdout(r#"{"text":"new"}"#),
            MessageSubmitStdout::Replaced("new".into())
        );
    }
    #[cfg(unix)]
    #[test]
    fn admitted_std_driver_cancels_and_reaps_before_late_side_effect() {
        let home = tempfile::tempdir().unwrap();
        let marker = home.path().join("late");
        let executor = HookExecutor::new(HooksConfig::default(), home.path().to_path_buf());
        let hook = Hook::new(
            HookEvent::SessionStart,
            &format!("sleep 2; touch '{}'", marker.display()),
        )
        .with_timeout(5);
        let token = tokio_util::sync::CancellationToken::new();
        let to_cancel = token.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            to_cancel.cancel();
        });
        let started = Instant::now();
        let result = executor.execute_sync_legacy(&hook, &HashMap::new(), None, Some(&token));
        worker.join().unwrap();
        assert!(!result.success);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!marker.exists());
    }
}

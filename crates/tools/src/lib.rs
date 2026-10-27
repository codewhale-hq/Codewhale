use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use codewhale_protocol::{ToolKind, ToolOutput, ToolPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

pub mod names;
mod outcome;
pub mod param_alias;
pub mod patch_preflight;
mod prepared;
mod resources;

pub use outcome::{ToolExecutionOutcome, ToolTerminalStatus};
pub use prepared::PreparedToolCall;
pub use resources::{ResourceClaim, schedule_non_conflicting};

tokio::task_local! {
    static TOOL_EXECUTION_LOCK_HELD: ();
}

/// Capabilities that a tool may have or require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCapability {
    /// Tool only reads data, never modifies state.
    ReadOnly,
    /// Tool writes to the filesystem.
    WritesFiles,
    /// Tool executes arbitrary shell commands.
    ExecutesCode,
    /// Tool makes network requests.
    Network,
    /// Tool can be run in a sandbox.
    Sandboxable,
    /// Tool requires user approval before execution.
    RequiresApproval,
}

/// Approval requirement for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalRequirement {
    /// Never needs approval: safe read-only operations.
    #[default]
    Auto,
    /// Suggest approval but allow user to skip.
    Suggest,
    /// Always require explicit user approval.
    Required,
}

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    #[error("Failed to validate input: {message}")]
    InvalidInput { message: String },
    #[error("Failed to validate input: missing required field '{field}'")]
    MissingField { field: String },
    #[error("Failed to resolve path '{}': path escapes workspace", path.display())]
    PathEscape { path: PathBuf },
    #[error("Failed to execute tool: {message}")]
    ExecutionFailed {
        message: String,
        /// Structured facts about the failure, in the same shape a
        /// `ToolResult` would carry them. A process-backed tool that reports a
        /// nonzero exit or a timeout as an error puts its `exit_code` and
        /// `status` here, so observers (hooks, receipts) still see them.
        metadata: Option<Value>,
    },
    #[error("Failed to execute tool: operation timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("Tool execution cancelled: {message}")]
    Cancelled { message: String },
    #[error("Failed to locate tool: {message}")]
    NotAvailable { message: String },
    #[error("Failed to authorize tool execution: {message}")]
    PermissionDenied { message: String },
}

impl ToolError {
    #[must_use]
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn missing_field(field: impl Into<String>) -> Self {
        Self::MissingField {
            field: field.into(),
        }
    }

    #[must_use]
    pub fn execution_failed(msg: impl Into<String>) -> Self {
        Self::ExecutionFailed {
            message: msg.into(),
            metadata: None,
        }
    }

    /// An execution failure that still carries structured result metadata,
    /// such as a shell command's `exit_code` and `status`.
    #[must_use]
    pub fn execution_failed_with_metadata(msg: impl Into<String>, metadata: Value) -> Self {
        Self::ExecutionFailed {
            message: msg.into(),
            metadata: Some(metadata),
        }
    }

    /// Structured metadata the failing tool attached, if any.
    #[must_use]
    pub fn metadata(&self) -> Option<&Value> {
        match self {
            Self::ExecutionFailed { metadata, .. } => metadata.as_ref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn cancelled(msg: impl Into<String>) -> Self {
        Self::Cancelled {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn path_escape(path: impl Into<PathBuf>) -> Self {
        Self::PathEscape { path: path.into() }
    }

    #[must_use]
    pub fn not_available(msg: impl Into<String>) -> Self {
        Self::NotAvailable {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::PermissionDenied {
            message: msg.into(),
        }
    }
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The output content, which may be JSON or plain text.
    pub content: String,
    /// Whether the execution was successful.
    pub success: bool,
    /// Optional structured metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Provider-neutral non-text content returned alongside a tool result.
/// Image-producing tools return owned base64 bytes here (MCP uses its standard
/// `content` image blocks). A path in `ToolResult.metadata` is descriptive
/// metadata, never permission for the engine to read another host file.
/// The runtime validates format, full decode and size, retains one image per
/// result, and keeps omitted-image receipts with the text result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    Image { mime_type: String, data: String },
}

impl ToolResult {
    /// Create a successful result with content.
    #[must_use]
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            success: true,
            metadata: None,
        }
    }

    /// Create an error result with message.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            success: false,
            metadata: None,
        }
    }

    /// Create a successful result from JSON.
    pub fn json<T: Serialize>(value: &T) -> std::result::Result<Self, serde_json::Error> {
        Ok(Self {
            content: serde_json::to_string(value)?,
            success: true,
            metadata: None,
        })
    }

    /// Add metadata to the result.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Name the JSON type of a value the way a tool schema would spell it.
#[must_use]
pub fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Render a value for an error message, truncated so a huge payload cannot
/// swamp the transcript.
#[must_use]
pub fn value_preview(value: &Value) -> String {
    let preview = value.to_string();
    if preview.chars().count() > 120 {
        preview.chars().take(117).collect::<String>() + "..."
    } else {
        preview
    }
}

/// The one error every type mismatch on a tool parameter produces.
///
/// Names the parameter, the type that arrived, and the type the schema
/// declares, plus the offending value — everything the caller needs to fix
/// the call on the next turn without another round trip.
#[must_use]
pub fn type_mismatch(field: &str, value: &Value, expected: &str) -> ToolError {
    ToolError::invalid_input(format!(
        "field '{field}' must be {expected}; got {}. Received: {}",
        json_type_name(value),
        value_preview(value)
    ))
}

/// Whether a value counts as "the caller did not supply this field".
///
/// JSON `null` is the wire spelling of absence, so an optional field set to
/// `null` takes its default rather than erroring. This is the *only*
/// tolerance in the optional extractors, and it is uniform across all of
/// them: `null` means no value, and no value is exactly what a default is
/// for. Every other type mismatch is an error.
fn is_absent(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null))
}

/// Helper to extract a required string field from JSON input.
pub fn required_str<'a>(input: &'a Value, field: &str) -> std::result::Result<&'a str, ToolError> {
    if let Some(value) = input.get(field) {
        if let Some(string_value) = value.as_str() {
            return Ok(string_value);
        }

        return Err(type_mismatch(field, value, "a string"));
    }

    // When the field is missing, list the fields the caller *did*
    // supply so the model can spot the mismatch without a retry.
    let provided: Vec<&str> = input
        .as_object()
        .map(|obj| obj.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
    if provided.is_empty() {
        Err(ToolError::missing_field(field))
    } else {
        let hint = format!(
            "missing required field '{field}'. Input provided: {}",
            provided.join(", ")
        );
        Err(ToolError::invalid_input(hint))
    }
}

/// Require the `action` discriminator on a canonical action-family call.
///
/// Every family schema marks `action` required, but the wrappers used to
/// default a missing one (`File` → read, `Git` → status, `Web` → search,
/// `Run` → tests). A call that merely omitted or misspelled the discriminator
/// therefore ran a *different* operation and returned that operation's success
/// receipt: `File{path, content}` answered an intended write with the file's
/// current contents, so the write silently never happened. Same shape as
/// #5209 — refuse, and name the values that actually dispatch.
///
/// `actions` must be the set this tool instance can really run, so a mode that
/// hides `write` never suggests it.
pub fn required_action(
    input: &Value,
    tool: &str,
    actions: &[&str],
) -> std::result::Result<String, ToolError> {
    match input.get("action") {
        Some(Value::String(action)) => Ok(action.clone()),
        Some(other) => Err(ToolError::invalid_input(format!(
            "{tool} requires `action` to be a string, got {other}; nothing was run. Pass one of: {}.",
            actions.join(", ")
        ))),
        None => Err(ToolError::invalid_input(format!(
            "{tool} requires an `action` parameter; nothing was run. Pass one of: {}.",
            actions.join(", ")
        ))),
    }
}

/// Helper to extract an optional string field from JSON input.
///
/// A wrong type is an error, never a silent `None`. See [`type_mismatch`]
/// for why nothing is coerced.
pub fn optional_str<'a>(
    input: &'a Value,
    field: &str,
) -> std::result::Result<Option<&'a str>, ToolError> {
    let value = input.get(field);
    if is_absent(value) {
        return Ok(None);
    }
    let value = value.expect("is_absent covers the None case");
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| type_mismatch(field, value, "a string"))
}

/// Read a JSON number as a non-negative integer, accepting whole-number
/// floats such as `200.0`.
///
/// Several providers serialize every JSON number as a float, so a ranged
/// `read` arrives as `{"offset": 200.0}`. Exact integers keep the `as_u64`
/// fast path (so `u64::MAX` stays exact); a float is accepted only when it
/// is finite, non-negative, has no fractional part and fits in `u64`.
/// Negative, fractional, string and array values are still refused.
#[must_use]
pub fn json_nonnegative_integer(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let float = value.as_f64()?;
    // 2^64 is exactly representable as f64; anything at or above it overflows.
    const U64_LIMIT: f64 = 18_446_744_073_709_551_616.0;
    if float.is_finite() && float >= 0.0 && float.fract() == 0.0 && float < U64_LIMIT {
        // The guards above make this cast exact.
        return Some(float as u64);
    }
    None
}

/// Helper to extract a required u64 field from JSON input.
///
/// Absence (field missing or `null`) is a `missing_field` error; a value
/// that is present but not a u64 is a [`type_mismatch`] naming the field and
/// the expected type, so the caller fixes the field's type instead of
/// re-sending it as missing.
pub fn required_u64(input: &Value, field: &str) -> std::result::Result<u64, ToolError> {
    let value = input.get(field);
    if is_absent(value) {
        return Err(ToolError::missing_field(field));
    }
    let value = value.expect("is_absent covers the None case");
    json_nonnegative_integer(value)
        .ok_or_else(|| type_mismatch(field, value, "a non-negative integer"))
}

/// Helper to extract an optional u64 field with default.
///
/// A wrong type is an error, never a silent fall back to `default`.
pub fn optional_u64(
    input: &Value,
    field: &str,
    default: u64,
) -> std::result::Result<u64, ToolError> {
    let value = input.get(field);
    if is_absent(value) {
        return Ok(default);
    }
    let value = value.expect("is_absent covers the None case");
    json_nonnegative_integer(value)
        .ok_or_else(|| type_mismatch(field, value, "a non-negative integer"))
}

/// Helper to extract an optional bool field with default.
///
/// A wrong type is an error, never a silent fall back to `default`. In
/// particular the string `"true"` is refused rather than coerced: the
/// default this used to fall back to is frequently the *opposite* of what
/// the caller asked for, and some of those defaults gate irreversible
/// actions.
pub fn optional_bool(
    input: &Value,
    field: &str,
    default: bool,
) -> std::result::Result<bool, ToolError> {
    Ok(optional_bool_opt(input, field)?.unwrap_or(default))
}

/// Helper to extract an optional bool that has no default.
///
/// `None` means the caller did not supply the field; a wrong type is an
/// error. Use this where "unset" is itself meaningful — an authority
/// declaration that is dropped instead of read is a restriction that
/// silently evaporates.
pub fn optional_bool_opt(
    input: &Value,
    field: &str,
) -> std::result::Result<Option<bool>, ToolError> {
    let value = input.get(field);
    if is_absent(value) {
        return Ok(None);
    }
    let value = value.expect("is_absent covers the None case");
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| type_mismatch(field, value, "a boolean"))
}

/// Descriptor that describes a tool available in the registry.
///
/// Contains the tool's name, its JSON input/output schemas, and
/// execution constraints such as timeout and parallelism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Unique name used to look up the tool.
    pub name: String,
    /// JSON Schema describing the tool's expected input parameters.
    pub input_schema: Value,
    /// JSON Schema describing the tool's output format.
    pub output_schema: Value,
    /// Whether multiple invocations of this tool may run concurrently.
    pub supports_parallel_tool_calls: bool,
    /// Optional per-call timeout in milliseconds; `None` means no timeout.
    pub timeout_ms: Option<u64>,
}

/// A [`ToolDescriptor`] together with its runtime configuration.
///
/// Wraps a `ToolDescriptor` and exposes the parallelism flag directly so the
/// dispatcher can check it without digging into the inner spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfiguredToolDescriptor {
    /// The underlying tool descriptor.
    pub spec: ToolDescriptor,
    /// Whether this tool supports concurrent invocations.
    pub supports_parallel_tool_calls: bool,
}

/// Identifies where a tool call originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSource {
    /// Direct invocation from the model or user.
    Direct,
    /// Invocation through the JavaScript REPL environment.
    JsRepl,
}

/// A tool invocation request before it has been validated and dispatched.
///
/// Contains the tool name, its input payload, and metadata about where the
/// call originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Name of the tool to invoke.
    pub name: String,
    /// The input payload for the tool.
    pub payload: ToolPayload,
    /// Where this call originated (direct or REPL).
    pub source: ToolCallSource,
    /// Optional raw tool-call identifier from the upstream provider.
    pub raw_tool_call_id: Option<String>,
}

impl ToolCall {
    /// Derive the execution subject for this call.
    ///
    /// For local shell payloads this returns the shell command and its
    /// working directory; for all other payloads the tool name and the
    /// provided `fallback_cwd` are returned instead. The third element
    /// of the tuple is a human-readable kind label (`"shell"` or `"tool"`).
    pub fn execution_subject(&self, fallback_cwd: &str) -> (String, String, &'static str) {
        match &self.payload {
            ToolPayload::LocalShell { params } => (
                params.command.clone(),
                params
                    .cwd
                    .clone()
                    .unwrap_or_else(|| fallback_cwd.to_string()),
                "shell",
            ),
            _ => (self.name.clone(), fallback_cwd.to_string(), "tool"),
        }
    }
}

/// A validated tool invocation ready to be handled.
///
/// Created by the registry after a [`ToolCall`] passes validation, this
/// carries all the context a [`ToolHandler`] needs to execute the tool.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    /// Unique identifier for this invocation (generated or from the provider).
    pub call_id: String,
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// The input payload for the tool.
    pub payload: ToolPayload,
    /// Where this invocation originated.
    pub source: ToolCallSource,
}

/// Errors that can occur during tool dispatch and execution.
///
/// Unlike [`ToolError`], which represents input validation failures within
/// a tool, `FunctionCallError` covers problems at the dispatch layer: the
/// tool was not found, its kind did not match, it was rejected because it
/// is mutating, it timed out, was cancelled, or its handler returned an
/// error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunctionCallError {
    /// No tool with the given name is registered.
    ToolNotFound { name: String },
    /// The payload kind does not match the handler's expected kind.
    KindMismatch { expected: ToolKind, got: ToolKind },
    /// The tool is mutating but `allow_mutating` was `false`.
    MutatingToolRejected { name: String },
    /// The tool execution exceeded its configured timeout.
    TimedOut { name: String, timeout_ms: u64 },
    /// The tool execution was cancelled.
    Cancelled { name: String },
    /// The tool handler returned an error.
    ExecutionFailed { name: String, error: String },
}

/// Trait implemented by concrete tool handlers.
///
/// Each registered tool is backed by a handler that reports its kind,
/// whether it is mutating, and performs the actual execution.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// The [`ToolKind`] this handler expects (e.g. `Function` or `Mcp`).
    fn kind(&self) -> ToolKind;

    /// Returns `true` if `kind` matches this handler's expected kind.
    ///
    /// The default implementation compares against [`kind()`](ToolHandler::kind).
    fn matches_kind(&self, kind: ToolKind) -> bool {
        self.kind() == kind
    }

    /// Whether this tool performs side-effects that require user approval.
    ///
    /// Defaults to `false` (read-only / safe).
    fn is_mutating(&self) -> bool {
        false
    }

    /// Execute the tool with the given invocation context.
    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, FunctionCallError>;
}

/// Manages concurrent tool execution via a read/write lock.
///
/// Parallel-safe tools acquire a read lock (allowing overlap), while
/// serial tools acquire a write lock (exclusive access). Reentrant calls
/// (e.g. a tool invoking another tool) skip locking to avoid deadlock.
#[derive(Debug)]
pub struct ToolCallRuntime {
    execution_lock: Arc<RwLock<()>>,
}

impl Default for ToolCallRuntime {
    fn default() -> Self {
        Self {
            execution_lock: Arc::new(RwLock::new(())),
        }
    }
}

#[derive(Debug)]
enum ToolExecutionGuard {
    Parallel(#[allow(dead_code)] OwnedRwLockReadGuard<()>),
    Serial(#[allow(dead_code)] OwnedRwLockWriteGuard<()>),
    Reentrant,
}

impl ToolCallRuntime {
    async fn acquire(&self, supports_parallel: bool) -> ToolExecutionGuard {
        if TOOL_EXECUTION_LOCK_HELD.try_with(|_| ()).is_ok() {
            return ToolExecutionGuard::Reentrant;
        }

        if supports_parallel {
            ToolExecutionGuard::Parallel(self.execution_lock.clone().read_owned().await)
        } else {
            ToolExecutionGuard::Serial(self.execution_lock.clone().write_owned().await)
        }
    }
}

/// Central registry that maps tool names to their specs and handlers.
///
/// Use [`register()`](ToolRegistry::register) to add tools, then
/// [`dispatch()`](ToolRegistry::dispatch) to invoke them. The registry
/// owns a [`ToolCallRuntime`] that manages concurrent execution.
#[derive(Default)]
pub struct ToolRegistry {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: HashMap<String, ConfiguredToolDescriptor>,
    runtime: ToolCallRuntime,
}

impl ToolRegistry {
    /// Register a tool with its specification and handler.
    ///
    /// The tool's name is taken from `spec.name`. Returns an error if
    /// registration fails (currently infallible, but the `Result` is
    /// reserved for future validation).
    pub fn register(&mut self, spec: ToolDescriptor, handler: Arc<dyn ToolHandler>) -> Result<()> {
        let name = spec.name.clone();
        self.specs.insert(
            name.clone(),
            ConfiguredToolDescriptor {
                supports_parallel_tool_calls: spec.supports_parallel_tool_calls,
                spec,
            },
        );
        self.handlers.insert(name, handler);
        Ok(())
    }

    /// Validate and execute a tool call.
    ///
    /// Looks up the tool by name, verifies the payload kind matches the
    /// handler, enforces the `allow_mutating` guard, acquires the
    /// appropriate execution lock, and forwards the call to the handler.
    /// Returns a [`FunctionCallError`] if any validation step fails or
    /// the handler returns an error.
    pub async fn dispatch(
        &self,
        call: ToolCall,
        allow_mutating: bool,
    ) -> std::result::Result<ToolOutput, FunctionCallError> {
        let handler = self.handlers.get(&call.name).cloned().ok_or_else(|| {
            FunctionCallError::ToolNotFound {
                name: call.name.clone(),
            }
        })?;
        let configured =
            self.specs
                .get(&call.name)
                .cloned()
                .ok_or_else(|| FunctionCallError::ToolNotFound {
                    name: call.name.clone(),
                })?;

        let payload_kind = tool_payload_kind(&call.payload);
        let expected = handler.kind();
        if !handler.matches_kind(payload_kind) {
            return Err(FunctionCallError::KindMismatch {
                expected,
                got: payload_kind,
            });
        }
        if handler.is_mutating() && !allow_mutating {
            return Err(FunctionCallError::MutatingToolRejected { name: call.name });
        }

        let invocation = ToolInvocation {
            call_id: call
                .raw_tool_call_id
                .clone()
                .unwrap_or_else(|| format!("tool-call-{}", uuid::Uuid::new_v4())),
            tool_name: call.name.clone(),
            payload: call.payload,
            source: call.source,
        };

        let _guard = self
            .runtime
            .acquire(configured.supports_parallel_tool_calls)
            .await;

        TOOL_EXECUTION_LOCK_HELD
            .scope(
                (),
                self.execute_with_timeout(handler, configured.spec.timeout_ms, invocation),
            )
            .await
    }

    async fn execute_with_timeout(
        &self,
        handler: Arc<dyn ToolHandler>,
        timeout_ms: Option<u64>,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, FunctionCallError> {
        if let Some(timeout_ms) = timeout_ms {
            let name = invocation.tool_name.clone();
            match tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                handler.handle(invocation),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(FunctionCallError::TimedOut { name, timeout_ms }),
            }
        } else {
            handler.handle(invocation).await
        }
    }
}

fn tool_payload_kind(payload: &ToolPayload) -> ToolKind {
    match payload {
        ToolPayload::Mcp { .. } => ToolKind::Mcp,
        ToolPayload::Function { .. }
        | ToolPayload::Custom { .. }
        | ToolPayload::LocalShell { .. } => ToolKind::Function,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_result_success_sets_plain_content() {
        let content = "operation completed successfully";
        let result = ToolResult::success(content);

        assert!(result.success);
        assert_eq!(result.content, content);
        assert!(result.metadata.is_none());
    }

    #[test]
    fn tool_result_json_round_trips_content() {
        let result = ToolResult::json(&json!({"ok": true})).expect("json");
        assert!(result.success);
        let content: serde_json::Value =
            serde_json::from_str(&result.content).expect("content is valid json");
        assert_eq!(content, json!({"ok": true}));
    }

    #[test]
    fn helper_extractors_validate_shape() {
        let input = json!({"name": "demo", "count": 7, "enabled": true});
        assert_eq!(required_str(&input, "name").expect("name"), "demo");
        assert_eq!(optional_str(&input, "name").unwrap(), Some("demo"));
        assert_eq!(optional_str(&input, "missing").unwrap(), None);
        assert_eq!(optional_str(&json!({"name": null}), "name").unwrap(), None);
        assert_eq!(optional_u64(&input, "count", 0).unwrap(), 7);
        assert!(optional_bool(&input, "enabled", false).unwrap());
        // "name" is present but a string: a type mismatch, not a missing
        // field, so the caller fixes the type instead of re-sending the name.
        let err = required_u64(&input, "name")
            .expect_err("a present string is not a missing u64")
            .to_string();
        assert!(
            err.contains("field 'name' must be a non-negative integer"),
            "{err}"
        );
    }

    /// The rule, stated once: an optional parameter of the wrong JSON type is
    /// an error that names the parameter, what arrived, and what was wanted.
    /// `null` alone means "absent" and takes the default.
    #[test]
    fn optional_extractors_refuse_type_mismatches_instead_of_defaulting() {
        // The shipping bug: a stringy "true" became the default `false`,
        // which for `dry_run` is the opposite of what the caller asked and
        // gates an irreversible action.
        let err = optional_bool(&json!({"dry_run": "true"}), "dry_run", false)
            .expect_err("a stringy bool must not become the default")
            .to_string();
        assert!(err.contains("dry_run"), "{err}");
        assert!(err.contains("must be a boolean"), "{err}");
        assert!(err.contains("got string"), "{err}");
        assert!(err.contains("\"true\""), "{err}");

        for bad in [json!("true"), json!(1), json!(0), json!([]), json!({})] {
            assert!(
                optional_bool(&json!({"flag": bad}), "flag", false).is_err(),
                "optional_bool accepted {bad}"
            );
        }
        for bad in [json!("7"), json!(-1), json!(1.5), json!(true), json!([7])] {
            assert!(
                optional_u64(&json!({"n": bad}), "n", 42).is_err(),
                "optional_u64 accepted {bad}"
            );
        }
        for bad in [json!(7), json!(true), json!(["a"]), json!({"a": 1})] {
            assert!(
                optional_str(&json!({"s": bad}), "s").is_err(),
                "optional_str accepted {bad}"
            );
        }

        // `null` is the wire spelling of absence, uniformly across all three.
        assert!(optional_bool(&json!({"flag": null}), "flag", true).unwrap());
        assert_eq!(optional_u64(&json!({"n": null}), "n", 42).unwrap(), 42);
        assert_eq!(optional_str(&json!({"s": null}), "s").unwrap(), None);
    }

    #[test]
    fn type_mismatch_truncates_a_huge_offending_value() {
        let big = Value::String("x".repeat(500));
        let err = type_mismatch("body", &big, "a boolean").to_string();
        assert!(err.contains("body"), "{err}");
        assert!(err.ends_with("..."), "{err}");
        assert!(err.chars().count() < 250, "{err}");
    }

    #[test]
    fn required_u64_distinguishes_missing_from_type_mismatch() {
        // Absent (or null) is a missing-field error.
        assert!(matches!(
            required_u64(&json!({}), "count"),
            Err(ToolError::MissingField { .. })
        ));
        assert!(matches!(
            required_u64(&json!({"count": null}), "count"),
            Err(ToolError::MissingField { .. })
        ));

        // Present and valid values pass through, including the extremes.
        assert_eq!(required_u64(&json!({"count": 42}), "count").unwrap(), 42);
        assert_eq!(
            required_u64(&json!({"count": u64::MAX}), "count").unwrap(),
            u64::MAX
        );

        // Whole-number floats are integers some providers send as `200.0`.
        assert_eq!(
            required_u64(&json!({"count": 200.0}), "count").unwrap(),
            200
        );
        assert_eq!(required_u64(&json!({"count": 0.0}), "count").unwrap(), 0);
        assert_eq!(
            optional_u64(&json!({"count": 200.0}), "count", 7).unwrap(),
            200
        );

        // Present but wrongly typed is a type mismatch naming the field and
        // the expected type — never a missing-field misdirection. Floats that
        // are negative, fractional or beyond u64 stay refused.
        for value in [
            json!(-1),
            json!(-1.0),
            json!(2.5),
            json!(1.8446744073709552e19),
            json!("42"),
        ] {
            let err = required_u64(&json!({"count": value}), "count")
                .expect_err("wrong type must not look missing")
                .to_string();
            assert!(
                err.contains("field 'count' must be a non-negative integer"),
                "{err}"
            );
        }
    }

    #[test]
    fn required_str_reports_provided_fields_on_missing_required_field() {
        let input = json!({"path": "src/lib.rs", "content": "new body"});
        let err = required_str(&input, "replace").expect_err("replace is missing");
        let message = err.to_string();
        assert!(message.contains("missing required field 'replace'"));
        assert!(message.contains("Input provided:"));
        assert!(message.contains("path"));
        assert!(message.contains("content"));
    }

    #[test]
    fn required_str_reports_wrong_type_when_field_exists() {
        let input = json!({"replace": [{"path": "src/lib.rs", "content": "new body"}]});
        let err = required_str(&input, "replace").expect_err("replace has wrong type");
        let message = err.to_string();
        assert!(message.contains("field 'replace' must be a string"));
        assert!(message.contains("got array"));
        assert!(message.contains(r#""content":"new body""#));
        assert!(message.contains(r#""path":"src/lib.rs""#));
    }

    #[test]
    fn tool_error_display_matches_legacy_text() {
        let err = ToolError::missing_field("path");
        assert_eq!(
            err.to_string(),
            "Failed to validate input: missing required field 'path'"
        );
    }

    #[test]
    fn tool_error_missing_field_constructor() {
        let err = ToolError::missing_field("my_field");
        assert!(matches!(err, ToolError::MissingField { field } if field == "my_field"));
    }

    #[test]
    fn tool_error_not_available_displays_reason() {
        let err = ToolError::not_available("custom tool not found");

        assert!(matches!(err, ToolError::NotAvailable { .. }));
        assert_eq!(
            err.to_string(),
            "Failed to locate tool: custom tool not found"
        );
    }

    #[test]
    fn tool_error_permission_denied_displays_reason() {
        let err = ToolError::permission_denied("unauthorized user");

        assert!(matches!(err, ToolError::PermissionDenied { .. }));
        assert_eq!(
            err.to_string(),
            "Failed to authorize tool execution: unauthorized user"
        );
    }

    #[test]
    fn tool_error_execution_failed_displays_reason() {
        let err = ToolError::execution_failed("process crashed");

        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message, .. } if message == "process crashed")
        );
        assert_eq!(err.to_string(), "Failed to execute tool: process crashed");
    }

    #[test]
    fn tool_error_invalid_input_creates_correct_variant() {
        let err = ToolError::invalid_input("test invalid message");
        match err {
            ToolError::InvalidInput { message } => {
                assert_eq!(message, "test invalid message");
            }
            _ => panic!("Expected ToolError::InvalidInput, got {err:?}"),
        }
    }

    #[test]
    fn tool_error_path_escape_display() {
        let path = std::path::PathBuf::from("../outside");
        let err = ToolError::path_escape(path);
        assert_eq!(
            err.to_string(),
            "Failed to resolve path '../outside': path escapes workspace"
        );
    }

    #[test]
    fn tool_call_execution_subject_uses_local_shell_command_and_cwd() {
        let call = ToolCall {
            name: "shell".to_string(),
            payload: ToolPayload::LocalShell {
                params: codewhale_protocol::LocalShellParams {
                    command: "ls -l".to_string(),
                    cwd: Some("/custom/dir".to_string()),
                    timeout_ms: None,
                },
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };

        assert_eq!(
            call.execution_subject("/fallback/dir"),
            ("ls -l".to_string(), "/custom/dir".to_string(), "shell")
        );
    }

    #[test]
    fn tool_call_execution_subject_falls_back_for_shell_without_cwd() {
        let call = ToolCall {
            name: "shell".to_string(),
            payload: ToolPayload::LocalShell {
                params: codewhale_protocol::LocalShellParams {
                    command: "echo hello".to_string(),
                    cwd: None,
                    timeout_ms: None,
                },
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };

        assert_eq!(
            call.execution_subject("/fallback/dir"),
            (
                "echo hello".to_string(),
                "/fallback/dir".to_string(),
                "shell"
            )
        );
    }

    #[test]
    fn tool_call_execution_subject_uses_tool_name_for_non_shell_payloads() {
        let call = ToolCall {
            name: "my_tool".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            source: ToolCallSource::Direct,
            raw_tool_call_id: None,
        };

        assert_eq!(
            call.execution_subject("/fallback/dir"),
            ("my_tool".to_string(), "/fallback/dir".to_string(), "tool")
        );
    }
}

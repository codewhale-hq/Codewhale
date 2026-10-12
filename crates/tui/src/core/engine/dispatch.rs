//! Tool dispatch — plan/execute helpers for the per-turn tool batch.
//!
//! Extracted from `core/engine.rs` (P1.3). The high-level ordering still
//! lives in `Engine::run_turn`; this module owns:
//!
//! * Streaming-buffer parsing into a finalized `serde_json::Value` tool input
//!   (`final_tool_input`, `parse_tool_input`, fenced/JSON segment helpers).
//! * Policy predicates the turn loop consults — when a batch can run in
//!   parallel and the small set of read-only MCP tools that are safe to run
//!   in parallel.
//! * The tool execution plan/outcome types the batch driver passes around.
//!
//! All items are `pub(super)`-only: the public engine surface (Op/Event,
//! `EngineHandle`, `spawn_engine`) stays in `core/engine.rs`.

use serde_json::json;

use crate::tools::spec::{
    ResourceClaim, ToolError, ToolExecutionOutcome, ToolResult, ToolResultContentBlock,
    schedule_non_conflicting,
};
use codewhale_models::{Tool, ToolCaller};

use super::ToolUseState;

const MAX_SCHEMA_CONTAINER_REPAIR_BYTES: usize = 64 * 1024;

// === Types ============================================================

pub(super) struct ToolExecOutcome {
    pub(super) index: usize,
    pub(super) id: String,
    pub(super) model_call: Option<crate::core::events::ModelToolCall>,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
    pub(super) started_at: std::time::Instant,
    pub(super) terminal: ToolExecutionOutcome,
    pub(super) content_blocks: Vec<ToolResultContentBlock>,
    /// Read-result bytes before spillover adds call-specific artifact paths.
    pub(super) original_content_digest: Option<[u8; 32]>,
}

/// Notice appended as a user-role message when the guard first asks the worker
/// to change strategy after repeated no-progress denials (#6015).
pub(crate) const FLEET_STRATEGY_SWITCH_NOTICE: &str = "Fleet strategy switch required: repeated permission denials produced no new evidence. The rejected action is held. Use another permitted tool from the current catalog to make progress, or report completed work and the blocker. Do not work around permissions or request the same approval again.";

/// Notice appended when denials continue past the strategy switch: the next
/// response is report-only and its tool calls are admission-held (#6015).
pub(crate) const FLEET_FINAL_REPORT_NOTICE: &str = "Fleet no-progress final report: permission denials continued after the strategy switch without new evidence. Your next response is report-only; no tools will execute. Report what you completed, exact evidence, the permission blocker and remaining work. This is the last response unless the user changes direction or authority.";

/// Terminal reason once the report-only response has been recorded (#6015).
pub(crate) const FLEET_NO_PROGRESS_STOP: &str = "Fleet worker stopped after repeated permission denials without new evidence. Work and tool results are retained in the transcript; review the blocker before resuming.";

/// Progress observations for one provider response, independent of tool finish
/// order. Only typed permission denials contribute to the retry guard (#6015).
#[derive(Default)]
pub(crate) struct FleetDenialBatch {
    denied: std::collections::HashSet<String>,
    made_progress: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FleetDenialAction {
    Continue,
    SwitchStrategy,
    FinalReport,
}

/// Turn-local guard for an engine with a Fleet authority envelope. This is an
/// admission predicate and result accumulator, not another execution loop.
/// Three responses give the model two opportunities to use denial feedback;
/// after one strategy notice, three more denied responses request a report.
/// Fleet sub-agent workers run the same guard in their own loop (#6015).
#[derive(Default)]
pub(crate) struct FleetDenialGuard {
    denied_rounds: std::collections::HashMap<String, u8>,
    switch_requested: bool,
    recovery_denied_rounds: u8,
    denial_rounds_without_progress: u32,
    report_only: bool,
    // Last observed bytes per read request: paths alone cannot distinguish a
    // changed file, and an unchanged read must not repeatedly reset denials.
    // Coverage is bounded; an evicted observation is treated conservatively
    // as new evidence. No raw arguments/bytes are kept.
    reads: std::collections::VecDeque<([u8; 32], [u8; 32])>,
}

impl FleetDenialGuard {
    const REPEATED_DENIAL_ROUNDS: u8 = 3;
    const MAX_OBSERVATIONS: usize = 32;

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn report_only(&self) -> bool {
        self.report_only
    }

    pub(crate) fn awaiting_strategy_change(&self) -> bool {
        self.switch_requested
    }

    pub(super) fn denial_rounds_without_progress(&self) -> u32 {
        self.denial_rounds_without_progress
    }

    pub(crate) fn original_content_digest(
        name: &str,
        input: &serde_json::Value,
        output: &ToolResult,
    ) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let action = crate::tools::canonical_action::canonical_action_alias(name, input);
        (output.success
            && matches!(
                action,
                "read_file" | "list_dir" | "file_search" | "grep_files"
            ))
        .then(|| Sha256::digest(output.content.as_bytes()).into())
    }

    pub(crate) fn admission_error(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> Option<ToolError> {
        let action = crate::tools::canonical_action::canonical_action_alias(name, input);
        if self.report_only {
            Some(ToolError::permission_denied(
                "Fleet no-progress final report: no tools may execute in this response. Report completed work, evidence and the remaining blocker; do not change permission mode or retry tools.",
            ))
        } else if self.switch_requested && self.denied_rounds.contains_key(action) {
            Some(ToolError::permission_denied(
                "Fleet permission-denial loop: this action is held until useful permitted work or an explicit authority change. Use another permitted tool or report the blocker; do not change permission mode or request permission again.",
            ))
        } else {
            None
        }
    }

    pub(crate) fn observe(
        &mut self,
        batch: &mut FleetDenialBatch,
        name: &str,
        input: &serde_json::Value,
        status: crate::tools::spec::ToolTerminalStatus,
        result: &Result<ToolResult, ToolError>,
        original_content_digest: Option<[u8; 32]>,
    ) {
        use crate::tools::spec::ToolTerminalStatus;

        let action = crate::tools::canonical_action::canonical_action_alias(name, input);
        if status == ToolTerminalStatus::Denied
            && matches!(result, Err(ToolError::PermissionDenied { .. }))
        {
            batch.denied.insert(action.to_owned());
            return;
        }
        let Ok(output) = result else { return };
        if status != ToolTerminalStatus::Succeeded
            || !output.success
            || output.metadata.as_ref().is_some_and(|metadata| {
                metadata
                    .get("executed")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
                    || metadata
                        .get("cancelled")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
            })
        {
            return;
        }
        // Waiting is useful coordination, but its repeated success receipt is
        // neither new evidence nor a failure. Its own timeouts still govern it.
        if matches!(
            action,
            "exec_shell_wait" | "exec_wait" | "terminal/wait" | "wait_for_dev_server" | "sleep"
        ) || name == "agent"
            && input
                .get("action")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|action| matches!(action, "wait" | "status" | "list"))
        {
            return;
        }
        if matches!(
            action,
            "read_file" | "list_dir" | "file_search" | "grep_files"
        ) {
            use sha2::{Digest, Sha256};

            let mut semantic_input = input.clone();
            if action != name
                && let Some(object) = semantic_input.as_object_mut()
            {
                object.remove("action");
            }
            let mut hasher = Sha256::new();
            hasher.update(action.as_bytes());
            hasher.update([0]);
            // Tool JSON preserves insertion order; reordered equivalent keys
            // must not manufacture a new read request.
            hasher.update(crate::client::canonical_json(&semantic_input).as_bytes());
            let key: [u8; 32] = hasher.finalize().into();
            let contents = original_content_digest
                .unwrap_or_else(|| Sha256::digest(output.content.as_bytes()).into());
            let previous = self
                .reads
                .iter()
                .position(|(old_key, _)| *old_key == key)
                .and_then(|index| self.reads.remove(index));
            batch.made_progress |=
                previous.is_none_or(|(_, old_contents)| old_contents != contents);
            self.reads.push_back((key, contents));
            if self.reads.len() > Self::MAX_OBSERVATIONS {
                self.reads.pop_front();
            }
        } else {
            // A successful mutation or unfamiliar tool is useful work. Do not
            // terminate it based on guesses about its content or side effects.
            batch.made_progress = true;
        }
    }

    pub(crate) fn finish_batch(&mut self, batch: FleetDenialBatch) -> FleetDenialAction {
        if self.report_only {
            return FleetDenialAction::Continue;
        }
        if batch.made_progress {
            self.denied_rounds.clear();
            self.switch_requested = false;
            self.recovery_denied_rounds = 0;
            self.denial_rounds_without_progress = 0;
            return FleetDenialAction::Continue;
        }
        if batch.denied.is_empty() {
            return FleetDenialAction::Continue;
        }
        self.denial_rounds_without_progress = self.denial_rounds_without_progress.saturating_add(1);
        if self.switch_requested {
            self.recovery_denied_rounds = self.recovery_denied_rounds.saturating_add(1);
            if self.recovery_denied_rounds >= Self::REPEATED_DENIAL_ROUNDS {
                self.report_only = true;
                return FleetDenialAction::FinalReport;
            }
            return FleetDenialAction::Continue;
        }
        for action in batch.denied {
            // The guard never retains payloads. Unknown families beyond this
            // bounded window do not evict an already observed denial streak.
            if self.denied_rounds.contains_key(&action)
                || self.denied_rounds.len() < Self::MAX_OBSERVATIONS
            {
                let count = self.denied_rounds.entry(action).or_default();
                *count = count.saturating_add(1);
            }
        }
        if self
            .denied_rounds
            .values()
            .any(|count| *count >= Self::REPEATED_DENIAL_ROUNDS)
        {
            self.switch_requested = true;
            FleetDenialAction::SwitchStrategy
        } else {
            FleetDenialAction::Continue
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ToolExecutionPlan {
    pub(super) index: usize,
    pub(super) id: String,
    pub(super) model_call: Option<crate::core::events::ModelToolCall>,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
    pub(super) caller: Option<ToolCaller>,
    pub(super) interactive: bool,
    pub(super) approval_required: bool,
    pub(super) approval_description: String,
    pub(super) approval_force_prompt: bool,
    pub(super) supports_parallel: bool,
    pub(super) read_only: bool,
    pub(super) detached_start: bool,
    pub(super) resources: Vec<ResourceClaim>,
    pub(super) blocked_error: Option<ToolError>,
    pub(super) guard_result: Option<ToolResult>,
}

pub(super) enum ToolExecutionBatch {
    Parallel(Vec<ToolExecutionPlan>),
    Serial(Box<ToolExecutionPlan>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolApprovalStamp {
    ApprovedByUser,
    ApprovedWithPolicy,
}

impl ToolApprovalStamp {
    const ALL: [Self; 2] = [Self::ApprovedByUser, Self::ApprovedWithPolicy];

    fn decision(self) -> &'static str {
        match self {
            Self::ApprovedByUser => "approved_by_user",
            Self::ApprovedWithPolicy => "approved_with_policy",
        }
    }

    fn model_visible_note(self) -> &'static str {
        match self {
            Self::ApprovedByUser => {
                "[approval] This tool call required approval and was approved by the user before execution."
            }
            Self::ApprovedWithPolicy => {
                "[approval] This tool call required approval and was approved by the user with an adjusted execution policy before execution."
            }
        }
    }
}

pub(super) fn stamp_tool_result_approval(result: &mut ToolResult, approval: ToolApprovalStamp) {
    let approval_metadata = json!({
        "required": true,
        "decision": approval.decision(),
        "model_visible": true,
    });
    let metadata = result.metadata.get_or_insert_with(|| json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert("approval".to_string(), approval_metadata);
    } else {
        let prior = std::mem::replace(metadata, json!({}));
        if let Some(object) = metadata.as_object_mut() {
            object.insert("_prior".to_string(), prior);
            object.insert("approval".to_string(), approval_metadata);
        }
    }

    let note = approval.model_visible_note();
    if result.content.starts_with("[approval] ") {
        return;
    }
    if result.content.is_empty() {
        result.content = note.to_string();
    } else {
        result.content = format!("{note}\n\n{}", result.content);
    }
}

/// The tool output a person reads: the result without the note
/// [`stamp_tool_result_approval`] put in front of it for the model (#6566).
///
/// Only a result the engine stamped (`metadata.approval.model_visible`) loses
/// a note, and only that decision's exact note text followed by a blank line
/// (or nothing). Output that merely begins with "[approval] " — from a
/// command, a file, or anything else a tool read — is shown whole, so
/// injected text cannot hide tool output from the person.
pub(crate) fn content_without_approval_note(result: &ToolResult) -> &str {
    let content = result.content.as_str();
    let approval = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("approval"));
    let stamped = approval
        .and_then(|approval| approval.get("model_visible"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let Some(stamp) = approval
        .and_then(|approval| approval.get("decision"))
        .and_then(serde_json::Value::as_str)
        .and_then(|decision| {
            ToolApprovalStamp::ALL
                .into_iter()
                .find(|stamp| stamp.decision() == decision)
        })
        .filter(|_| stamped)
    else {
        return content;
    };
    match content.strip_prefix(stamp.model_visible_note()) {
        Some("") => "",
        Some(rest) => rest.strip_prefix("\n\n").unwrap_or(content),
        None => content,
    }
}

// Hold the lock guard for the duration of a tool execution.
// The inner guards are held for RAII purposes (dropped when the guard is dropped).
pub(super) enum ToolExecGuard<'a> {
    Read(#[allow(dead_code)] tokio::sync::RwLockReadGuard<'a, ()>),
    Write(#[allow(dead_code)] tokio::sync::RwLockWriteGuard<'a, ()>),
}

// === Caller policy and errors ========================================

pub(super) fn caller_type_for_tool_use(caller: Option<&ToolCaller>) -> &str {
    caller.map_or("direct", |c| c.caller_type.as_str())
}

pub(super) fn caller_allowed_for_tool(
    caller: Option<&ToolCaller>,
    tool_def: Option<&Tool>,
) -> bool {
    let requested = caller_type_for_tool_use(caller);
    if let Some(def) = tool_def
        && let Some(allowed) = &def.allowed_callers
    {
        if allowed.is_empty() {
            return requested == "direct";
        }
        return allowed.iter().any(|item| item == requested);
    }
    requested == "direct"
}

/// Whole-word check for "mode"/"modes" — a plain `contains("mode")` also
/// matched "model", letting provider model errors skip the actionable-hint
/// suffix (#3020).
fn mentions_mode_word(lower: &str) -> bool {
    lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| word == "mode" || word == "modes")
}

#[cfg(test)]
pub(super) fn format_tool_error(err: &ToolError, tool_name: &str) -> String {
    format_tool_error_with_schema(err, tool_name, None)
}

pub(super) fn format_tool_error_with_schema(
    err: &ToolError,
    tool_name: &str,
    input_schema: Option<&serde_json::Value>,
) -> String {
    let message = match err {
        ToolError::InvalidInput { message } => {
            format!("Invalid input for tool '{tool_name}': {message}")
        }
        ToolError::MissingField { field } => {
            format!("Tool '{tool_name}' is missing required field '{field}'")
        }
        ToolError::PathEscape { path } => format!(
            "Path escapes workspace: {}. Use a workspace-relative path or enable trust mode.",
            path.display()
        ),
        ToolError::ExecutionFailed { message, .. } => message.clone(),
        ToolError::Timeout { seconds } => format!(
            "Tool '{tool_name}' timed out after {seconds}s. Try a narrower scope or a longer timeout."
        ),
        ToolError::Cancelled { message } => message.clone(),
        ToolError::NotAvailable { message } => {
            let lower = message.to_ascii_lowercase();
            // #3020: Pass through self-explanatory messages that already name the
            // cause (mode switch, allow_shell, feature flag).  Avoids appending a
            // conflicting "Check mode, feature flags" suffix on top of
            // a message that already names the mode and who can change it.
            if lower.contains("current tool catalog")
                || lower.contains("did you mean:")
                || mentions_mode_word(&lower)
                || lower.contains("allow_shell")
                || lower.contains("feature flag")
            {
                message.clone()
            } else {
                format!(
                    "Tool '{tool_name}' is not available: {message}. Check mode, feature flags, or tool name."
                )
            }
        }
        ToolError::PermissionDenied { message } => {
            let lower = message.to_ascii_lowercase();
            // #3020: messages that already name the denial cause get no
            // conflicting "Adjust approval mode" suffix. They keep the
            // `Tool '…' was denied:` lead, which is how a receipt tells a
            // call Codewhale blocked from one that ran and failed; an
            // approval denial already starts `Tool '…' denied by user`.
            if lower.contains("denied by user") {
                message.clone()
            } else if mentions_mode_word(&lower) || lower.contains("allow_shell") {
                format!("Tool '{tool_name}' was denied: {message}")
            } else {
                format!(
                    "Tool '{tool_name}' was denied: {message}. Adjust approval mode or request permission."
                )
            }
        }
    };

    let (category, bad_field) = match err {
        ToolError::InvalidInput { .. } => ("invalid_input", None),
        ToolError::MissingField { field } => ("missing_field", Some(field.as_str())),
        ToolError::PathEscape { .. } => ("path_escape", Some("path")),
        ToolError::NotAvailable { .. } => ("tool_not_available", Some("tool_name")),
        _ => return message,
    };
    let valid_shape = input_schema.cloned().unwrap_or_else(|| {
        serde_json::json!({
            "type": "object",
            "guidance": format!("Use the advertised input schema for '{tool_name}'")
        })
    });
    let feedback = serde_json::json!({
        "category": category,
        "bad_field": bad_field,
        "valid_shape": valid_shape,
        "retryable": true,
        "side_effect_status": "not_started"
    });
    format!("{message}\nTool validation feedback: {feedback}")
}

// === Streaming-buffer parsing =========================================

/// Promote a streaming `ToolUseState` to a finalized JSON input.
///
/// Order of preference:
///
///   1. `input_buffer` (the raw streamed delta concatenation) — parsed as
///      JSON. This is the most authoritative because it's what the model
///      actually emitted.
///   2. `input` (the per-delta best-effort parse mirror) — used when the
///      buffer is empty (pre-streaming tool calls take this path).
///   3. `input_buffer` non-empty but unparseable → fall back to `input`
///      (the per-delta parser has already mirrored the most recent valid
///      partial parse into `tool_state.input`).
pub(super) fn final_tool_input(state: &ToolUseState) -> serde_json::Value {
    if state.input_parse_error.is_some() {
        return malformed_tool_arguments_input(&state.input_buffer);
    }
    if !state.input_buffer.trim().is_empty()
        && let Some(parsed) = parse_tool_input(&state.input_buffer)
    {
        // Structure was synthesized to make this parse, so the argument text
        // was cut off. Route it to the same malformed-arguments path as an
        // outright parse failure rather than dispatching a completed guess.
        if parsed.structure_synthesized {
            return malformed_tool_arguments_input(&state.input_buffer);
        }
        return parsed.value;
    }
    state.input.clone()
}

/// A parsed tool-argument buffer, plus whether the parse only succeeded
/// because the repair ladder synthesized structure (see
/// `crate::tools::arg_repair`). Mid-stream callers mirroring partial state
/// may ignore the flag; the caller making the final dispatch decision must
/// not, because synthesized structure means the argument text was cut off.
pub(super) struct ParsedToolInput {
    pub(super) value: serde_json::Value,
    pub(super) structure_synthesized: bool,
}

pub(super) fn parse_tool_input(buffer: &str) -> Option<ParsedToolInput> {
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Try the deterministic arg-repair ladder first (handles trailing commas,
    // unclosed braces, embedded control chars, etc.)
    if let Ok(repaired) = crate::tools::arg_repair::repair(trimmed) {
        return Some(ParsedToolInput {
            value: repaired.value,
            structure_synthesized: repaired.structure_synthesized,
        });
    }
    // Fall back to existing strategies for code-fenced, double-encoded, and
    // segment-extraction patterns that the repair ladder doesn't cover.
    if let Some(stripped) = strip_code_fences(trimmed)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped)
    {
        return Some(ParsedToolInput {
            value,
            structure_synthesized: false,
        });
    }
    if let Ok(serde_json::Value::String(inner)) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&inner)
    {
        return Some(ParsedToolInput {
            value,
            structure_synthesized: false,
        });
    }
    extract_json_segment(trimmed)
        .and_then(|segment| serde_json::from_str::<serde_json::Value>(&segment).ok())
        .map(|value| ParsedToolInput {
            value,
            structure_synthesized: false,
        })
}

/// Decode a JSON container that a provider encoded as a string when the tool
/// schema explicitly requires an object or array.
///
/// This intentionally avoids general argument coercion: the string must be
/// bounded, parse as strict JSON, and decode to the declared container type.
/// Primitive strings are never coerced.
pub(super) fn normalize_schema_json_containers(
    value: &mut serde_json::Value,
    schema: &serde_json::Value,
) -> usize {
    let expected_container = if schema_declares_type(schema, "object") {
        Some("object")
    } else if schema_declares_type(schema, "array") {
        Some("array")
    } else {
        None
    };

    if let (Some(expected), serde_json::Value::String(encoded)) = (expected_container, &*value)
        && encoded.len() <= MAX_SCHEMA_CONTAINER_REPAIR_BYTES
        && let Ok(decoded) = serde_json::from_str::<serde_json::Value>(encoded)
        && ((expected == "object" && decoded.is_object())
            || (expected == "array" && decoded.is_array()))
    {
        *value = decoded;
        return 1 + normalize_schema_json_containers(value, schema);
    }

    match value {
        serde_json::Value::Object(object) => {
            let properties = schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            object
                .iter_mut()
                .map(|(key, child)| {
                    properties
                        .and_then(|items| items.get(key))
                        .map(|child_schema| normalize_schema_json_containers(child, child_schema))
                        .unwrap_or(0)
                })
                .sum()
        }
        serde_json::Value::Array(items) => schema
            .get("items")
            .map(|item_schema| {
                items
                    .iter_mut()
                    .map(|item| normalize_schema_json_containers(item, item_schema))
                    .sum()
            })
            .unwrap_or(0),
        _ => 0,
    }
}

fn schema_declares_type(schema: &serde_json::Value, expected: &str) -> bool {
    match schema.get("type") {
        Some(serde_json::Value::String(value)) => value == expected,
        Some(serde_json::Value::Array(values)) => values.iter().any(|value| value == expected),
        _ => false,
    }
}

pub(super) fn malformed_tool_arguments_input(buffer: &str) -> serde_json::Value {
    json!({ "raw_arguments": buffer })
}

pub(super) fn malformed_tool_arguments_error(buffer: &str) -> String {
    format!("malformed tool arguments from model: expected valid JSON, got {buffer:?}")
}

fn strip_code_fences(text: &str) -> Option<String> {
    if !text.contains("```") {
        return None;
    }
    let line_count = text.lines().count();
    let mut lines = Vec::with_capacity(line_count);
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        lines.push(line);
    }
    let stripped = lines.join("\n");
    let stripped = stripped.trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

fn extract_json_segment(text: &str) -> Option<String> {
    extract_balanced_segment(text, '{', '}').or_else(|| extract_balanced_segment(text, '[', ']'))
}

fn extract_balanced_segment(text: &str, open: char, close: char) -> Option<String> {
    let start = text.find(open)?;
    let mut depth = 0i32;
    let mut end = None;
    for (offset, ch) in text[start..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                end = Some(start + offset + ch.len_utf8());
                break;
            }
        }
    }
    end.map(|end_idx| text[start..end_idx].to_string())
}

// === Dispatch policy ==================================================

#[cfg(test)]
pub(super) fn should_parallelize_tool_batch(plans: &[ToolExecutionPlan]) -> bool {
    if plans.is_empty() || !plans.iter().all(tool_plan_can_join_parallel_batch) {
        return false;
    }
    schedule_non_conflicting(
        plans
            .iter()
            .map(|plan| ((), plan.resources.clone()))
            .collect(),
    )
    .len()
        == 1
}

pub(super) fn tool_plan_is_parallel_safe(plan: &ToolExecutionPlan) -> bool {
    plan.read_only && plan.supports_parallel && !plan.approval_required && !plan.interactive
}

pub(super) fn tool_plan_can_join_parallel_batch(plan: &ToolExecutionPlan) -> bool {
    plan.blocked_error.is_none()
        && (tool_plan_is_parallel_safe(plan)
            || (plan.detached_start && !plan.approval_required && !plan.interactive))
}

pub(super) fn plan_tool_execution_batches(
    plans: Vec<ToolExecutionPlan>,
) -> Vec<ToolExecutionBatch> {
    let mut batches = Vec::new();
    let mut parallel_candidates = Vec::new();

    let flush_parallel = |parallel_candidates: &mut Vec<_>,
                          batches: &mut Vec<ToolExecutionBatch>| {
        for chunk in schedule_non_conflicting(std::mem::take(parallel_candidates)) {
            batches.push(ToolExecutionBatch::Parallel(chunk));
        }
    };

    for plan in plans {
        if tool_plan_can_join_parallel_batch(&plan) {
            let resources = plan.resources.clone();
            parallel_candidates.push((plan, resources));
            continue;
        }

        flush_parallel(&mut parallel_candidates, &mut batches);
        batches.push(ToolExecutionBatch::Serial(Box::new(plan)));
    }

    flush_parallel(&mut parallel_candidates, &mut batches);

    batches
}

pub(super) fn mcp_tool_is_parallel_safe(name: &str) -> bool {
    matches!(
        name,
        "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "mcp_read_resource"
            | "read_mcp_resource"
            | "mcp_get_prompt"
    )
}

pub(super) fn mcp_tool_is_read_only(name: &str) -> bool {
    matches!(
        name,
        "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "mcp_read_resource"
            | "read_mcp_resource"
            | "mcp_get_prompt"
    )
}

pub(super) fn mcp_tool_approval_description(name: &str, input: &serde_json::Value) -> String {
    use crate::tools::approval_cache::{ComputerUseUserGate, computer_use_user_gate};

    // K1/K2: a Computer Use consent or script card names exactly what the
    // person is granting. Generic "may have side effects" text is how a
    // model-issued consent used to read as routine.
    match computer_use_user_gate(name, input) {
        Some(ComputerUseUserGate::Consent {
            action,
            app,
            bundle_id,
            scope,
            remember,
            confirm,
        }) => {
            if confirm {
                // The plugin paused on an action that cannot be taken back
                // and handed the model a token; approving this card is the
                // person's confirmation of that one action.
                return "Computer Use confirmation requested by the model: allow the irreversible action (pay, buy, send, transfer or delete) the plugin just paused on. Approve only if you asked for exactly that action.".to_string();
            }
            let target = match scope {
                "foreground" => {
                    "shared-desktop foreground control (take the pointer and focus)".to_string()
                }
                _ => {
                    let app = app.as_deref().unwrap_or("<unnamed app>");
                    match bundle_id.as_deref() {
                        Some(bundle) => format!("app '{app}' (bundle id {bundle})"),
                        None => format!("app '{app}' (bundle id not given)"),
                    }
                }
            };
            let lifetime = if action == "revoke" {
                "clears session and persisted decisions, including a saved deny"
            } else if remember {
                "persisted until revoked"
            } else {
                "this session"
            };
            let verb = if action == "revoke" {
                "revoke recorded decisions for"
            } else {
                "allow"
            };
            return format!(
                "Computer Use consent requested by the model: {verb} {target}; scope: {scope}; {lifetime}. Approve only if you want this."
            );
        }
        Some(ComputerUseUserGate::AppScript {
            language,
            script_sha256,
            first_line,
            line_count,
        }) => {
            let shown = if line_count > 1 {
                format!("first of {line_count} lines")
            } else {
                "1 line".to_string()
            };
            return format!(
                "Computer Use app_script: run this exact {language} script outside the sandbox (sha256 {}, {shown}): {first_line}",
                &script_sha256[..16]
            );
        }
        Some(ComputerUseUserGate::Computer {
            action,
            transport,
            destination,
        }) => {
            let transport = transport.as_deref().unwrap_or("default");
            let destination = destination.as_deref().unwrap_or("<not given>");
            return format!(
                "Computer Use: {action} a computer the model will then drive (transport {transport}, {destination}). Approve only if you want this machine controlled."
            );
        }
        None => {}
    }
    match crate::mcp::mcp_tool_approval_hint(name) {
        _ if mcp_tool_is_read_only(name) => format!("Read-only MCP tool '{name}'"),
        Some(crate::mcp::McpToolApprovalHint::TrustedReadOnly) => {
            format!("Read-only MCP tool '{name}' (declared by a reviewed plugin)")
        }
        Some(crate::mcp::McpToolApprovalHint::Destructive) => {
            format!("MCP tool '{name}' is marked destructive by its server")
        }
        None => format!("MCP tool '{name}' may have side effects"),
    }
}

#[cfg(test)]
mod schema_json_container_tests {
    use super::*;
    use crate::tools::spec::ToolSpec;
    use serde_json::json;

    #[test]
    fn decodes_nested_containers_and_passes_tool_validation() {
        let schema = crate::tools::user_input::RequestUserInputTool::default().input_schema();
        let encoded_options = serde_json::to_string(&json!([
            { "label": "Repository", "description": "Inspect the current repository" },
            { "label": "Workspace", "description": "Inspect the whole workspace" }
        ]))
        .expect("encode options");
        let encoded_questions = serde_json::to_string(&json!([{
            "header": "Scope",
            "id": "scope",
            "question": "Which scope should be inspected?",
            "options": encoded_options
        }]))
        .expect("encode questions");
        let mut input = json!({ "questions": encoded_questions });

        assert_eq!(normalize_schema_json_containers(&mut input, &schema), 2);
        assert!(input["questions"].is_array());
        assert!(input["questions"][0]["options"].is_array());
        crate::tools::user_input::UserInputRequest::from_value(&input)
            .expect("normalized input must still pass tool-specific validation");
    }

    #[test]
    fn leaves_primitives_wrong_types_and_unbounded_strings_unchanged() {
        let schema = json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "count": { "type": "integer" },
                "items": { "type": "array" },
                "oversized": { "type": "array" }
            }
        });
        let oversized = format!("[\"{}\"]", "x".repeat(MAX_SCHEMA_CONTAINER_REPAIR_BYTES));
        let mut input = json!({
            "text": "[\"still text\"]",
            "count": "10",
            "items": "{\"wrong\":\"container\"}",
            "oversized": oversized
        });
        let before = input.clone();

        assert_eq!(normalize_schema_json_containers(&mut input, &schema), 0);
        assert_eq!(input, before);
    }
}

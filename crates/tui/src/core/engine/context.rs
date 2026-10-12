//! Context budgeting and prompt-shaping helpers for the engine.
//!
//! These functions are shared by the streaming turn loop, capacity flow, and
//! engine session maintenance code. Keeping them here prevents the top-level
//! engine module from accumulating unrelated context-policy details.

use crate::config::ProviderKind;
use crate::context_budget::ContextBudget;
#[cfg(test)]
pub(super) use crate::route_budget::effective_max_output_tokens;
pub(super) use crate::route_budget::effective_max_output_tokens_for_route;
use crate::tools::spec::ToolResult;
use codewhale_config::route::RouteLimits;
use codewhale_models::SystemPrompt;
use serde_json::Value;
/// Allow a few emergency recovery attempts before failing the turn.
pub(super) const MAX_CONTEXT_RECOVERY_ATTEMPTS: u8 = 2;
/// Max chars to keep from metadata-provided output summaries.
const TOOL_RESULT_METADATA_SUMMARY_CHARS: usize = 320;

#[cfg(test)]
pub(super) use crate::compaction::COMPACTION_SUMMARY_MARKER;

/// What the model sees of one tool result (#6508).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultContextView {
    pub(crate) text: String,
    /// The view leaves out bytes of the result and no session artifact holds
    /// them yet. The engine saves the full output before the result fans out,
    /// so the view it builds afterwards can name a ref instead.
    pub(crate) needs_full_output_artifact: bool,
}

impl ToolResultContextView {
    fn whole(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            needs_full_output_artifact: false,
        }
    }
}

/// A structured summary and whether it left anything of the raw result out.
struct ContextSummary {
    text: String,
    lossy: bool,
}

/// Where the full output of a tool call can be read back, from its metadata.
struct RecoveryRef<'a> {
    path: &'a str,
    /// The `art_<id>` ref `retrieve_tool_result` resolves. `None` for a
    /// legacy spill, which no tool call reaches.
    artifact_id: Option<&'a str>,
}

fn recovery_ref(metadata: Option<&Value>) -> Option<RecoveryRef<'_>> {
    let obj = metadata?.as_object()?;
    let text = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    if let Some(artifact_id) = text("artifact_id").filter(|id| id.starts_with("art_")) {
        let path = text("artifact_path").or_else(|| text("spillover_path"))?;
        return Some(RecoveryRef {
            path,
            artifact_id: Some(artifact_id),
        });
    }
    text("spillover_path").map(|path| RecoveryRef {
        path,
        artifact_id: None,
    })
}

pub(super) fn summarize_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let take = limit.saturating_sub(3);
    let mut out: String = text.chars().take(take).collect();
    out.push_str("...");
    out
}

/// [`summarize_text`] that records whether it cut anything.
fn keep_text(text: &str, limit: usize, lossy: &mut bool) -> String {
    let kept = summarize_text(text, limit);
    *lossy |= kept.len() != text.len();
    kept
}

/// [`summarize_text_head_tail`] that records whether it cut anything.
fn keep_head_tail(text: &str, limit: usize, lossy: &mut bool) -> String {
    let kept = summarize_text_head_tail(text, limit);
    *lossy |= kept.len() != text.len();
    kept
}

fn summarize_text_head_tail(text: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    if limit <= 20 {
        return summarize_text(text, limit);
    }

    let marker = "\n\n[... output truncated for context ...]\n\n";
    let marker_len = marker.chars().count();
    if limit <= marker_len + 20 {
        return summarize_text(text, limit);
    }

    let remaining = limit - marker_len;
    let head_len = remaining.saturating_mul(2) / 3;
    let tail_len = remaining.saturating_sub(head_len);
    let head: String = text.chars().take(head_len).collect();
    let tail_vec: Vec<char> = text.chars().rev().take(tail_len).collect();
    let tail: String = tail_vec.into_iter().rev().collect();
    format!("{head}{marker}{tail}")
}

fn tool_result_metadata_summary(metadata: Option<&serde_json::Value>) -> Option<String> {
    let obj = metadata?.as_object()?;
    for key in ["summary", "stdout_summary", "stderr_summary", "message"] {
        if let Some(text) = obj.get(key).and_then(serde_json::Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(summarize_text(trimmed, TOOL_RESULT_METADATA_SUMMARY_CHARS));
            }
        }
    }
    None
}

fn summarize_subagent_status(status: &serde_json::Value, lossy: &mut bool) -> String {
    if let Some(raw) = status.as_str() {
        return raw.to_string();
    }
    if let Some(obj) = status.as_object()
        && let Some((kind, value)) = obj.iter().next()
    {
        if let Some(reason) = value.as_str().filter(|s| !s.trim().is_empty()) {
            return format!("{kind}({})", keep_text(reason.trim(), 120, lossy));
        }
        return kind.to_string();
    }
    status.to_string()
}

fn summarize_subagent_snapshot(
    snapshot: &serde_json::Value,
    index: usize,
    result_limit: usize,
    lossy: &mut bool,
) -> String {
    if let Some(inner) = snapshot.get("snapshot") {
        return summarize_subagent_snapshot(inner, index, result_limit, lossy);
    }

    let Some(obj) = snapshot.as_object() else {
        return format!(
            "- item {index}: {}",
            keep_text(&snapshot.to_string(), result_limit, lossy)
        );
    };

    let agent_id = obj
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let agent_type = obj
        .get("agent_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("agent");
    let status = obj
        .get("status")
        .map(|status| summarize_subagent_status(status, lossy))
        .unwrap_or_else(|| "unknown".to_string());
    let objective = obj
        .get("assignment")
        .and_then(|assignment| assignment.get("objective"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| keep_text(s, 220, lossy));
    let result = obj
        .get("result")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| keep_text(s, result_limit, lossy));
    let steps = obj.get("steps_taken").and_then(serde_json::Value::as_u64);
    let duration_ms = obj.get("duration_ms").and_then(serde_json::Value::as_u64);

    let mut lines = vec![format!("- {agent_id} ({agent_type}) status={status}")];
    if let Some(objective) = objective {
        lines.push(format!("  objective: {objective}"));
    }
    match result {
        Some(result) => lines.push(format!("  result: {result}")),
        None => lines.push("  result: not available yet".to_string()),
    }
    if steps.is_some() || duration_ms.is_some() {
        let steps = steps
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let duration_ms = duration_ms
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!("  stats: steps={steps}, duration_ms={duration_ms}"));
    }
    lines.join("\n")
}

/// Guidance heading every summarized sub-agent result. It names only tools
/// the model can call: `read` and `bash` are eager, and `handle_read` is
/// deferred, so it carries its activation path (#6747).
pub(crate) fn subagent_summary_guidance() -> String {
    format!(
        "Child results are self-reports; verify side effects with `read` (or `bash`, e.g. `git status`, where available) before claiming success.\n\
         Use `handle_read` on `transcript_handle` for bounded transcript slices when the returned summary is not enough; {}.\n",
        crate::tools::handle::HANDLE_READ_ACTIVATION_HINT
    )
}

/// A payload is a sub-agent snapshot when it carries the identity/status shape
/// this summarizer knows how to render (`agent_id`/`agent_type`, optionally
/// wrapped in a `snapshot` field).
fn looks_like_subagent_snapshot(value: &serde_json::Value) -> bool {
    let value = value.get("snapshot").unwrap_or(value);
    value
        .as_object()
        .is_some_and(|obj| obj.contains_key("agent_id") || obj.contains_key("agent_type"))
}

/// Sub-agent snapshots shown in full before the rest are only counted.
const SUBAGENT_SNAPSHOTS_SHOWN: usize = 8;
/// Floor for one sub-agent's result text on the smallest routes.
const SUBAGENT_RESULT_MIN_CHARS: usize = 1_600;

fn compact_subagent_tool_result_for_context(
    tool_name: &str,
    raw: &str,
    budget: usize,
) -> Option<ContextSummary> {
    if tool_name != "agent" {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let snapshots: Vec<&serde_json::Value> = match &parsed {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![&parsed],
        _ => return None,
    };

    // Coordination envelopes (`wait`, `status`, `claim`, ...) carry typed
    // fields the parent needs verbatim: `settled`, `still_running`,
    // `timed_out`, `waited_ms`, `note`. Projecting them through the snapshot
    // renderer replaced every one with `unknown (agent) status=unknown` and
    // dropped the real payload. Summarize only snapshot-shaped results; let
    // anything else fall through to the generic bounded path.
    if snapshots.is_empty()
        || !snapshots
            .iter()
            .all(|value| looks_like_subagent_snapshot(value))
    {
        return None;
    }

    // Each shown result gets an equal share of half the route budget, so a
    // child's report is not cut at a fixed size on a large route.
    let shown = snapshots.len().min(SUBAGENT_SNAPSHOTS_SHOWN);
    let result_limit = (budget / 2 / shown.max(1)).max(SUBAGENT_RESULT_MIN_CHARS);
    let mut lossy = snapshots.len() > SUBAGENT_SNAPSHOTS_SHOWN;
    let mut out = String::from("[sub-agent result summarized for parent context]\n");
    out.push_str(&subagent_summary_guidance());
    for (idx, snapshot) in snapshots.iter().enumerate() {
        if idx >= SUBAGENT_SNAPSHOTS_SHOWN {
            out.push_str(&format!(
                "- ... {} more sub-agent result(s) omitted from context summary\n",
                snapshots.len().saturating_sub(idx)
            ));
            break;
        }
        out.push_str(&summarize_subagent_snapshot(
            snapshot,
            idx + 1,
            result_limit,
            &mut lossy,
        ));
        out.push('\n');
    }
    Some(ContextSummary {
        text: out.trim_end().to_string(),
        lossy,
    })
}

fn json_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn json_number_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_i64()
                .map(|n| n.to_string())
                .or_else(|| value.as_u64().map(|n| n.to_string()))
        })
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
}

/// Characters a structured summary keeps for its own header lines and the
/// recovery footer, before the rest of the budget goes to output streams.
const STRUCTURED_SUMMARY_FRAME_CHARS: usize = 1_000;
/// Floor for one stream or gate detail on the smallest routes.
const STRUCTURED_DETAIL_MIN_CHARS: usize = 600;

fn compact_run_tests_result_for_context(
    raw: &str,
    metadata: Option<&Value>,
    budget: usize,
) -> Option<ContextSummary> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let success = parsed.get("success")?.as_bool()?;
    let exit_code = json_number_text(&parsed, "exit_code").unwrap_or_else(|| "?".to_string());
    let command = json_text(&parsed, "command").unwrap_or("(unknown command)");
    let stdout = json_text(&parsed, "stdout");
    let stderr = json_text(&parsed, "stderr");
    let mut lossy = false;

    let mut lines = vec![
        "[run_tests result summarized for context]".to_string(),
        format!(
            "status: {}, exit_code: {exit_code}",
            if success { "passed" } else { "failed" }
        ),
        format!("command: {}", keep_text(command, 300, &mut lossy)),
    ];
    // The cargo failure summary names the failing tests. It leads, so the
    // names stay inline however long the streams are.
    if let Some(summary) = metadata.and_then(|metadata| json_text(metadata, "summary")) {
        lines.push(format!("failure summary: {summary}"));
    }
    let header_chars: usize = lines.iter().map(|line| line.chars().count() + 1).sum();
    let streams = usize::from(stderr.is_some()) + usize::from(stdout.is_some());
    let stream_limit = (budget.saturating_sub(header_chars + STRUCTURED_SUMMARY_FRAME_CHARS)
        / streams.max(1))
    .max(STRUCTURED_DETAIL_MIN_CHARS);
    if let Some(stderr) = stderr {
        lines.push(format!(
            "stderr: {}",
            keep_head_tail(stderr, stream_limit, &mut lossy)
        ));
    }
    if let Some(stdout) = stdout {
        lines.push(format!(
            "stdout: {}",
            keep_head_tail(stdout, stream_limit, &mut lossy)
        ));
    }
    Some(ContextSummary {
        text: lines.join("\n"),
        lossy,
    })
}

fn run_verifier_status_rank(status: Option<&str>) -> u8 {
    match status.unwrap_or_default() {
        "failed" | "timeout" => 0,
        "skipped" => 1,
        "passed" => 2,
        _ => 3,
    }
}

/// Gates listed one per line before the rest are only counted.
const VERIFIER_GATES_SHOWN: usize = 12;

fn compact_run_verifiers_result_for_context(raw: &str, budget: usize) -> Option<ContextSummary> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let gates = parsed.get("gates")?.as_array()?;
    let summary = json_text(&parsed, "summary")
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let passed = json_number_text(&parsed, "passed").unwrap_or_else(|| "?".to_string());
            let failed = json_number_text(&parsed, "failed").unwrap_or_else(|| "?".to_string());
            let skipped = json_number_text(&parsed, "skipped").unwrap_or_else(|| "?".to_string());
            format!("{passed} passed, {failed} failed, {skipped} skipped")
        });

    let mut ordered: Vec<&Value> = gates.iter().collect();
    ordered.sort_by(|a, b| {
        run_verifier_status_rank(json_text(a, "status"))
            .cmp(&run_verifier_status_rank(json_text(b, "status")))
            .then_with(|| json_text(a, "name").cmp(&json_text(b, "name")))
    });

    let mut lossy = ordered.len() > VERIFIER_GATES_SHOWN;
    let mut lines = vec![
        "[run_verifiers result summarized for context]".to_string(),
        format!("summary: {summary}"),
    ];
    let profile = json_text(&parsed, "profile");
    let level = json_text(&parsed, "level");
    if profile.is_some() || level.is_some() {
        lines.push(format!(
            "selection: profile={}, level={}",
            profile.unwrap_or("?"),
            level.unwrap_or("?")
        ));
    }

    let shown = ordered.iter().take(VERIFIER_GATES_SHOWN);
    let detailed = shown
        .clone()
        .filter(|gate| json_text(gate, "status") != Some("passed"))
        .count();
    let detail_limit = (budget.saturating_sub(STRUCTURED_SUMMARY_FRAME_CHARS * 2)
        / detailed.max(1))
    .max(STRUCTURED_DETAIL_MIN_CHARS);

    for gate in shown {
        let name = json_text(gate, "name").unwrap_or("gate");
        let ecosystem = json_text(gate, "ecosystem").unwrap_or("unknown");
        let status = json_text(gate, "status").unwrap_or("unknown");
        let exit = json_number_text(gate, "exit_code")
            .map(|code| format!(" exit={code}"))
            .unwrap_or_default();
        lines.push(format!("- {name} ({ecosystem}): {status}{exit}"));
        // A stream longer than the verifier keeps in memory is saved whole.
        for stream in ["stdout", "stderr"] {
            if let Some(reference) = json_text(gate, &format!("{stream}_log_ref")) {
                lines.push(format!(
                    "  full {stream}: retrieve_tool_result ref=\"{reference}\""
                ));
            }
        }

        let stdout = json_text(gate, "stdout");
        let stderr = json_text(gate, "stderr");
        if status == "passed" {
            // A passing gate's output stays out of context; the saved full
            // output keeps it readable.
            lossy |= stdout.is_some() || stderr.is_some();
            continue;
        }
        if let Some(command) = json_text(gate, "command") {
            lines.push(format!(
                "  command: {}",
                keep_text(command, 240, &mut lossy)
            ));
        }
        // One detail per gate: the skip reason, else stderr, else stdout.
        let (detail, other_streams) = match json_text(gate, "skipped_reason") {
            Some(reason) => (Some(reason), stderr.is_some() || stdout.is_some()),
            None => match stderr {
                Some(stderr) => (Some(stderr), stdout.is_some()),
                None => (stdout, false),
            },
        };
        lossy |= other_streams;
        if let Some(detail) = detail {
            lines.push(format!(
                "  detail: {}",
                keep_head_tail(detail, detail_limit, &mut lossy)
            ));
        }
    }
    if ordered.len() > VERIFIER_GATES_SHOWN {
        lines.push(format!(
            "- ... {} more gate(s) omitted from context summary",
            ordered.len() - VERIFIER_GATES_SHOWN
        ));
    }

    Some(ContextSummary {
        text: lines.join("\n"),
        lossy,
    })
}

fn compact_task_gate_run_result_for_context(raw: &str) -> Option<ContextSummary> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let gate = parsed.get("gate")?;
    let gate_name = json_text(gate, "gate").unwrap_or("gate");
    let status = json_text(gate, "status").unwrap_or("unknown");
    let command = json_text(gate, "command").unwrap_or("(unknown command)");
    let summary = json_text(gate, "summary")
        .or_else(|| json_text(&parsed, "stderr_summary"))
        .or_else(|| json_text(&parsed, "stdout_summary"));
    let exit = json_number_text(gate, "exit_code")
        .map(|code| format!(", exit_code: {code}"))
        .unwrap_or_default();
    let mut lossy = false;

    let mut lines = vec![
        "[task_gate_run result summarized for context]".to_string(),
        format!("gate: {gate_name}, status: {status}{exit}"),
        format!("command: {}", keep_text(command, 300, &mut lossy)),
    ];
    if let Some(summary) = summary {
        lines.push(format!("summary: {}", keep_text(summary, 800, &mut lossy)));
    }
    if let Some(log_path) = json_text(gate, "log_path") {
        lines.push(format!("log_path: {log_path}"));
    }
    Some(ContextSummary {
        text: lines.join("\n"),
        lossy,
    })
}

fn compact_structured_tool_result_for_context(
    tool_name: &str,
    raw: &str,
    metadata: Option<&Value>,
    budget: usize,
) -> Option<ContextSummary> {
    match tool_name {
        "run_tests" => compact_run_tests_result_for_context(raw, metadata, budget),
        "run_verifiers" => compact_run_verifiers_result_for_context(raw, budget),
        // `tasks` is the unified durable-task tool (piagent phase B); its
        // gate_run action emits the same gate payload as the legacy
        // `task_gate_run` alias. The compactor returns None unless the
        // content actually parses as a gate result, so non-gate `tasks`
        // results fall through to the generic path unchanged.
        "task_gate_run" | "tasks" => compact_task_gate_run_result_for_context(raw),
        _ => compact_todo_write_result_for_context(raw, metadata),
    }
}

fn compact_todo_write_result_for_context(
    raw: &str,
    metadata: Option<&Value>,
) -> Option<ContextSummary> {
    let metadata = metadata?;
    if metadata.get("canonical_tool").and_then(Value::as_str) != Some("todo_write") {
        return None;
    }
    let header = raw
        .lines()
        .next()
        .filter(|line| line.starts_with("Todo list updated ("))?;
    let active = metadata
        .pointer("/task_updates/checklist/items")?
        .as_array()?
        .iter()
        .find(|item| item.get("status").and_then(Value::as_str) == Some("in_progress"));
    let text = match active {
        Some(item) => format!(
            "{header}\nIn progress: #{} {}",
            item.get("id").and_then(Value::as_u64).unwrap_or_default(),
            item.get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim(),
        ),
        None => format!("{header}\nNo item in progress"),
    };
    Some(ContextSummary { text, lossy: false })
}

#[cfg(test)]
pub(crate) fn compact_tool_result_for_context(
    model: &str,
    tool_name: &str,
    output: &ToolResult,
) -> String {
    compact_tool_result_for_route(ProviderKind::Deepseek, model, None, tool_name, output)
}

pub(crate) fn compact_tool_result_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    tool_name: &str,
    output: &ToolResult,
) -> String {
    tool_result_context_view(provider, model, route_limits, tool_name, output).text
}

/// The model's view of one tool result (#6508).
///
/// There is one size authority: [`crate::route_budget::route_inline_char_budget`].
/// A result within it reaches the model whole, whatever the tool. A larger
/// one is cut to a head and tail around a footer that names where the full
/// output lives and the `art_<id>` ref `retrieve_tool_result` reads it back
/// with. The view never writes anything: when it would leave bytes out and no
/// artifact holds them, it says so through `needs_full_output_artifact`, and
/// the engine saves the full output and builds the view again.
///
/// Structured summaries (`run_tests`, `run_verifiers`, gate results,
/// sub-agent snapshots) keep their shape, scale their detail with the same
/// budget, and follow the same rule whenever they leave something out.
pub(crate) fn tool_result_context_view(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    tool_name: &str,
    output: &ToolResult,
) -> ToolResultContextView {
    let raw = output.content.trim();
    if raw.is_empty() {
        return ToolResultContextView::whole(String::new());
    }
    let metadata = output.metadata.as_ref();

    // A result already bounded by the adaptive evidence envelope is an
    // honest, context-sized preview whose footer names the artifact path and
    // a recovery instruction. Re-compacting it would strip that recovery
    // contract and double-truncate the output, so pass it through unchanged.
    if metadata
        .and_then(|metadata| metadata.get("evidence_available"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ToolResultContextView::whole(raw);
    }

    // The `read` primitive already bounds itself to an explicit per-call byte
    // budget and, when that budget truncates the file, ends with a footer
    // naming the exact offset to continue from. Compacting it a second time
    // would drop content the caller deliberately budgeted for *and* delete the
    // continuation contract. A result that stayed inside its declared budget
    // therefore passes through; one that exceeded it takes the path below.
    if metadata
        .and_then(|metadata| metadata.get("read_budget_bytes"))
        .and_then(Value::as_u64)
        .is_some_and(|budget| raw.len() as u64 <= budget)
    {
        return ToolResultContextView::whole(raw);
    }

    let budget =
        crate::route_budget::route_inline_char_budget_for_route(provider, model, route_limits);
    let recovery = recovery_ref(metadata);
    let recovery_path = recovery.as_ref().map(|recovery| recovery.path);
    let retrieval_ref = recovery.as_ref().and_then(|recovery| recovery.artifact_id);

    let summary = compact_subagent_tool_result_for_context(tool_name, raw, budget)
        .or_else(|| compact_structured_tool_result_for_context(tool_name, raw, metadata, budget));
    if let Some(summary) = summary {
        return summary_view(summary, budget, recovery.as_ref());
    }

    // Spillover already saved the full output and left a preview. Re-fit that
    // preview to the budget and keep its ref; never save the preview itself.
    if let Some(metadata) =
        metadata.filter(|metadata| metadata.get("retained_head_bytes").is_some())
        && let Some(text) = crate::tools::truncate::refit_spilled_preview(
            &output.content,
            metadata,
            budget,
            recovery_path,
            retrieval_ref,
        )
    {
        return ToolResultContextView::whole(text.trim().to_string());
    }

    if raw.chars().count() <= budget {
        return ToolResultContextView::whole(raw);
    }

    let lead = tool_result_metadata_summary(metadata)
        .map(|summary| format!("Summary: {summary}\n"))
        .unwrap_or_default();
    let body = crate::tools::truncate::fit_to_inline_budget(
        raw,
        budget.saturating_sub(lead.chars().count()),
        recovery_path,
        retrieval_ref,
    );
    ToolResultContextView {
        text: format!("{lead}{body}"),
        needs_full_output_artifact: recovery.is_none(),
    }
}

/// Finish a structured summary: when it left anything out, end it with the
/// same recovery instruction every other cut carries.
fn summary_view(
    summary: ContextSummary,
    budget: usize,
    recovery: Option<&RecoveryRef<'_>>,
) -> ToolResultContextView {
    if !summary.lossy {
        return ToolResultContextView::whole(summary.text);
    }
    let recovery_path = recovery.map(|recovery| recovery.path);
    let retrieval_ref = recovery.and_then(|recovery| recovery.artifact_id);
    let footer = match recovery_path {
        Some(path) => format!(
            "[full output at {path}; {}]",
            crate::tools::truncate::spillover_recovery_instruction(retrieval_ref)
        ),
        None => format!(
            "[the full output could not be saved; {}]",
            crate::tools::truncate::spillover_recovery_instruction(None)
        ),
    };
    let text = crate::tools::truncate::fit_to_inline_budget(
        &summary.text,
        budget.saturating_sub(footer.chars().count() + 1),
        recovery_path,
        retrieval_ref,
    );
    ToolResultContextView {
        text: format!("{text}\n{footer}"),
        needs_full_output_artifact: recovery.is_none(),
    }
}

pub(super) fn extract_compaction_summary_prompt(
    prompt: Option<SystemPrompt>,
) -> Option<SystemPrompt> {
    crate::compaction::extract_compaction_summary(prompt.as_ref())
}

/// Internal input-side token budget for a provider/model route:
/// `window - reserved_output - headroom`. Used by the preflight check,
/// emergency recovery, and capacity trimming to decide when to compact.
/// Unknown model ids fall back to the provider's conservative default instead
/// of disabling preflight; custom long-context deployments can still advertise
/// their window with a `-256k`/`-1024k` model suffix.
///
/// The reserved-output term is the route-effective request cap: exactly what
/// the API can receive after explicit overrides, compatibility/route ceilings,
/// and the route window are intersected. A second hidden reasoning reserve
/// would make preflight disagree with the wire request and can cause premature
/// compaction on otherwise valid large-window inputs.
#[cfg(test)]
pub(super) fn context_input_budget_for_provider(
    provider: ProviderKind,
    model: &str,
) -> Option<usize> {
    context_input_budget_for_route(provider, model, None, 0)
}

/// Public so external callers (e.g. a host/bridge deriving its own compaction
/// trigger line) can reuse the *exact* same internal input-budget math — window
/// minus the route-effective output reservation
/// (`route_output_reservation`) minus headroom —
/// instead of re-deriving those constants and silently drifting from the engine.
/// Pass `input_tokens = 0` to get the full emergency input budget for the route.
pub fn context_input_budget_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    input_tokens: usize,
) -> Option<usize> {
    route_context_budget_for_route(provider, model, route_limits, input_tokens)
        .and_then(|budget| usize::try_from(budget.available_input_tokens).ok())
}

#[cfg(test)]
pub(super) fn route_context_budget_for_provider(
    provider: ProviderKind,
    model: &str,
    input_tokens: usize,
) -> Option<ContextBudget> {
    route_context_budget_for_route(provider, model, None, input_tokens)
}

pub(super) fn route_context_budget_for_route(
    provider: ProviderKind,
    model: &str,
    route_limits: Option<RouteLimits>,
    input_tokens: usize,
) -> Option<ContextBudget> {
    crate::route_budget::route_context_budget(provider, model, route_limits, input_tokens)
}

pub(super) fn is_context_length_error_message(message: &str) -> bool {
    // Only genuine context-length rejections may drive the bounded
    // context-recovery retry. The broader `InvalidInput` bucket also holds
    // wrong-model rejections ("Model not exist."), malformed requests, and
    // truncated-output terminations, where re-sending a compacted history
    // cannot help and would hide the real error.
    let lower = message.to_lowercase();
    lower.contains("model output truncated")
        || lower.contains("model response incomplete")
        || crate::llm_client::is_context_length_message(&lower)
        || (lower.contains("requested") && lower.contains("tokens") && lower.contains("maximum"))
}

/// The turn is over: the input still exceeds the route's input budget after
/// the bounded recovery. Say what ran and name the levers that exist where
/// the message is read — an interactive session has `/compact` and `/clear`;
/// a headless host (`exec`, app-server, CI) has neither (#6374).
pub(super) fn context_overflow_exhausted_message(
    interactive: bool,
    emergency_compactions: u32,
    estimated_input: usize,
    input_budget: usize,
) -> String {
    let passes = match emergency_compactions {
        1 => "1 emergency compaction pass".to_string(),
        n => format!("{n} emergency compaction passes"),
    };
    let levers = if interactive {
        "Run /compact to summarize further or /clear to start over; a larger context route or a lower output cap also raises the input budget."
    } else {
        "Shorten the input or choose a larger context route; a lower output cap (CODEWHALE_MAX_OUTPUT_TOKENS) or a lower [compaction] retained_user_message_tokens raises the usable input budget."
    };
    format!(
        "Context is still above this route's input budget after {passes} \
         (~{estimated_input} tokens estimated, ~{input_budget} budget). {levers}"
    )
}

/// The single error line for a request that cannot fit the route and has
/// too little earlier conversation to summarize (experience mark 2). It names the real
/// cause and one next step instead of blaming a compaction that never had
/// anything to work with.
pub(super) fn context_does_not_fit_message(
    interactive: bool,
    local_ollama: bool,
    model: &str,
    estimated_input: usize,
    input_budget: usize,
    prefix_tokens: usize,
) -> String {
    let pick = |what: &str| {
        if interactive {
            format!("Pick {what}: /model.")
        } else {
            format!("Choose {what}.")
        }
    };
    if local_ollama && crate::local_ollama::looks_like_non_chat_tag(model) {
        return format!("{model} can't chat. {}", pick("a chat model"));
    }
    let larger = if local_ollama {
        "a larger model, or raise num_ctx"
    } else {
        "a larger model"
    };
    if prefix_tokens >= input_budget {
        format!(
            "{model}'s context window (~{input_budget} tokens usable) is smaller than \
             Codewhale's working instructions (~{prefix_tokens} tokens). {}",
            pick(larger)
        )
    } else {
        format!(
            "This message (~{estimated_input} tokens with Codewhale's instructions) does not \
             fit {model}'s window (~{input_budget} tokens usable), and there is not enough \
             earlier conversation to summarize. Shorten it, or {}",
            pick(larger).to_lowercase()
        )
    }
}

pub(super) fn is_image_input_rejection_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    let image_signal = lower.contains("image_url")
        || lower.contains("content.type")
        || lower.contains("content type")
        || lower.contains("does not support image")
        || lower.contains("image input")
        || lower.contains("unsupported modality")
        || lower
            .split(|character: char| !character.is_alphanumeric())
            .any(|term| term == "vision");
    let rejection_signal = lower.contains("400")
        || lower.contains("invalid")
        || lower.contains("unsupported")
        || lower.contains("not support");
    image_signal && rejection_signal
}

#[cfg(test)]
mod tests {
    use super::is_image_input_rejection_message;

    #[test]
    fn image_rejection_classifier_matches_provider_400s() {
        assert!(is_image_input_rejection_message(
            r#"request (400): {"error":{"code":"1214","message":"messages.content.type 参数非法, 取值范围 ['text']"}}"#
        ));
        assert!(is_image_input_rejection_message(
            "Invalid content type. image_url is only supported by certain models."
        ));
        assert!(!is_image_input_rejection_message("Model not exist."));
        assert!(!is_image_input_rejection_message("invalid revision id"));
        assert!(!is_image_input_rejection_message(
            "This model's maximum context length is 131072 tokens."
        ));
    }
}

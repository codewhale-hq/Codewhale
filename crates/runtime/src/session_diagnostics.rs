//! Privacy-first session failure diagnostics (#2022).
//!
//! This module intentionally consumes loose JSONL event shapes instead of one
//! exact persisted-session schema. Runtime logs, tool audits, and future bug
//! exports can all emit slightly different records; the classifier only needs
//! redacted handles, aggregate counts, and broad failure classes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use codewhale_error_taxonomy::{ErrorCategory, classify_error_message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFailureClass {
    CommandExit,
    Network,
    SandboxApproval,
    MissingDependency,
    Timeout,
    BackgroundJob,
    ToolSchema,
    Model,
    Unknown,
    UnclosedTurn,
}

impl fmt::Display for SessionFailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::CommandExit => "command_exit",
            Self::Network => "network",
            Self::SandboxApproval => "sandbox_approval",
            Self::MissingDependency => "missing_dependency",
            Self::Timeout => "timeout",
            Self::BackgroundJob => "background_job",
            Self::ToolSchema => "tool_schema",
            Self::Model => "model",
            Self::Unknown => "unknown",
            Self::UnclosedTurn => "unclosed_turn",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFailureSource {
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFailureSummary {
    pub total_lines: usize,
    pub malformed_lines: usize,
    pub counts: BTreeMap<SessionFailureClass, usize>,
    pub sources: BTreeMap<SessionFailureClass, Vec<SessionFailureSource>>,
}

impl SessionFailureSummary {
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn count(&self, class: SessionFailureClass) -> usize {
        self.counts.get(&class).copied().unwrap_or(0)
    }

    fn record(&mut self, class: SessionFailureClass, source: SessionFailureSource) {
        *self.counts.entry(class).or_insert(0) += 1;
        self.sources.entry(class).or_default().push(source);
    }
}

#[must_use]
pub fn analyze_session_failure_jsonl(jsonl: &str) -> SessionFailureSummary {
    let mut summary = SessionFailureSummary {
        total_lines: 0,
        malformed_lines: 0,
        counts: BTreeMap::new(),
        sources: BTreeMap::new(),
    };
    let mut open_turns: BTreeMap<String, SessionFailureSource> = BTreeMap::new();

    for (idx, raw_line) in jsonl.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        summary.total_lines += 1;
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            summary.malformed_lines += 1;
            continue;
        };

        let event = event_name(&value);
        let turn_id = string_field_any(&value, &["turn_id", "turnId", "run_id"]);
        let source = source_handle(line_no, &value, event.clone(), turn_id.as_deref());
        let failure_signal = has_failure_signal(&value);

        if event_matches(
            event.as_deref(),
            &["turn_started", "turnstarted", "turn_start"],
        ) {
            if let Some(turn_id) = turn_id {
                open_turns.insert(turn_id, source);
            }
            continue;
        }
        if event_matches(
            event.as_deref(),
            &[
                "turn_complete",
                "turncompleted",
                "turn_finished",
                "turnfinished",
            ],
        ) {
            if let Some(turn_id) = turn_id.as_ref() {
                open_turns.remove(turn_id);
            }
            if failure_signal {
                let class = classify_failure_signal(&value);
                summary.record(class, source);
            }
            continue;
        }

        if failure_signal {
            let class = classify_failure_signal(&value);
            summary.record(class, source);
        }
    }

    for (_, source) in open_turns {
        summary.record(SessionFailureClass::UnclosedTurn, source);
    }

    summary
}

#[must_use]
pub fn format_redacted_failure_summary(summary: &SessionFailureSummary) -> String {
    if summary.counts.is_empty() {
        return "No session failure signals detected.".to_string();
    }
    let mut lines = vec![format!(
        "Session failure diagnostics: {} JSONL lines inspected, {} malformed skipped.",
        summary.total_lines, summary.malformed_lines
    )];
    for (class, count) in &summary.counts {
        let sample = summary
            .sources
            .get(class)
            .and_then(|sources| sources.first())
            .map(format_source)
            .unwrap_or_else(|| "no source".to_string());
        lines.push(format!("- {class}: {count} (sample: {sample})"));
    }
    lines.join("\n")
}

fn source_handle(
    line: usize,
    value: &Value,
    event: Option<String>,
    turn_id: Option<&str>,
) -> SessionFailureSource {
    let tool_name = string_field_any(value, &["tool_name", "toolName", "tool"])
        .filter(|name| event.as_deref().is_none_or(|event| event != name));
    SessionFailureSource {
        line,
        event,
        turn_ref: turn_id.map(codewhale_util::utils::redacted_identifier_for_log),
        tool_name,
        timestamp: string_field_any(value, &["timestamp", "ts", "created_at", "createdAt"]),
    }
}

fn format_source(source: &SessionFailureSource) -> String {
    let mut parts = vec![format!("line {}", source.line)];
    if let Some(event) = source.event.as_deref() {
        parts.push(format!("event={event}"));
    }
    if let Some(turn_ref) = source.turn_ref.as_deref() {
        parts.push(format!("turn={turn_ref}"));
    }
    if let Some(tool_name) = source.tool_name.as_deref() {
        parts.push(format!("tool={tool_name}"));
    }
    if let Some(timestamp) = source.timestamp.as_deref() {
        parts.push(format!("ts={timestamp}"));
    }
    parts.join(" ")
}

fn has_failure_signal(value: &Value) -> bool {
    numeric_field_any(value, &["exit_code", "exitCode"]).is_some_and(|code| code != 0)
        || bool_field_any(value, &["success"]).is_some_and(|success| !success)
        || bool_field_any(value, &["is_error", "isError"]).unwrap_or(false)
        || failure_status(value).is_some()
        || string_field_any(value, &["error", "stderr"]).is_some_and(|text| !text.is_empty())
}

fn classify_failure_signal(value: &Value) -> SessionFailureClass {
    if let Some(message) = diagnostic_message(value) {
        return classify_session_failure(value, &message);
    }
    if let Some(status) = failure_status(value) {
        let lower = status.to_ascii_lowercase();
        if lower.contains("timeout") || lower.contains("timed_out") {
            return SessionFailureClass::Timeout;
        }
        if lower.contains("cancel") || lower.contains("background") || lower.contains("stale") {
            return SessionFailureClass::BackgroundJob;
        }
    }
    if numeric_field_any(value, &["exit_code", "exitCode"]).is_some_and(|code| code != 0) {
        return SessionFailureClass::CommandExit;
    }
    SessionFailureClass::Unknown
}

fn classify_session_failure(value: &Value, message: &str) -> SessionFailureClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("background")
        || lower.contains("task_shell")
        || lower.contains("job timed out")
        || lower.contains("job cancelled")
        || lower.contains("stale job")
    {
        return SessionFailureClass::BackgroundJob;
    }
    if lower.contains("sandbox")
        || lower.contains("approval")
        || lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("read-only")
        || lower.contains("access is denied")
    {
        return SessionFailureClass::SandboxApproval;
    }
    if lower.contains("command not found")
        || lower.contains("no such file or directory")
        || lower.contains("missing binary")
        || lower.contains("enoent")
        || lower.contains("not installed")
    {
        return SessionFailureClass::MissingDependency;
    }
    if lower.contains("missing field")
        || lower.contains("invalid tool")
        || lower.contains("invalid input")
        || lower.contains("schema")
        || lower.contains("tool arguments")
    {
        return SessionFailureClass::ToolSchema;
    }
    if numeric_field_any(value, &["exit_code", "exitCode"]).is_some_and(|code| code != 0)
        || lower.contains("non-zero")
        || lower.contains("exit status")
        || lower.contains("exit code")
    {
        return SessionFailureClass::CommandExit;
    }
    match classify_error_message(message) {
        ErrorCategory::Network | ErrorCategory::RateLimit => SessionFailureClass::Network,
        ErrorCategory::Timeout => SessionFailureClass::Timeout,
        ErrorCategory::Budget => SessionFailureClass::Unknown,
        ErrorCategory::Authorization => SessionFailureClass::SandboxApproval,
        ErrorCategory::Authentication => SessionFailureClass::Model,
        ErrorCategory::State => SessionFailureClass::MissingDependency,
        // A bare placeholder ("error") says nothing about a schema fault; the
        // classifier files it under Parse only so provider turns can say
        // "unreadable" (#6843).
        ErrorCategory::Parse
            if codewhale_error_taxonomy::unreadable_error_notice(message).is_some() =>
        {
            SessionFailureClass::Unknown
        }
        ErrorCategory::InvalidInput | ErrorCategory::Parse => SessionFailureClass::ToolSchema,
        ErrorCategory::Tool => SessionFailureClass::CommandExit,
        ErrorCategory::Internal if lower.contains("model") => SessionFailureClass::Model,
        ErrorCategory::Internal => SessionFailureClass::Unknown,
    }
}

fn diagnostic_message(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    collect_string_fields(
        value,
        &mut parts,
        &[
            "error", "message", "stderr", "reason", "result", "output", "content",
        ],
        0,
    );
    let mut seen = BTreeSet::new();
    let deduped = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .filter(|part| seen.insert(part.clone()))
        .collect::<Vec<_>>();
    (!deduped.is_empty()).then(|| deduped.join(" "))
}

fn collect_string_fields(value: &Value, out: &mut Vec<String>, keys: &[&str], depth: usize) {
    if depth > 4 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if keys
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    && let Some(text) = value.as_str()
                {
                    out.push(text.to_string());
                }
                collect_string_fields(value, out, keys, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_string_fields(item, out, keys, depth + 1);
            }
        }
        _ => {}
    }
}

fn event_name(value: &Value) -> Option<String> {
    string_field_any(value, &["event", "type", "kind"]).map(|event| normalize_event(&event))
}

fn normalize_event(event: &str) -> String {
    event
        .trim()
        .trim_matches('"')
        .replace(['-', ' ', '.'], "_")
        .to_ascii_lowercase()
}

fn event_matches(event: Option<&str>, aliases: &[&str]) -> bool {
    event.is_some_and(|event| aliases.contains(&event))
}

fn failure_status(value: &Value) -> Option<String> {
    string_field_any(value, &["status", "state", "outcome"]).filter(|status| {
        let normalized = normalize_event(status);
        matches!(
            normalized.as_str(),
            "failed"
                | "failure"
                | "error"
                | "errored"
                | "cancelled"
                | "canceled"
                | "timeout"
                | "timed_out"
                | "stale"
        )
    })
}

fn string_field_any(value: &Value, keys: &[&str]) -> Option<String> {
    string_field_any_at(value, keys, 0)
}

fn string_field_any_at(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.iter().find_map(|(candidate, value)| {
                    candidate.eq_ignore_ascii_case(key).then_some(value)
                }) && let Some(text) = value.as_str()
                {
                    return Some(text.to_string());
                }
            }
            for child in map.values() {
                if let Some(found) = string_field_any_at(child, keys, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| string_field_any_at(item, keys, depth + 1)),
        _ => None,
    }
}

fn numeric_field_any(value: &Value, keys: &[&str]) -> Option<i64> {
    numeric_field_any_at(value, keys, 0)
}

fn numeric_field_any_at(value: &Value, keys: &[&str], depth: usize) -> Option<i64> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.iter().find_map(|(candidate, value)| {
                    candidate.eq_ignore_ascii_case(key).then_some(value)
                }) && let Some(number) = value.as_i64()
                {
                    return Some(number);
                }
            }
            for child in map.values() {
                if let Some(found) = numeric_field_any_at(child, keys, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| numeric_field_any_at(item, keys, depth + 1)),
        _ => None,
    }
}

fn bool_field_any(value: &Value, keys: &[&str]) -> Option<bool> {
    bool_field_any_at(value, keys, 0)
}

fn bool_field_any_at(value: &Value, keys: &[&str], depth: usize) -> Option<bool> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.iter().find_map(|(candidate, value)| {
                    candidate.eq_ignore_ascii_case(key).then_some(value)
                }) && let Some(flag) = value.as_bool()
                {
                    return Some(flag);
                }
            }
            for child in map.values() {
                if let Some(found) = bool_field_any_at(child, keys, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| bool_field_any_at(item, keys, depth + 1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_error_placeholder_is_not_a_schema_failure() {
        // The taxonomy files a bare "error" under Parse so provider turns can
        // call it unreadable; a tool that printed only that is not a schema bug.
        let value = serde_json::json!({});
        assert_eq!(
            classify_session_failure(&value, "ERROR"),
            SessionFailureClass::Unknown
        );
    }

    #[test]
    fn synthetic_jsonl_classifies_environment_and_tool_failures() {
        let jsonl = r#"
{"event":"turn_started","turn_id":"turn-secret-1","timestamp":"2026-06-25T12:00:00Z"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"exec_shell","exit_code":127,"stderr":"bash: rg: command not found"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"exec_shell","exit_code":2,"stderr":"command failed with exit code 2"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"web_search","error":"DNS resolution failed for api.example.test"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"apply_patch","error":"Permission denied by sandbox: read-only filesystem"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"exec_shell","error":"request timed out after 30s"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"task_shell_wait","error":"background job timed out"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"mcp_tool","error":"missing field: tool arguments"}
{"event":"turn_complete","turn_id":"turn-secret-1","status":"completed"}
{"event":"turn_started","turn_id":"turn-secret-2"}
not json at all
"#;

        let summary = analyze_session_failure_jsonl(jsonl);

        assert_eq!(summary.malformed_lines, 1);
        assert_eq!(summary.count(SessionFailureClass::MissingDependency), 1);
        assert_eq!(summary.count(SessionFailureClass::CommandExit), 1);
        assert_eq!(summary.count(SessionFailureClass::Network), 1);
        assert_eq!(summary.count(SessionFailureClass::SandboxApproval), 1);
        assert_eq!(summary.count(SessionFailureClass::Timeout), 1);
        assert_eq!(summary.count(SessionFailureClass::BackgroundJob), 1);
        assert_eq!(summary.count(SessionFailureClass::ToolSchema), 1);
        assert_eq!(summary.count(SessionFailureClass::UnclosedTurn), 1);

        let sources = summary
            .sources
            .get(&SessionFailureClass::MissingDependency)
            .expect("missing-dependency source");
        assert_eq!(sources[0].tool_name.as_deref(), Some("exec_shell"));
        assert!(
            sources[0]
                .turn_ref
                .as_deref()
                .is_some_and(|turn| turn.starts_with("<redacted:")),
            "turn ids must be redacted: {sources:?}"
        );
    }

    #[test]
    fn redacted_summary_omits_raw_messages_and_paths() {
        let jsonl = r#"
{"event":"turn_started","turn_id":"turn-secret-1"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"read_file","timestamp":"2026-06-25T12:34:56Z","error":"No such file or directory: /Users/alice/secret/project/.env"}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);
        let rendered = format_redacted_failure_summary(&summary);

        assert!(rendered.contains("missing_dependency"));
        assert!(rendered.contains("line 3"));
        assert!(rendered.contains("tool=read_file"));
        assert!(rendered.contains("ts=2026-06-25T12:34:56Z"));
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains(".env"));
        assert!(!rendered.contains("turn-secret-1"));
    }

    #[test]
    fn successful_content_does_not_create_unknown_failure() {
        let jsonl = r#"
{"event":"turn_started","turn_id":"turn-secret-1"}
{"event":"tool_call_complete","turn_id":"turn-secret-1","tool_name":"exec_shell","success":true,"content":"command output mentioning error budgets is still normal content"}
{"event":"turn_complete","turn_id":"turn-secret-1","status":"completed","message":"done"}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);

        assert_eq!(summary.count(SessionFailureClass::Unknown), 0);
        assert_eq!(summary.count(SessionFailureClass::CommandExit), 0);
        assert_eq!(summary.count(SessionFailureClass::UnclosedTurn), 0);
        assert!(
            summary.counts.is_empty(),
            "summary should be empty: {summary:?}"
        );
    }

    #[test]
    fn empty_error_field_is_not_a_failure_signal() {
        let jsonl = r#"
{"event":"tool_call_complete","success":true,"error":"","stderr":""}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);

        assert!(
            summary.counts.is_empty(),
            "empty error/stderr sentinels should not signal failure: {summary:?}"
        );
    }

    #[test]
    fn nested_generic_fields_do_not_shadow_source_handles() {
        let jsonl = r#"
{"event":"tool_call_complete","turn_id":"turn-real","tool_name":"exec_shell","success":false,"payload":{"id":"nested-id","name":"nested-name","error":"nested error"}}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);
        let source = summary
            .sources
            .values()
            .flat_map(|sources| sources.iter())
            .next()
            .expect("failure source");

        assert_eq!(source.tool_name.as_deref(), Some("exec_shell"));
        assert!(
            source
                .turn_ref
                .as_deref()
                .is_some_and(|turn| turn.starts_with("<redacted:")),
            "top-level turn id should be redacted and used: {source:?}"
        );
    }

    #[test]
    fn deeply_nested_error_field_is_ignored() {
        let jsonl = r#"
{"event":"tool_call_complete","payload":{"a":{"b":{"c":{"d":{"e":{"error":"too deep"}}}}}}}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);

        assert!(
            summary.counts.is_empty(),
            "deeply nested error fields should not signal failure: {summary:?}"
        );
    }

    #[test]
    fn authentication_failures_are_model_failures_not_sandbox_approval() {
        let jsonl = r#"
{"event":"model_response","success":false,"error":"Authentication failed: invalid API key"}
"#;

        let summary = analyze_session_failure_jsonl(jsonl);

        assert_eq!(summary.count(SessionFailureClass::Model), 1);
        assert_eq!(summary.count(SessionFailureClass::SandboxApproval), 0);
    }
}

//! The input contracts the model sees: the canonical union schema, and the
//! per-action schemas the hidden legacy aliases still advertise.

use serde_json::{Value, json};

/// The union schema for the canonical `github` tool: `action` plus every
/// field any allowed action reads. `read_only` drops the write fields so the
/// Plan-mode surface cannot even describe a mutation.
pub(super) fn canonical_schema(allowed_actions: &[&str], read_only: bool) -> Value {
    let actions: Vec<&str> = allowed_actions.to_vec();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "action".to_string(),
        json!({
            "type": "string",
            "enum": actions,
            "description": "Action to perform."
        }),
    );
    properties.insert(
        "number".to_string(),
        json!({ "type": "integer", "minimum": 1, "description": "Issue/PR number (all actions)." }),
    );
    properties.insert(
        "include_comments".to_string(),
        json!({ "type": "boolean", "default": true, "description": "(action=issue_context)" }),
    );
    properties.insert("report_id".to_string(), json!({"type": "string", "maxLength": 73, "description": "(report_read) Opaque current-session draft ID returned by report_draft."}));
    properties.insert(
        "include_diff".to_string(),
        json!({ "type": "boolean", "default": false, "description": "(action=pr_context)" }),
    );
    if !read_only {
        properties.insert("revises".to_string(), json!({"type": "string", "maxLength": 73, "description": "(report_draft) Existing current-session draft to revise. Old bytes are preserved."}));
        properties.insert("report".to_string(), report_schema());
        properties.insert(
            "target".to_string(),
            json!({ "type": "string", "enum": ["issue", "pr"], "description": "(action=comment)" }),
        );
        properties.insert(
            "body".to_string(),
            json!({ "type": "string", "description": "Comment body (action=comment)." }),
        );
        properties.insert(
            "evidence".to_string(),
            json!({
                "type": "object",
                "description": "Evidence object (action=comment/close_*). Close actions require files_changed, tests_run, final_status.",
                "properties": {
                    "files_changed": { "type": "array", "items": { "type": "string" } },
                    "tests_run": { "type": "array", "items": { "type": "string" } },
                    "commits": { "type": "array", "items": { "type": "string" } },
                    "final_status": { "type": "string" }
                }
            }),
        );
        properties.insert(
            "acceptance_criteria".to_string(),
            json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "(action=close_issue/close_pr)" }),
        );
        properties.insert(
            "comment".to_string(),
            json!({ "type": "string", "description": "Optional closing comment (action=close_issue/close_pr)." }),
        );
        properties.insert(
            "allow_dirty".to_string(),
            json!({ "type": "boolean", "default": false, "description": "(action=close_issue/close_pr) Close is rejected when the worktree is dirty unless this is true." }),
        );
        properties.insert(
            "dry_run".to_string(),
            json!({ "type": "boolean", "default": false, "description": "(action=comment/close_issue/close_pr)" }),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false
    })
}

fn report_schema() -> Value {
    let narrative = json!({"type": "string", "minLength": 1, "maxLength": 1600});
    let items = json!({"type": "array", "minItems": 1, "maxItems": 8, "items": {"type": "string", "minLength": 1, "maxLength": 800}});
    let context = json!({"type": "string", "minLength": 1, "maxLength": 100});
    json!({"type": "object", "additionalProperties": false,
        "description": "(report_draft) Bounded narrative about a likely Codewhale defect, written by the active agent from observed evidence. No logs, code blocks, attachments, paths or private URLs. Only a local draft; publication unavailable.",
        "properties": {
            "title": {"type": "string", "minLength": 1, "maxLength": 160},
            "expected": narrative, "actual": narrative, "impact": narrative,
            "steps": items, "observed": items,
            "inferred": {"type": "array", "maxItems": 4, "items": {"type": "string", "minLength": 1, "maxLength": 800}},
            "reported_provider": context, "reported_tool": context, "reported_terminal": context,
            "related_issues": {"type": "array", "maxItems": 5, "items": {"type": "integer", "minimum": 1, "maximum": u32::MAX}, "description": "Already-known Codewhale issue numbers; displayed as agent-supplied, unverified references. No search is performed."}
        }, "required": ["title", "expected", "actual", "impact", "steps", "observed"]
    })
}

/// The exact schema the legacy per-action tool exposed, kept so hidden alias
/// registrations report an identical contract to the pre-unification tools.
pub(super) fn legacy_action_schema(action: &str) -> Value {
    match action {
        "issue_context" => json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "minimum": 1 },
                "include_comments": { "type": "boolean", "default": true }
            },
            "required": ["number"],
            "additionalProperties": false
        }),
        "pr_context" => json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "minimum": 1 },
                "include_diff": { "type": "boolean", "default": false }
            },
            "required": ["number"],
            "additionalProperties": false
        }),
        "comment" => json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "enum": ["issue", "pr"] },
                "number": { "type": "integer", "minimum": 1 },
                "body": { "type": "string" },
                "evidence": { "type": "object" },
                "dry_run": { "type": "boolean", "default": false }
            },
            "required": ["target", "number", "body", "evidence"],
            "additionalProperties": false
        }),
        // close_issue / close_pr share the close schema.
        _ => close_input_schema(),
    }
}

fn close_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "number": { "type": "integer", "minimum": 1 },
            "acceptance_criteria": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "evidence": {
                "type": "object",
                "properties": {
                    "files_changed": { "type": "array", "items": { "type": "string" } },
                    "tests_run": { "type": "array", "items": { "type": "string" } },
                    "commits": { "type": "array", "items": { "type": "string" } },
                    "final_status": { "type": "string" }
                },
                "required": ["files_changed", "tests_run", "final_status"]
            },
            "comment": { "type": "string" },
            "allow_dirty": { "type": "boolean", "default": false },
            "dry_run": { "type": "boolean", "default": false }
        },
        "required": ["number", "acceptance_criteria", "evidence"],
        "additionalProperties": false
    })
}

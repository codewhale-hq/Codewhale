//! Model-facing controller for Codewhale's continual RLM harness.

use async_trait::async_trait;
use serde_json::{Value, json};

use codewhale_runtime::continual_harness::{
    HarnessEntryKind, HarnessRefinement, overview, refine, remove,
};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Persistent, bounded harness state for context-aware, long-running work.
pub struct HarnessTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarnessAction {
    Overview,
    Refine,
    Remove,
}

fn parse_action(input: &Value) -> Result<HarnessAction, ToolError> {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("overview")
        .trim()
        .to_ascii_lowercase();
    match action.as_str() {
        "" | "overview" | "list" | "status" => Ok(HarnessAction::Overview),
        "refine" | "learn" => Ok(HarnessAction::Refine),
        "remove" | "forget" | "delete" => Ok(HarnessAction::Remove),
        _ => Err(ToolError::invalid_input(
            "harness action must be overview, refine, or remove",
        )),
    }
}

fn parse_kind(input: &Value) -> Result<HarnessEntryKind, ToolError> {
    match required_str(input, "kind")?.trim() {
        "prompt_note" => Ok(HarnessEntryKind::PromptNote),
        "subagent_spec" => Ok(HarnessEntryKind::SubagentSpec),
        "skill_hint" => Ok(HarnessEntryKind::SkillHint),
        other => Err(ToolError::invalid_input(format!(
            "harness kind `{other}` must be prompt_note, subagent_spec, or skill_hint"
        ))),
    }
}

#[async_trait]
impl ToolSpec for HarnessTool {
    fn name(&self) -> &'static str {
        "harness"
    }

    fn description(&self) -> &'static str {
        "Inspect or refine the durable continual harness for this workspace. Use action=overview at the start of substantial multi-turn work to recover bounded prompt notes, reusable sub-agent briefs, and skill hints. Use action=refine only after observing concrete evidence for a reusable improvement; it stores a small project-local entry that later turns receive as untrusted working guidance. Use action=remove to retire an obsolete entry. Keep large source material in rlm, compose parallel child work with workflow task(...), and use agent action=message/followup for child coordination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["overview", "refine", "remove"],
                    "description": "overview (default) lists harness state; refine writes one evidence-backed improvement; remove retires one exact id."
                },
                "kind": {
                    "type": "string",
                    "enum": ["prompt_note", "subagent_spec", "skill_hint"],
                    "description": "Required for refine: the kind of reusable improvement."
                },
                "title": {
                    "type": "string",
                    "description": "Required for refine: compact title for the improvement."
                },
                "content": {
                    "type": "string",
                    "description": "Required for refine: bounded, reusable guidance rather than a transcript or temporary scratch."
                },
                "evidence": {
                    "type": "string",
                    "description": "Required for refine: the concrete observation that justified retaining this guidance."
                },
                "id": {
                    "type": "string",
                    "description": "Required for remove: exact entry id returned by overview."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match parse_action(input) {
            Ok(HarnessAction::Overview) => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        matches!(parse_action(input), Ok(HarnessAction::Overview))
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        match parse_action(&input)? {
            HarnessAction::Overview => {
                let state = overview(&context.workspace).map_err(|error| {
                    ToolError::execution_failed(format!("harness overview failed: {error}"))
                })?;
                ToolResult::json(&json!({
                    "path": state.path,
                    "entries": state.entries,
                    "runtime": {
                        "context": "rlm keeps large context as a persistent Python variable",
                        "orchestration": "workflow composes task(...) calls and parallel fan-out",
                        "messaging": "agent action=message and action=followup coordinate active children",
                        "continuation": "goal keeps an explicit durable objective active"
                    }
                }))
                .map_err(|error| ToolError::execution_failed(error.to_string()))
            }
            HarnessAction::Refine => {
                let entry = refine(
                    &context.workspace,
                    HarnessRefinement {
                        kind: parse_kind(&input)?,
                        title: required_str(&input, "title")?.to_string(),
                        content: required_str(&input, "content")?.to_string(),
                        evidence: required_str(&input, "evidence")?.to_string(),
                    },
                )
                .map_err(|error| {
                    ToolError::execution_failed(format!("harness refinement failed: {error}"))
                })?;
                ToolResult::json(&json!({
                    "refined": entry,
                    "receipt": "Stored as project-local supplemental guidance for later turns."
                }))
                .map_err(|error| ToolError::execution_failed(error.to_string()))
            }
            HarnessAction::Remove => {
                let entry =
                    remove(&context.workspace, required_str(&input, "id")?).map_err(|error| {
                        ToolError::execution_failed(format!("harness removal failed: {error}"))
                    })?;
                ToolResult::json(&json!({"removed": entry}))
                    .map_err(|error| ToolError::execution_failed(error.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn overview_is_read_only_and_refinement_round_trips() {
        let tmp = tempdir().expect("tempdir");
        let context = ToolContext::new(tmp.path());
        let tool = HarnessTool;

        assert!(tool.is_read_only_for(&json!({"action": "overview"})));
        assert_eq!(
            tool.approval_requirement_for(&json!({"action": "overview"})),
            ApprovalRequirement::Auto
        );

        let created = tool
            .execute(
                json!({
                    "action": "refine",
                    "kind": "prompt_note",
                    "title": "Preserve exact release evidence",
                    "content": "Keep literal test result lines with each release claim.",
                    "evidence": "A prior release handoff mixed stale CI evidence with current local output."
                }),
                &context,
            )
            .await
            .expect("refinement result");
        let created_json: Value = serde_json::from_str(&created.content).expect("json receipt");
        let id = created_json["refined"]["id"].as_str().expect("entry id");

        let overview = tool
            .execute(json!({"action": "overview"}), &context)
            .await
            .expect("overview result");
        assert!(overview.content.contains(id));
        assert!(overview.content.contains("workflow composes"));
    }

    #[tokio::test]
    async fn refine_rejects_missing_evidence() {
        let tmp = tempdir().expect("tempdir");
        let context = ToolContext::new(tmp.path());
        let error = HarnessTool
            .execute(
                json!({
                    "action": "refine",
                    "kind": "skill_hint",
                    "title": "Use a skill",
                    "content": "Try the relevant skill."
                }),
                &context,
            )
            .await
            .expect_err("evidence must be required");
        assert!(error.to_string().contains("evidence"));
    }
}

//! Canonical action-based wrapper for git inspection tools.
//!
//! The model sees one tool: `Git` with an `action` parameter
//! (status | diff | log | show | blame | commit_plan | fetch | merge_tree).
//! The per-action legacy execution aliases were removed in v0.9.3.
//! `commit_plan` (#3999) is the propose-only atomic-commit planner: it returns
//! a split plan and writes nothing. `fetch` is the family's one ref-mutating,
//! network-reaching action — the bounded verify-mode surface (#6298) — so the
//! family is read-only end to end *except* fetch; `merge_tree` is a pure read.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::git::{GitCommitPlanTool, GitDiffTool, GitStatusTool};
use super::git_history::{GitBlameTool, GitFetchTool, GitLogTool, GitMergeTreeTool, GitShowTool};
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codewhale_tools::required_action;

pub struct GitTool {
    name: &'static str,
    forced_action: Option<&'static str>,
}

impl GitTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
        }
    }

    const ACTIONS: &'static [&'static str] = &[
        "status",
        "diff",
        "log",
        "show",
        "blame",
        "commit_plan",
        "fetch",
        "merge_tree",
    ];

    fn required_action(&self, input: &Value) -> Result<String, ToolError> {
        if let Some(forced) = self.forced_action {
            return Ok(forced.to_string());
        }
        required_action(input, self.name, Self::ACTIONS)
    }

    fn strip_action(&self, input: Value) -> Result<Value, ToolError> {
        let mut input = input;
        if let Some(obj) = input.as_object_mut() {
            obj.remove("action");
            Ok(input)
        } else {
            Err(ToolError::invalid_input(
                "Git tool input must be a JSON object, e.g. {\"action\": \"status\"}",
            ))
        }
    }
}

#[async_trait]
impl ToolSpec for GitTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        self.name == "Git"
    }

    fn description(&self) -> &'static str {
        "Inspect repository state and history with status, diff, log, show, blame, or merge_tree; commit_plan proposes an ordered atomic-commit split of the working tree; fetch updates remote-tracking refs from a configured remote. Only fetch touches the network or mutates refs; every other action is read-only and parallel-safe."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "show", "blame", "commit_plan", "fetch", "merge_tree"],
                    "description": "Action to perform. commit_plan returns a proposed split of the working tree into dependency-ordered commits (rejecting cycles) and writes nothing; land each group with git add/commit. merge_tree computes a merge result without touching the working tree."
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file path to scope the git command"
                },
                "cached": {
                    "type": "boolean",
                    "description": "When true, diff staged changes (action=diff)"
                },
                "unified": {
                    "type": "integer",
                    "description": "Number of context lines for diff or show output"
                },
                "max_count": {
                    "type": "integer",
                    "description": "Maximum commits to return (action=log)"
                },
                "author": {
                    "type": "string",
                    "description": "Author filter (action=log)"
                },
                "since": {
                    "type": "string",
                    "description": "Lower date bound (action=log)"
                },
                "until": {
                    "type": "string",
                    "description": "Upper date bound (action=log)"
                },
                "rev": {
                    "type": "string",
                    "description": "Revision to show (action=show) or blame against (action=blame)"
                },
                "patch": {
                    "type": "boolean",
                    "description": "Include patch hunks (action=show)"
                },
                "stat": {
                    "type": "boolean",
                    "description": "Include stat summary (action=show)"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to include (action=blame)"
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines to include (action=blame)"
                },
                "porcelain": {
                    "type": "boolean",
                    "description": "Emit line-porcelain output (action=blame)"
                },
                "remote": {
                    "type": "string",
                    "description": "Configured remote name to fetch from, default origin (action=fetch). Never a URL."
                },
                "refspecs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional refspecs to fetch, e.g. pull/123/head (action=fetch); a `src:dst` destination must be under refs/remotes/. Empty fetches the remote's defaults."
                },
                "ours": {
                    "type": "string",
                    "description": "First revision (action=merge_tree)"
                },
                "theirs": {
                    "type": "string",
                    "description": "Second revision (action=merge_tree)"
                },
                "base": {
                    "type": "string",
                    "description": "Optional merge base (--merge-base); omit and git finds the bases itself (action=merge_tree)"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        // `fetch` is the family's one network-reaching, ref-mutating action;
        // it holds the same bar as the other code/process-executing tools.
        if input.get("action").and_then(Value::as_str) == Some("fetch") {
            ApprovalRequirement::Required
        } else {
            ApprovalRequirement::Auto
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        // Only `fetch` mutates (remote-tracking refs). A missing action
        // resolves to the `status` policy default, which is read-only.
        input
            .get("action")
            .and_then(Value::as_str)
            .is_none_or(|action| action != "fetch")
    }

    fn supports_parallel_for(&self, input: &Value) -> bool {
        // Concurrent fetches contend on ref locks; inspection stays parallel.
        input.get("action").and_then(Value::as_str) != Some("fetch")
    }

    fn starts_detached_for(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = self.required_action(&input)?;
        let input = self.strip_action(input)?;

        match action.as_str() {
            "status" => GitStatusTool.execute(input, context).await,
            "diff" => GitDiffTool.execute(input, context).await,
            "log" => GitLogTool.execute(input, context).await,
            "show" => GitShowTool.execute(input, context).await,
            "blame" => GitBlameTool.execute(input, context).await,
            "commit_plan" => GitCommitPlanTool.execute(input, context).await,
            "fetch" => GitFetchTool.execute(input, context).await,
            "merge_tree" => GitMergeTreeTool.execute(input, context).await,
            other => Err(ToolError::invalid_input(format!(
                "Unknown Git action \"{other}\"; nothing was run. Pass one of: {}.",
                Self::ACTIONS.join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    async fn err(input: Value) -> String {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        GitTool::new("Git")
            .execute(input, &ctx)
            .await
            .expect_err("call must be refused")
            .to_string()
    }

    #[tokio::test]
    async fn missing_action_is_refused_with_the_valid_values() {
        let message = err(json!({"path": "src"})).await;
        assert!(message.contains("requires an `action`"), "{message}");
        assert!(message.contains("nothing was run"), "{message}");
        assert!(message.contains("blame"), "{message}");
    }

    #[tokio::test]
    async fn unknown_action_names_the_actions_that_dispatch() {
        let message = err(json!({"action": "commit"})).await;
        assert!(message.contains("commit"), "{message}");
        assert!(
            message.contains("status, diff, log, show, blame, commit_plan, fetch, merge_tree"),
            "{message}"
        );
    }

    /// `commit_plan` proposes and never writes, so the envelope must class it
    /// with the other read-only Git actions rather than as a mutation (#3999).
    #[test]
    fn commit_plan_is_bounded_read_only_for_the_envelope() {
        use crate::tools::execution_envelope::{CallClass, classify_call};
        let tool = GitTool::new("Git");
        let input = json!({"action": "commit_plan"});
        assert!(tool.is_read_only_for(&input));
        assert_eq!(classify_call("Git", &input, &tool), CallClass::Bounded);
    }

    /// `fetch` is the family's one ref-mutating action: held approval, not
    /// read-only, not parallel-safe, and classed as bounded fetch — shell
    /// plus network, never workspace write (#6298). `merge_tree` is a pure
    /// read and stays with the inspection actions.
    #[test]
    fn fetch_and_merge_tree_classification() {
        use crate::tools::execution_envelope::{CallClass, classify_call};
        let tool = GitTool::new("Git");

        let fetch = json!({"action": "fetch", "remote": "origin"});
        assert!(!tool.is_read_only_for(&fetch));
        assert_eq!(
            tool.approval_requirement_for(&fetch),
            ApprovalRequirement::Required
        );
        assert!(!tool.supports_parallel_for(&fetch));
        assert_eq!(classify_call("Git", &fetch, &tool), CallClass::BoundedFetch);

        let merge_tree = json!({"action": "merge_tree", "ours": "main", "theirs": "side"});
        assert!(tool.is_read_only_for(&merge_tree));
        assert_eq!(
            tool.approval_requirement_for(&merge_tree),
            ApprovalRequirement::Auto
        );
        assert!(tool.supports_parallel_for(&merge_tree));
        assert_eq!(classify_call("Git", &merge_tree, &tool), CallClass::Bounded);
    }

    #[test]
    fn advertised_actions_match_the_actions_that_dispatch() {
        let schema = GitTool::new("Git").input_schema();
        let advertised: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|value| value.as_str().expect("string"))
            .collect();
        assert_eq!(advertised, GitTool::ACTIONS);
    }
}

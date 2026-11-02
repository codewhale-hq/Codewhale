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
        "review_scope",
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
        "Inspect repository state and history with status, diff, log, show, blame, merge_tree, or review_scope; commit_plan proposes an ordered atomic-commit split of the working tree; fetch updates remote-tracking refs from a configured remote. Only fetch touches the network or mutates refs; every other action is read-only and parallel-safe."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "show", "blame", "commit_plan", "fetch", "merge_tree", "review_scope"],
                    "description": "Action to perform. commit_plan returns a proposed split of the working tree into dependency-ordered commits (rejecting cycles) and writes nothing; land each group with git add/commit. merge_tree computes a merge result without touching the working tree."
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file path to scope the git command"
                },
                "query": {
                    "type": "string",
                    "enum": ["branches", "commits", "root", "resolve", "ahead_behind", "merge_base"],
                    "description": "Bounded metadata query for choosing a review scope in the current workspace (action=review_scope). resolve uses rev; ahead_behind and merge_base require full commit IDs in ours and theirs."
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
            "review_scope" => review_scope(input, context).await,
            other => Err(ToolError::invalid_input(format!(
                "Unknown Git action \"{other}\"; nothing was run. Pass one of: {}.",
                Self::ACTIONS.join(", ")
            ))),
        }
    }
}

async fn review_scope(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_input("review_scope requires a metadata query"))?;
    let field = |name: &str| -> Result<String, ToolError> {
        let value = input
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid_input(format!("review_scope requires {name}")))?;
        if value.is_empty()
            || value.len() > 1024
            || value.starts_with('-')
            || value.chars().any(char::is_whitespace)
            || value.chars().any(char::is_control)
        {
            return Err(ToolError::invalid_input("Invalid Git review reference"));
        }
        Ok(value.to_string())
    };
    let commit = |name: &str| -> Result<String, ToolError> {
        let value = field(name)?;
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ToolError::invalid_input(
                "Git review comparisons require full commit IDs",
            ));
        }
        Ok(value)
    };
    let (allowed, args): (&[&str], Vec<String>) = match query {
        "branches" => (
            &["query"],
            vec![
                "for-each-ref".into(),
                "--count=4097".into(),
                "--format=%(refname:short)".into(),
                "refs/heads".into(),
            ],
        ),
        "commits" => (
            &["query"],
            vec![
                "log".into(),
                "--no-show-signature".into(),
                "--max-count=100".into(),
                "--format=%H%x09%s".into(),
            ],
        ),
        "root" => (
            &["query"],
            vec!["rev-parse".into(), "--show-toplevel".into()],
        ),
        "resolve" => (
            &["query", "rev"],
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "--end-of-options".into(),
                format!("{}^{{commit}}", field("rev")?),
            ],
        ),
        "ahead_behind" => (
            &["query", "ours", "theirs"],
            vec![
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                format!("{}...{}", commit("ours")?, commit("theirs")?),
            ],
        ),
        "merge_base" => (
            &["query", "ours", "theirs"],
            vec!["merge-base".into(), commit("ours")?, commit("theirs")?],
        ),
        _ => {
            return Err(ToolError::invalid_input(
                "Unsupported Git review metadata query",
            ));
        }
    };
    if input
        .as_object()
        .is_none_or(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return Err(ToolError::invalid_input(
            "Unsupported Git review metadata field",
        ));
    }
    let workspace = context.workspace.clone();
    let mut command =
        tokio::task::spawn_blocking(move || super::git::read_only_git_command(&workspace))
            .await
            .map_err(|error| {
                ToolError::execution_failed(format!("Git preparation failed: {error}"))
            })??;
    command
        .args(["--no-optional-locks", "--no-pager", "--no-lazy-fetch"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    let mut command = tokio::process::Command::from(command);
    let cancel = context.cancel_token.clone().unwrap_or_default();
    let stop = async {
        tokio::select! {
            () = cancel.cancelled() => {},
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => {},
        }
    };
    let run = codewhale_runtime::process_tree::contained_output_with_input_bounded(
        &mut command,
        Vec::new(),
        1024 * 1024,
        64 * 1024,
        stop,
    )
    .await
    .map_err(|error| ToolError::execution_failed(format!("Git review metadata failed: {error}")))?;
    if run.stopped {
        return Err(ToolError::execution_failed(
            "Git review metadata was cancelled or exceeded its deadline",
        ));
    }
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    if query == "branches" && stdout.lines().count() > 4096 {
        return Err(ToolError::execution_failed(
            "Git review branch list exceeds 4096 entries",
        ));
    }
    Ok(ToolResult::success(
        json!({
            "stdout": stdout,
            "stderr": String::from_utf8_lossy(&run.output.stderr),
            "exit_code": run.output.status.code(),
        })
        .to_string(),
    ))
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

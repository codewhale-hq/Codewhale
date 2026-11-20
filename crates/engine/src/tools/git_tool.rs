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

    fn allowed_params(action: &str) -> Option<&'static [&'static str]> {
        match action {
            "status" | "commit_plan" => Some(&["path"]),
            "diff" => Some(&["path", "cached", "unified", "rev", "base", "target"]),
            "log" => Some(&["path", "max_count", "author", "since", "until"]),
            "show" => Some(&["rev", "path", "patch", "stat", "unified"]),
            "blame" => Some(&["path", "rev", "start_line", "max_lines", "porcelain"]),
            "fetch" => Some(&["remote", "refspecs", "path"]),
            "merge_tree" => Some(&["ours", "theirs", "base", "path"]),
            "review_scope" => Some(&["query", "rev", "ours", "theirs"]),
            _ => None,
        }
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
                    "description": "Revision or range to diff (action=diff, e.g. main..HEAD or HEAD~3), show (action=show), blame against (action=blame), or resolve (action=review_scope)"
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
                    "description": "Base revision to diff from (action=diff), or optional merge base (--merge-base, action=merge_tree)"
                },
                "target": {
                    "type": "string",
                    "description": "Target revision to diff to; requires base (action=diff)"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::Network,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
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
        let unknown_action = |action: &str| {
            ToolError::invalid_input(format!(
                "Unknown Git action \"{action}\"; nothing was run. Pass one of: {}.",
                Self::ACTIONS.join(", ")
            ))
        };
        let allowed = Self::allowed_params(&action).ok_or_else(|| unknown_action(&action))?;
        validate_action_params(&action, &input, allowed)?;

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
            other => Err(unknown_action(other)),
        }
    }
}

fn validate_action_params(action: &str, input: &Value, allowed: &[&str]) -> Result<(), ToolError> {
    let Some(object) = input.as_object() else {
        return Err(ToolError::invalid_input(
            "Git tool input must be a JSON object, e.g. {\"action\": \"status\"}",
        ));
    };
    let mut unexpected: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unexpected.is_empty() {
        return Ok(());
    }
    unexpected.sort_unstable();
    Err(ToolError::invalid_input(format!(
        "Unexpected parameter(s) for Git action=\"{action}\": {}. Allowed parameters for \"{action}\": {}.",
        unexpected.join(", "),
        allowed.join(", ")
    )))
}

fn apply_review_git_options(command: &mut std::process::Command, args: Vec<String>) {
    command
        .args(["--no-optional-locks", "--no-pager"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1");
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
    apply_review_git_options(&mut command, args);
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

    #[test]
    fn review_git_options_use_env_not_a_flag_old_git_rejects() {
        let mut command = std::process::Command::new("git");
        apply_review_git_options(&mut command, vec!["rev-parse".into(), "HEAD".into()]);
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["--no-optional-locks", "--no-pager", "rev-parse", "HEAD"]
        );
        assert!(!args.iter().any(|a| a == "--no-lazy-fetch"));
        let lazy = command
            .get_envs()
            .find(|(key, _)| *key == "GIT_NO_LAZY_FETCH")
            .and_then(|(_, value)| value);
        assert_eq!(lazy, Some(std::ffi::OsStr::new("1")));
    }

    #[tokio::test]
    async fn review_scope_resolve_runs_against_system_git() {
        if !<codewhale_dependencies::Git as codewhale_dependencies::ExternalTool>::available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let result = GitTool::new("Git")
            .execute(
                json!({"action": "review_scope", "query": "resolve", "rev": "HEAD"}),
                &ctx,
            )
            .await
            .expect("review_scope runs");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["exit_code"], 0, "{body}");
        assert_eq!(
            body["stdout"].as_str().map(|s| s.trim().len()),
            Some(40),
            "{body}"
        );
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

    #[test]
    fn capabilities_reflect_fetch_while_keeping_inspection_actions_read_only() {
        let tool = GitTool::new("Git");
        let caps = tool.capabilities();
        assert!(!caps.contains(&ToolCapability::ReadOnly), "{caps:?}");
        assert!(!tool.is_read_only());
        assert!(caps.contains(&ToolCapability::Network), "{caps:?}");
        assert!(caps.contains(&ToolCapability::Sandboxable), "{caps:?}");

        for action in [
            "status",
            "diff",
            "log",
            "show",
            "blame",
            "commit_plan",
            "merge_tree",
            "review_scope",
        ] {
            assert!(
                tool.is_read_only_for(&json!({"action": action})),
                "{action} must stay read-only"
            );
        }
        assert!(!tool.is_read_only_for(&json!({"action": "fetch"})));
    }

    #[tokio::test]
    async fn diff_supports_rev_base_target_and_rejects_flag_injection() {
        if !<codewhale_dependencies::Git as codewhale_dependencies::ExternalTool>::available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "user.email", "t@example.com"]);

        std::fs::write(tmp.path().join("a.txt"), "one\n").expect("write a");
        std::fs::write(tmp.path().join("b.txt"), "b1\n").expect("write b");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "c1"]);

        std::fs::write(tmp.path().join("a.txt"), "two\n").expect("write a");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "c2"]);

        std::fs::write(tmp.path().join("a.txt"), "three\n").expect("write a");
        std::fs::write(tmp.path().join("b.txt"), "b2\n").expect("write b");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "c3"]);

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = GitTool::new("Git");

        let rev_diff = tool
            .execute(json!({"action": "diff", "rev": "HEAD~1..HEAD"}), &ctx)
            .await
            .expect("rev range diff");
        assert!(rev_diff.success, "{}", rev_diff.content);
        assert!(rev_diff.content.contains("-two"), "{}", rev_diff.content);
        assert!(rev_diff.content.contains("+three"), "{}", rev_diff.content);
        assert!(rev_diff.content.contains("+b2"), "{}", rev_diff.content);

        let base_target = tool
            .execute(
                json!({
                    "action": "diff",
                    "base": "HEAD~2",
                    "target": "HEAD~1",
                    "path": "a.txt"
                }),
                &ctx,
            )
            .await
            .expect("base..target diff");
        assert!(base_target.success, "{}", base_target.content);
        assert!(
            base_target.content.contains("-one"),
            "{}",
            base_target.content
        );
        assert!(
            base_target.content.contains("+two"),
            "{}",
            base_target.content
        );
        assert!(
            !base_target.content.contains("b.txt"),
            "{}",
            base_target.content
        );

        let three_dot = tool
            .execute(json!({"action": "diff", "rev": "HEAD~1...HEAD"}), &ctx)
            .await
            .expect("three-dot diff");
        assert!(three_dot.success, "{}", three_dot.content);
        assert!(
            three_dot.content.contains("+three"),
            "{}",
            three_dot.content
        );

        let pwn_path = tmp.path().join("should_not_exist");
        let pwn_flag = format!("--output={}", pwn_path.display());
        for bad in [
            json!({"action": "diff", "rev": pwn_flag}),
            json!({"action": "diff", "rev": format!(" --output={}", pwn_path.display())}),
            json!({"action": "diff", "rev": format!("HEAD~1..--output={}", pwn_path.display())}),
            json!({"action": "diff", "rev": "HEAD\0--ext-diff"}),
            json!({"action": "diff", "base": "--ext-diff"}),
            json!({"action": "diff", "base": "HEAD~1", "target": "--output=/tmp/pwn"}),
            json!({"action": "diff", "base": "/etc/passwd", "target": "/etc/hosts"}),
            json!({"action": "diff", "target": "HEAD"}),
            json!({"action": "diff", "rev": "HEAD~1..HEAD", "base": "HEAD~1"}),
            json!({"action": "diff", "rev": "HEAD~1..HEAD", "cached": true}),
            json!({"action": "diff", "base": "HEAD~1", "target": "HEAD", "cached": true}),
        ] {
            let error = tool
                .execute(bad.clone(), &ctx)
                .await
                .expect_err(&format!("must reject {bad}"));
            assert!(
                matches!(error, ToolError::InvalidInput { .. }),
                "expected invalid_input for {bad}, got {error:?}"
            );
        }
        assert!(!pwn_path.exists());
    }

    #[tokio::test]
    async fn per_action_parameter_validation_rejects_cross_action_keys() {
        let schema = GitTool::new("Git").input_schema();
        let props = schema["properties"]
            .as_object()
            .expect("Git schema properties");
        for action in GitTool::ACTIONS {
            let allowed = GitTool::allowed_params(action)
                .unwrap_or_else(|| panic!("missing allowed_params for {action}"));
            for key in allowed {
                assert!(
                    props.contains_key(*key),
                    "allowed param {key} for {action} missing from schema"
                );
            }
        }

        let status_err = err(json!({"action": "status", "rev": "HEAD"})).await;
        assert!(status_err.contains("rev"), "{status_err}");
        assert!(status_err.contains("Allowed parameters"), "{status_err}");
        assert!(status_err.contains("path"), "{status_err}");

        let diff_err = err(json!({"action": "diff", "ours": "main", "theirs": "HEAD"})).await;
        assert!(diff_err.contains("ours, theirs"), "{diff_err}");
        assert!(diff_err.contains("rev, base, target"), "{diff_err}");

        let log_err = err(json!({"action": "log", "cached": true})).await;
        assert!(log_err.contains("cached"), "{log_err}");
        assert!(log_err.contains("max_count"), "{log_err}");
    }
}

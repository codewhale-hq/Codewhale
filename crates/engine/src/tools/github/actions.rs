//! What each action does once its input has cleared the guards in
//! [`super`]: read context for an issue or PR, post a comment, close a
//! thread.

use serde_json::Value;
use serde_json::json;

use crate::tools::spec::{
    ToolContext, ToolError, ToolResult, optional_bool, optional_str, required_str, required_u64,
};

use super::cli::{ensure_github_repo, git_status_porcelain, run_gh_json, run_gh_text};
use super::shape::{
    BODY_ARTIFACT_THRESHOLD, DIFF_ARTIFACT_THRESHOLD, artifact_refs_from_context,
    github_event_metadata, shape_large_text, summarize, write_artifact_if_needed,
};
use super::{GithubTool, validate_evidence};

impl GithubTool {
    pub(super) async fn execute_issue_context(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        ensure_github_repo(context)?;
        let number = required_u64(input, "number")?;
        let include_comments = optional_bool(input, "include_comments", true)?;
        let fields = if include_comments {
            "number,title,state,author,labels,assignees,milestone,body,comments,url,createdAt,updatedAt"
        } else {
            "number,title,state,author,labels,assignees,milestone,body,url,createdAt,updatedAt"
        };
        let number_s = number.to_string();
        let raw = run_gh_json(context, &["issue", "view", &number_s, "--json", fields])?;
        let shaped = shape_large_text(context, raw, "issue_body", BODY_ARTIFACT_THRESHOLD)?;
        let mut result = ToolResult::json(&json!({
            "summary": format!("Issue #{number}: {}", shaped["title"].as_str().unwrap_or("")),
            "issue": shaped,
        }))
        .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        let artifacts = artifact_refs_from_context(&result.content, "github_issue_body");
        if !artifacts.is_empty() {
            result = result.with_metadata(json!({ "task_updates": { "artifacts": artifacts } }));
        }
        Ok(result)
    }

    pub(super) async fn execute_pr_context(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        ensure_github_repo(context)?;
        let number = required_u64(input, "number")?;
        let number_s = number.to_string();
        let raw = run_gh_json(
            context,
            &[
                "pr",
                "view",
                &number_s,
                "--json",
                "number,title,state,author,body,comments,reviews,reviewDecision,statusCheckRollup,baseRefName,headRefName,headRefOid,baseRefOid,files,url,createdAt,updatedAt",
            ],
        )?;
        let mut shaped = shape_large_text(context, raw, "pr_body", BODY_ARTIFACT_THRESHOLD)?;
        if optional_bool(input, "include_diff", false)? {
            let diff = run_gh_text(context, &["pr", "diff", &number_s, "--patch"])?;
            let diff_ref =
                write_artifact_if_needed(context, "pr_diff", &diff, DIFF_ARTIFACT_THRESHOLD)?;
            shaped["diff_summary"] = json!(summarize(&diff, 900));
            shaped["diff_artifact"] = json!(diff_ref);
        }
        let mut result = ToolResult::json(&json!({
            "summary": format!("PR #{number}: {}", shaped["title"].as_str().unwrap_or("")),
            "pr": shaped,
        }))
        .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        let mut artifacts = artifact_refs_from_context(&result.content, "github_pr_body");
        artifacts.extend(artifact_refs_from_context(
            &result.content,
            "github_pr_diff",
        ));
        if !artifacts.is_empty() {
            result = result.with_metadata(json!({ "task_updates": { "artifacts": artifacts } }));
        }
        Ok(result)
    }

    pub(super) async fn execute_comment(
        &self,
        input: &Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        validate_evidence(input, false)?;
        let target = required_str(input, "target")?;
        // Exactly the schema's enum. Any other spelling ("PR",
        // "pull_request") used to fall through to an issue comment.
        let subcmd = match target {
            "issue" => "issue",
            "pr" => "pr",
            other => {
                return Err(ToolError::invalid_input(format!(
                    "github comment: target must be \"issue\" or \"pr\", got {other:?}; nothing was posted"
                )));
            }
        };
        let number = required_u64(input, "number")?;
        let body = required_str(input, "body")?;
        if optional_bool(input, "dry_run", false)? {
            return Ok(ToolResult::success(format!(
                "Dry run: would comment on {target} #{number}."
            )));
        }
        let number_s = number.to_string();
        run_gh_text(context, &[subcmd, "comment", &number_s, "--body", body])?;
        let metadata = github_event_metadata(
            "comment",
            target,
            number,
            summarize(body, 240),
            None,
            write_artifact_if_needed(context, "github_comment", body, BODY_ARTIFACT_THRESHOLD)?,
        );
        Ok(
            ToolResult::success(format!("Commented on {target} #{number}."))
                .with_metadata(metadata),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GithubCloseTarget {
    Issue,
    Pr,
}

impl GithubCloseTarget {
    fn cli_subcommand(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "pr",
        }
    }

    fn metadata_target(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "pr",
        }
    }

    pub(super) fn display(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "PR",
        }
    }

    fn summary_subject(self) -> &'static str {
        match self {
            Self::Issue => "Issue",
            Self::Pr => "PR",
        }
    }
}

pub(super) fn close_github_thread(
    input: Value,
    context: &ToolContext,
    target: GithubCloseTarget,
) -> Result<ToolResult, ToolError> {
    validate_evidence(&input, true)?;
    if !optional_bool(&input, "allow_dirty", false)? {
        let status = git_status_porcelain(context)?;
        if !status.trim().is_empty() {
            return Ok(ToolResult::error(format!(
                "Refusing to close {}: worktree is dirty and allow_dirty was false.",
                target.display()
            ))
            .with_metadata(json!({ "dirty_status": status })));
        }
    }
    let number = required_u64(&input, "number")?;
    if optional_bool(&input, "dry_run", false)? {
        return Ok(ToolResult::success(format!(
            "Dry run: would close {} #{number}.",
            target.display()
        )));
    }
    let subcmd = target.cli_subcommand();
    let number_s = number.to_string();
    if let Some(comment) = optional_str(&input, "comment")? {
        run_gh_text(context, &[subcmd, "comment", &number_s, "--body", comment])?;
    }
    let close_args: Vec<&str> = match target {
        GithubCloseTarget::Issue => vec!["issue", "close", &number_s, "--reason", "completed"],
        GithubCloseTarget::Pr => vec!["pr", "close", &number_s],
    };
    run_gh_text(context, &close_args)?;
    let metadata = github_event_metadata(
        "close",
        target.metadata_target(),
        number,
        format!(
            "{} closed as completed with structured evidence",
            target.summary_subject()
        ),
        None,
        optional_str(&input, "comment")?
            .and_then(|comment| {
                write_artifact_if_needed(
                    context,
                    "github_close_comment",
                    comment,
                    BODY_ARTIFACT_THRESHOLD,
                )
                .ok()
            })
            .flatten(),
    );
    Ok(
        ToolResult::success(format!("Closed {} #{number}.", target.display()))
            .with_metadata(metadata),
    )
}

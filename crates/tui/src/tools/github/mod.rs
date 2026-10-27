//! GitHub context and guarded write tools backed by the `gh` CLI.
//!
//! Unified surface (piagent phase B): the model sees one tool, `github`,
//! with an `action` parameter routing to the per-action logic. The legacy
//! `github_*` execution aliases were removed in v0.9.3.
//!
//! This file is the surface and its guards — which action a call names, and
//! whether the input is allowed to run it. The work itself is split by
//! responsibility: [`schema`] declares the input contracts, [`actions`] runs
//! the actions, [`cli`] builds every `gh`/`git` invocation, and [`shape`]
//! turns payloads into tool results.

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

mod actions;
mod cli;
pub(crate) mod host;
pub(crate) mod report;
mod schema;
mod shape;

use actions::{GithubCloseTarget, close_github_thread};
use schema::{canonical_schema, legacy_action_schema};

// The suite at the bottom of this file builds JSON inputs and a recorder path
// that nothing in the production surface above names.
#[cfg(test)]
use serde_json::json;
// Unix-only like the recorder helper that returns it (`install_recording_gh`)
// — on Windows the test binary compiles without them, and an ungated import
// fails `-D warnings`.
#[cfg(all(test, unix))]
use std::path::PathBuf;

/// Actions the Plan-mode read-only surface exposes.
const READ_ACTIONS: &[&str] = &["issue_context", "pr_context", "report_read"];
const ALL_ACTIONS: &[&str] = &[
    "issue_context",
    "pr_context",
    "comment",
    "close_issue",
    "close_pr",
    "report_draft",
    "report_read",
];

/// Unified GitHub tool.
///
/// One struct, one input schema per surface: the canonical `github` tool
/// (all actions, or the read-only subset via [`GithubTool::read_only`]) plus
/// hidden legacy aliases carrying a `forced_action`.
pub struct GithubTool {
    name: &'static str,
    forced_action: Option<&'static str>,
    read_only: bool,
}

impl GithubTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
            read_only: false,
        }
    }

    /// Plan-mode variant: only the read-only actions are advertised and routed.
    pub const fn read_only(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
            read_only: true,
        }
    }

    #[cfg(test)]
    pub const fn alias(name: &'static str, action: &'static str) -> Self {
        Self {
            name,
            forced_action: Some(action),
            read_only: false,
        }
    }

    fn allowed_actions(&self) -> &'static [&'static str] {
        if self.read_only {
            READ_ACTIONS
        } else {
            ALL_ACTIONS
        }
    }

    fn resolve_action<'a>(&'a self, input: &'a Value) -> Result<&'a str, ToolError> {
        let action = match self.forced_action {
            Some(action) => action,
            None => input.get("action").and_then(Value::as_str).ok_or_else(|| {
                ToolError::invalid_input(format!(
                    "github: missing `action` (one of: {})",
                    self.allowed_actions().join(", ")
                ))
            })?,
        };
        if self.allowed_actions().contains(&action) {
            Ok(action)
        } else {
            Err(ToolError::invalid_input(format!(
                "github: invalid action `{action}` (one of: {})",
                self.allowed_actions().join(", ")
            )))
        }
    }

    fn action_is_read(action: &str) -> bool {
        READ_ACTIONS.contains(&action)
    }
}

#[async_trait]
impl ToolSpec for GithubTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        self.forced_action.is_none()
    }

    fn description(&self) -> &'static str {
        match self.forced_action {
            Some("issue_context") => {
                "Read GitHub issue context using gh. Read-only: body/comments/labels/state are summarized and large bodies become task artifacts when a durable task is active."
            }
            Some("pr_context") => {
                "Read GitHub PR context using gh: body/comments/reviews/check status/files and optional diff artifact. Read-only; no push/merge/close."
            }
            Some("comment") => {
                "Post an evidence-backed GitHub issue/PR comment with gh. Requires approval. Use blocker comments for partial work; do not claim closure without evidence."
            }
            Some("close_issue") => {
                "Close a GitHub issue only when structured acceptance evidence is present and approved. Rejected when the worktree is dirty unless allow_dirty=true. For pull requests use github_close_pr; do not call PRs issues in user-facing output. Never close merely because the agent is stopping."
            }
            Some("close_pr") => {
                "Close a GitHub pull request only when structured acceptance evidence is present and approved. Rejected when the worktree is dirty unless allow_dirty=true. Use this for PRs instead of github_close_issue so the UI, audit trail, and comments keep PR wording clear."
            }
            _ if self.read_only => {
                "Read GitHub issue/PR context using gh (issue_context, pr_context), or read a local current-session Codewhale issue draft with report_read. Local report publication is unavailable."
            }
            _ => {
                "GitHub context (issue_context, pr_context) and guarded comment/close_issue/close_pr actions. Closes are rejected when the worktree is dirty unless allow_dirty=true. Also report_draft and report_read: save/revise/read a LOCAL structured Codewhale issue draft in this session, without network or publication. When you observe evidence of a likely Codewhale/runtime/tool defect, you may draft it yourself, separate observations from inferences, offer /feedback review, and continue the original task. Ordinary user-code failures alone are not Codewhale defects; avoid repeated reports. Include only bounded narrative evidence, never prompts, logs, private code, credentials or paths. report_draft revises an existing draft when revises is supplied; exact repeats converge. Draft publication and duplicate search are unavailable: do not use other tools to post the draft without separate explicit user authorization. No push/merge."
            }
        }
    }

    fn input_schema(&self) -> Value {
        if let Some(action) = self.forced_action {
            return legacy_action_schema(action);
        }
        canonical_schema(self.allowed_actions(), self.read_only)
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        match self.forced_action {
            Some(action) if Self::action_is_read(action) => {
                vec![ToolCapability::ReadOnly, ToolCapability::Network]
            }
            Some(_) => vec![ToolCapability::Network, ToolCapability::RequiresApproval],
            None if self.read_only => vec![ToolCapability::ReadOnly, ToolCapability::Network],
            None => vec![ToolCapability::Network, ToolCapability::RequiresApproval],
        }
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        match self.forced_action {
            Some(action) if Self::action_is_read(action) => ApprovalRequirement::Auto,
            Some(_) => ApprovalRequirement::Required,
            None if self.read_only => ApprovalRequirement::Auto,
            None => ApprovalRequirement::Required,
        }
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match self.resolve_action(input) {
            Ok("report_draft") => ApprovalRequirement::Auto,
            Ok(action) if Self::action_is_read(action) => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        match self.resolve_action(input) {
            Ok(action) => Self::action_is_read(action),
            Err(_) => self.is_read_only(),
        }
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = self.resolve_action(&input)?;
        if context
            .features
            .enabled(codewhale_runtime::features::Feature::GithubHost)
        {
            let request = host::Request::capture(action, &input, context)?;
            return crate::extension_host::manager()
                .execute_github(request, context)
                .await;
        }
        match action {
            "report_draft" => report::draft(input, context),
            "report_read" => report::read(input, context),
            "issue_context" => self.execute_issue_context(&input, context).await,
            "pr_context" => self.execute_pr_context(&input, context).await,
            "comment" => self.execute_comment(&input, context).await,
            "close_issue" => close_github_thread(input, context, GithubCloseTarget::Issue),
            "close_pr" => close_github_thread(input, context, GithubCloseTarget::Pr),
            action => Err(ToolError::invalid_input(format!(
                "github: invalid action `{action}`"
            ))),
        }
    }
}

fn validate_evidence(input: &Value, closing: bool) -> Result<(), ToolError> {
    let evidence = input
        .get("evidence")
        .and_then(Value::as_object)
        .ok_or_else(|| ToolError::invalid_input("evidence object is required"))?;
    if closing {
        let criteria = input
            .get("acceptance_criteria")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or_else(|| ToolError::invalid_input("acceptance_criteria must be non-empty"))?;
        if criteria
            .iter()
            .any(|item| item.as_str().unwrap_or("").trim().is_empty())
        {
            return Err(ToolError::invalid_input(
                "acceptance_criteria entries must be non-empty",
            ));
        }
        for key in ["files_changed", "tests_run", "final_status"] {
            if !evidence.contains_key(key) {
                return Err(ToolError::invalid_input(format!(
                    "closure evidence missing {key}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolSpec;

    #[test]
    fn close_refuses_when_git_cannot_determine_worktree_status() {
        use codewhale_dependencies::{ExternalTool, Git};

        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            Git::output(&["init", "-q"], tmp.path())
                .unwrap()
                .status
                .success()
        );
        let context = ToolContext::new(tmp.path());
        assert!(
            super::cli::git_status_porcelain(&context)
                .unwrap()
                .is_empty()
        );
        std::fs::write(tmp.path().join("work.txt"), "uncommitted work").unwrap();
        assert!(
            !super::cli::git_status_porcelain(&context)
                .unwrap()
                .is_empty()
        );
        // A real Git failure can have empty stdout; that is not a clean tree.
        std::fs::write(tmp.path().join(".git/index"), "invalid index").unwrap();
        let input = json!({
            "number": 424_242,
            "dry_run": true,
            "acceptance_criteria": ["done"],
            "evidence": {
                "files_changed": ["work.txt"],
                "tests_run": ["local fixture"],
                "final_status": "green"
            }
        });
        let error = close_github_thread(input, &context, GithubCloseTarget::Issue)
            .expect_err("unknown worktree state must not authorize closure")
            .to_string();
        assert!(error.contains("git status failed"), "{error}");
        assert!(error.contains("cannot verify"), "{error}");
    }

    #[test]
    fn close_schema_requires_structured_evidence() {
        let schema = GithubTool::alias("github_close_issue", "close_issue").input_schema();
        assert!(
            schema["properties"]["evidence"]["required"]
                .as_array()
                .expect("required")
                .contains(&json!("tests_run"))
        );
    }

    #[test]
    fn close_pr_schema_requires_structured_evidence() {
        let schema = GithubTool::alias("github_close_pr", "close_pr").input_schema();
        assert!(
            schema["properties"]["evidence"]["required"]
                .as_array()
                .expect("required")
                .contains(&json!("tests_run"))
        );
    }

    #[test]
    fn close_tools_distinguish_issue_and_pr_wording() {
        assert_eq!(GithubCloseTarget::Issue.display(), "issue");
        assert_eq!(GithubCloseTarget::Pr.display(), "PR");
        assert!(
            GithubTool::alias("github_close_issue", "close_issue")
                .description()
                .contains("github_close_pr")
        );
        assert!(
            GithubTool::alias("github_close_pr", "close_pr")
                .description()
                .contains("pull request")
        );
    }

    // The dirty-worktree refusal is easy to hit on a real close; the
    // model-facing text must disclose it and the flag that overrides it.
    #[test]
    fn close_disclosure_names_the_dirty_worktree_refusal() {
        for description in [
            GithubTool::alias("github_close_issue", "close_issue").description(),
            GithubTool::alias("github_close_pr", "close_pr").description(),
            GithubTool::new("github").description(),
        ] {
            assert!(
                description.contains("dirty") && description.contains("allow_dirty"),
                "close descriptions must disclose the dirty-worktree refusal: {description}"
            );
        }
        let schema = GithubTool::new("github").input_schema();
        let allow_dirty = schema["properties"]["allow_dirty"]["description"]
            .as_str()
            .expect("allow_dirty must carry a description");
        assert!(
            allow_dirty.contains("rejected when the worktree is dirty"),
            "allow_dirty must disclose the refusal it overrides: {allow_dirty}"
        );
    }

    /// D03-03: only the schema's `issue`/`pr` select a thread kind. Any other
    /// spelling is refused before `gh` runs instead of commenting on an issue.
    #[tokio::test]
    async fn comment_refuses_a_target_outside_the_schema_enum() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let context = ToolContext::new(tmp.path());
        let tool = GithubTool::new("github");
        for target in ["PR", "pull_request", "pull"] {
            let error = tool
                .execute(
                    json!({
                        "action": "comment",
                        "target": target,
                        "number": 7,
                        "body": "evidence",
                        "evidence": {},
                        "dry_run": true
                    }),
                    &context,
                )
                .await
                .expect_err("an unknown target must not resolve to an issue")
                .to_string();
            assert!(error.contains("target must be"), "{target}: {error}");
        }
        for target in ["issue", "pr"] {
            let result = tool
                .execute(
                    json!({
                        "action": "comment",
                        "target": target,
                        "number": 7,
                        "body": "evidence",
                        "evidence": {},
                        "dry_run": true
                    }),
                    &context,
                )
                .await
                .expect("schema targets stay valid");
            assert!(result.content.contains(&format!("{target} #7")));
        }
    }

    #[test]
    fn missing_close_evidence_refuses() {
        let input = json!({
            "number": 1,
            "acceptance_criteria": ["done"],
            "evidence": { "files_changed": [] }
        });
        let err = validate_evidence(&input, true).expect_err("should refuse");
        assert!(err.to_string().contains("tests_run"));
    }

    #[test]
    fn canonical_schema_lists_all_actions() {
        let schema = GithubTool::new("github").input_schema();
        let actions = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        for action in [
            "issue_context",
            "pr_context",
            "comment",
            "close_issue",
            "close_pr",
        ] {
            assert!(
                actions.iter().any(|value| value.as_str() == Some(action)),
                "canonical schema must offer action {action}"
            );
        }
        for field in [
            "number",
            "target",
            "body",
            "evidence",
            "acceptance_criteria",
        ] {
            assert!(
                schema["properties"][field].is_object(),
                "canonical schema must carry union field {field}"
            );
        }
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn read_only_variant_only_offers_read_actions() {
        let tool = GithubTool::read_only("github");
        let schema = tool.input_schema();
        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["issue_context", "pr_context", "report_read"])
        );
        assert!(!schema["properties"]["body"].is_object());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
        assert!(tool.is_read_only());
    }

    #[test]
    fn aliases_hide_from_model_and_force_action() {
        let comment = GithubTool::alias("github_comment", "comment");
        assert!(!comment.model_visible());
        assert_eq!(comment.name(), "github_comment");
        assert_eq!(
            comment.approval_requirement(),
            ApprovalRequirement::Required
        );
        assert!(comment.capabilities().contains(&ToolCapability::Network));

        let issue = GithubTool::alias("github_issue_context", "issue_context");
        assert_eq!(issue.approval_requirement(), ApprovalRequirement::Auto);
        assert!(issue.is_read_only_for(&json!({})));

        let canonical = GithubTool::new("github");
        assert!(canonical.model_visible());
        assert_eq!(
            canonical.approval_requirement_for(&json!({"action": "pr_context"})),
            ApprovalRequirement::Auto
        );
        assert_eq!(
            canonical.approval_requirement_for(&json!({"action": "close_pr"})),
            ApprovalRequirement::Required
        );
        assert!(canonical.is_read_only_for(&json!({"action": "issue_context"})));
        assert!(!canonical.is_read_only_for(&json!({"action": "comment"})));
    }

    #[test]
    fn canonical_rejects_unknown_or_missing_action() {
        let tool = GithubTool::new("github");
        let err = tool
            .resolve_action(&json!({}))
            .expect_err("missing action must fail");
        assert!(err.to_string().contains("missing `action`"));
        let err = tool
            .resolve_action(&json!({"action": "merge"}))
            .expect_err("unknown action must fail");
        assert!(err.to_string().contains("invalid action"));

        let read_only = GithubTool::read_only("github");
        let err = read_only
            .resolve_action(&json!({"action": "close_pr"}))
            .expect_err("read-only surface must reject write actions");
        assert!(err.to_string().contains("invalid action"));
    }

    /// Install a `gh` stand-in that appends its argv to `log` and succeeds.
    ///
    /// The close path must never reach a real `gh`, so the recorder both
    /// proves what was attempted and keeps the test from touching GitHub.
    /// Unix-only like its consumers: the recorder is a `sh` script, and on
    /// Windows the ungated helper is dead code that fails `-D warnings` —
    /// this was the unexplained red `Test (windows-latest)` on #5135.
    #[cfg(unix)]
    fn install_recording_gh(dir: &std::path::Path, log: &std::path::Path) -> PathBuf {
        let bin = dir.join("gh-recorder.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .expect("write recorder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
                .expect("chmod recorder");
        }
        bin
    }

    #[cfg(unix)]
    fn close_input_with_dry_run(dry_run: Value) -> Value {
        json!({
            "number": 424_242,
            "allow_dirty": true,
            "dry_run": dry_run,
            "acceptance_criteria": ["done"],
            "evidence": {
                "files_changed": ["src/lib.rs"],
                "tests_run": ["cargo test"],
                "final_status": "green"
            }
        })
    }

    #[test]
    #[cfg(unix)]
    fn stringy_dry_run_never_closes_the_thread() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log = tmp.path().join("gh-calls.log");
        let bin = install_recording_gh(tmp.path(), &log);
        let ctx = ToolContext::new(tmp.path());

        let _env = crate::test_support::lock_test_env();
        // SAFETY: serialized behind the process-wide test env lock.
        unsafe {
            std::env::set_var("CODEWHALE_GH_BIN", &bin);
        }
        let result = close_github_thread(
            close_input_with_dry_run(json!("true")),
            &ctx,
            GithubCloseTarget::Issue,
        );
        // SAFETY: same lock; restores the process environment.
        unsafe {
            std::env::remove_var("CODEWHALE_GH_BIN");
        }

        let invocations = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            invocations.is_empty(),
            "a stringy dry_run must not invoke gh at all; got: {invocations}"
        );
        let err = result.expect_err("dry_run must not be silently coerced to its default");
        let err = err.to_string();
        assert!(err.contains("dry_run"), "error must name the field: {err}");
        assert!(
            err.contains("boolean") && err.contains("string"),
            "error must name expected and received types: {err}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn real_dry_run_bool_still_short_circuits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log = tmp.path().join("gh-calls.log");
        let bin = install_recording_gh(tmp.path(), &log);
        let ctx = ToolContext::new(tmp.path());

        let _env = crate::test_support::lock_test_env();
        // SAFETY: serialized behind the process-wide test env lock.
        unsafe {
            std::env::set_var("CODEWHALE_GH_BIN", &bin);
        }
        let result = close_github_thread(
            close_input_with_dry_run(json!(true)),
            &ctx,
            GithubCloseTarget::Issue,
        );
        // SAFETY: same lock; restores the process environment.
        unsafe {
            std::env::remove_var("CODEWHALE_GH_BIN");
        }

        let result = result.expect("a real bool dry_run stays a dry run");
        assert!(result.success);
        assert!(result.content.contains("Dry run"), "{}", result.content);
        assert!(
            std::fs::read_to_string(&log).unwrap_or_default().is_empty(),
            "dry run must not invoke gh"
        );
    }
}

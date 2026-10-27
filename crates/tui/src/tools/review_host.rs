//! Captured review presentation. Inputs, provider budget and publication remain Core-owned.
use std::time::Duration;

use serde_json::{Value, json};

use super::review::{PrReviewPlan, ReviewOutput};
use super::review_pr::GhPullRequest;
use super::spec::{ToolContext, ToolError};
use crate::extension_host::StockOperation;
use codewhale_runtime::features::Feature;

pub(crate) fn view_snapshot(view: &GhPullRequest) -> Value {
    json!({"title":view.title,"body":view.body,"base":view.base,"head":view.head,"url":view.url,"head_sha":view.head_sha,"base_sha":view.base_sha,"changed_files":view.changed_files})
}
pub(crate) fn sort_json_keys() -> bool {
    json!({"z":null,"a":null})
        .as_object()
        .expect("object")
        .keys()
        .next()
        .is_some_and(|key| key == "a")
}
async fn project(
    operation: StockOperation,
    input: Value,
    context: &ToolContext,
) -> Result<String, ToolError> {
    if !context.features.enabled(Feature::ReviewHost) {
        return Err(ToolError::not_available(
            "Review Host backend is not selected",
        ));
    }
    let mut budget = Duration::from_secs(30);
    if let Some(deadline) = context.turn_deadline {
        budget = budget.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
    }
    let result = crate::extension_host::manager()
        .execute_stock(operation, input, context, budget)
        .await?;
    if !result.success
        || result
            .metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.is_null())
    {
        return Err(ToolError::execution_failed(
            "Review presenter returned an unowned outcome",
        ));
    }
    Ok(result.content)
}
pub(crate) async fn source_prompt(
    snapshot: Value,
    context: &ToolContext,
) -> Result<String, ToolError> {
    project(StockOperation::ReviewSourcePrompt, snapshot, context).await
}
pub(crate) async fn cli_prompt(
    diff: &str,
    commits: &str,
    context: &ToolContext,
) -> Result<String, ToolError> {
    source_prompt(
        json!({"kind":"cli_diff","diff":diff,"commits":commits}),
        context,
    )
    .await
}
pub(crate) async fn pr_prompts(
    number: u32,
    view: &GhPullRequest,
    plan: &PrReviewPlan,
    context: &ToolContext,
) -> Result<Vec<String>, ToolError> {
    if !context.features.enabled(Feature::ReviewHost) {
        return super::review::build_pr_review_prompts(number, view, plan, &context.workspace)
            .await
            .map_err(|error| ToolError::execution_failed(error.to_string()));
    }
    let view = std::sync::Arc::new(view.clone());
    let plan = std::sync::Arc::new(plan.clone());
    let mut prompts = Vec::with_capacity(plan.passes.len());
    for index in 0..plan.passes.len() {
        let admission = crate::extension_host::manager().admit_review_capture()?;
        let (view, plan, workspace) = (
            std::sync::Arc::clone(&view),
            std::sync::Arc::clone(&plan),
            context.workspace.clone(),
        );
        // Reuse Core's pinned parser and existing permit-retaining worker.
        // Capture one pass at a time; no extra catalog of source snapshots.
        let snapshot = super::github::host::report_worker(admission, move || {
            Ok(super::review::capture_pr_pass_snapshot(
                number,
                &view,
                &plan,
                &plan.passes[index],
                &workspace,
            ))
        })
        .await?;
        prompts.push(project(StockOperation::ReviewPassPrompt, snapshot, context).await?);
    }
    Ok(prompts)
}
pub(crate) async fn interactive(
    number: u32,
    view: &GhPullRequest,
    diff: &str,
    context: &ToolContext,
) -> Result<String, ToolError> {
    project(StockOperation::ReviewInteractivePr, json!({"number":number,"view":view_snapshot(view),"diff":super::review_pr::model_diff(diff)}), context).await
}
pub(crate) async fn report(
    review: Option<&ReviewOutput>,
    output: &str,
    posted: bool,
    context: &ToolContext,
) -> Result<String, ToolError> {
    project(
        StockOperation::ReviewReport,
        json!({"review":review,"output":output,"posted":posted}),
        context,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_dependencies::ExternalTool;
    #[tokio::test]
    async fn disabled_review_host_refuses_before_demand_or_provider() {
        let temp = tempfile::tempdir().unwrap();
        let context = ToolContext::new(temp.path());
        assert!(matches!(
            cli_prompt("diff", "", &context).await,
            Err(ToolError::NotAvailable { .. })
        ));
    }
    #[test]
    fn review_view_snapshot_carries_pinned_public_facts_without_additional_authority() {
        let view = GhPullRequest {
            title: "untrusted title".into(),
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            ..Default::default()
        };
        let snapshot = view_snapshot(&view);
        assert_eq!(snapshot["head_sha"], view.head_sha);
        assert_eq!(snapshot["title"], view.title);
        assert!(snapshot.get("token").is_none());
        assert!(snapshot.get("workspace").is_none());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn actual_review_pass_host_reuses_pinned_context_and_partial_plan_exactly() {
        let _home = crate::test_support::SealedHome::new();
        let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(false);
        let Some(node) = crate::extension_host::tests::node_for_tests("review_pass_context_parity")
        else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::extension_host::ExtensionHostManager::new(
            crate::extension_host::ExtensionHostOptions {
                runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
                node_override: Some(node),
                root: Some(root.path().join("host")),
                ..Default::default()
            },
        ));
        let _manager =
            crate::extension_host::TestManagerGuard::install(std::sync::Arc::clone(&manager));
        let git = |args: &[&str]| {
            let output = codewhale_dependencies::Git::command()
                .unwrap()
                .args(args)
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        let hooks = root.path().join("no-hooks");
        std::fs::create_dir(&hooks).unwrap();
        git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        git(&["config", "user.name", "Codewhale Test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        let original = format!(
            "fn existing() {{\n{}\n}}\n",
            (1..=80)
                .map(|i| format!("    // line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::fs::write(root.path().join("x.rs"), &original).unwrap();
        git(&["config", "commit.gpgsign", "false"]);
        git(&["add", "x.rs"]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(
            root.path().join("x.rs"),
            original.replace("// line 40", "panic!(\"introduced\");"),
        )
        .unwrap();
        git(&["add", "x.rs"]);
        git(&["commit", "-qm", "head"]);
        let head = git(&["rev-parse", "HEAD"]);
        let patch = git(&["diff", "&BASE", "&HEAD", "--", "x.rs"].map(|x| {
            if x == "&BASE" {
                base.as_str()
            } else if x == "&HEAD" {
                head.as_str()
            } else {
                x
            }
        }));
        let oversized = format!(
            "diff --git a/too-big b/too-big\n--- /dev/null\n+++ b/too-big\n@@ -0,0 +1 @@\n+{}\n",
            "x".repeat(30_000)
        );
        let diff = format!("{patch}\n{oversized}");
        let view = GhPullRequest {
            head_sha: head,
            base_sha: base,
            title: "untrusted ```suggestion".into(),
            body: "漢字 description".into(),
            changed_files: 2,
            ..Default::default()
        };
        let plan = super::super::review::plan_pr_review(&diff, &view, 20_000, 1).unwrap();
        assert_eq!(plan.manifest.skipped_files.len(), 1);
        let snapshot = super::super::review::capture_pr_pass_snapshot(
            7,
            &view,
            &plan,
            &plan.passes[0],
            root.path(),
        );
        assert!(
            !snapshot["context"].is_null(),
            "the same pinned source collector must actually run"
        );
        assert_eq!(snapshot["context"]["unavailable_files"], 0);
        assert!(!snapshot["context"]["files"].as_array().unwrap().is_empty());
        let expected = super::super::review::build_pr_pass_prompt(
            7,
            &view,
            &plan,
            &plan.passes[0],
            root.path(),
        );
        let mut flags = codewhale_runtime::features::Features::with_defaults();
        flags.enable(Feature::ReviewHost);
        let context = ToolContext::new(root.path()).with_features(flags);
        let prompts = pr_prompts(7, &view, &plan, &context).await.unwrap();
        assert_eq!(prompts, vec![expected]);
        let value: Value = serde_json::from_str(&prompts[0]).unwrap();
        assert_eq!(
            value["manifest"],
            serde_json::to_value(&plan.manifest).unwrap()
        );
        assert!(
            value["task"]
                .as_str()
                .unwrap()
                .contains("Do not claim full coverage.")
        );
        manager.shutdown().await;
    }
}

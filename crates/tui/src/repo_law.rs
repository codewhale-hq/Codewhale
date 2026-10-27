//! Mechanical enforcement of repo-law protected invariants.
//!
//! `.codewhale/constitution.json` invariants were previously advisory prose
//! rendered into the prompt. Entries that carry `paths` globs now also
//! compile into write holds evaluated in the engine's tool gate — the law
//! becomes mechanism, with a receipt naming the invariant.
//!
//! The contract mirrors the project-overlay rule ("overrides may only
//! tighten"):
//!
//! - Law can only ADD holds. There is no allow/widen shape in the schema, so
//!   a crafted constitution cannot grant authority.
//! - `ask` force-prompts only in Ask posture. Auto-Review, Full Access, and
//!   Never never open tool-approval prompts, so the same law fails closed
//!   there. `block` denies outright in every posture.
//! - No constitution (or an empty one) means no holds. A constitution that
//!   exists but cannot be read or parsed, or an enforced invariant whose glob
//!   does not compile, holds every write (`ask`) naming the problem: the law
//!   the file meant to state is unknown, and enforcing less than it says
//!   would fail open. Fixing the file releases the hold.
//! - Only the repo-local constitution participates. The user-global
//!   constitution stays advisory prose and never reaches this module.

use std::path::Path;

use serde_json::Value;

use crate::project_context::{RepoLawAction, RepoLawRule, load_repo_law_rules};
use codewhale_tools::patch_preflight::{NormalizedApplyPatchInput, normalize_apply_patch_input};

/// Semantic write actions whose inputs name filesystem targets we can hold.
/// Canonical action families are resolved to this policy vocabulary before the
/// check, so removing callable compatibility aliases cannot open a law bypass.
const WRITE_POLICY_ACTIONS: &[&str] = &["write_file", "edit_file", "apply_patch", "fim_edit"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoLawPlanDecision {
    /// Request a policy-forced approval naming the law. The engine converts
    /// this to a hard block in non-interactive Full Access.
    ForcePrompt(String),
    /// Deny the call outright, naming the law.
    Block(String),
}

/// Evaluate the workspace's repo law against a proposed tool call. Returns
/// `None` for tools without write targets, workspaces without enforceable
/// law, and writes outside every protected glob.
pub(crate) fn repo_law_plan_decision(
    workspace: &Path,
    tool_name: &str,
    tool_input: &Value,
) -> Option<RepoLawPlanDecision> {
    let policy_action =
        codewhale_execpolicy::canonical_action::canonical_action_alias(tool_name, tool_input);
    if !WRITE_POLICY_ACTIONS.contains(&policy_action) {
        return None;
    }
    let targets = write_target_paths(workspace, tool_input);
    if targets.is_empty() {
        return None;
    }
    let rules = match load_repo_law_rules(workspace) {
        Ok(rules) => rules,
        Err(problem) => {
            return Some(RepoLawPlanDecision::ForcePrompt(format!(
                "Repo law holds every write until .codewhale/constitution.json is fixed: {problem}"
            )));
        }
    };
    if rules.is_empty() {
        return None;
    }

    // Strongest action wins across all (rule, target) matches.
    let mut hold: Option<(&RepoLawRule, &str)> = None;
    for rule in &rules {
        for target in &targets {
            if rule.globs.is_match(target) {
                let stronger = matches!(rule.action, RepoLawAction::Block) || hold.is_none();
                let already_blocking = hold
                    .as_ref()
                    .is_some_and(|(held, _)| matches!(held.action, RepoLawAction::Block));
                if stronger && !already_blocking {
                    hold = Some((rule, target.as_str()));
                }
            }
        }
    }
    let (rule, target) = hold?;
    let protects = rule.patterns.join(", ");
    let reason = format!(
        "Repo law holds this write: \"{}\" protects {protects} (matched {target}, .codewhale/constitution.json)",
        rule.text
    );
    Some(match rule.action {
        RepoLawAction::Ask => RepoLawPlanDecision::ForcePrompt(reason),
        RepoLawAction::Block => RepoLawPlanDecision::Block(reason),
    })
}

/// Extract workspace-relative write targets from a tool input. Covers the
/// `path`/`target`/`destination` params and every path alias the file tools
/// fold onto `path` (`file_path`, `filePath`), canonical
/// `replace[].path`, legacy `changes[].path`, and
/// every unified-diff / codex-envelope header shape the patch tools accept —
/// old (`--- `) and new (`+++ `) paths, with or without an `a/`/`b/` prefix,
/// tab-timestamp suffixes stripped, and `/dev/null` (deletion) falling back
/// to the counterpart path. Missing any shape the tool honors is a hold
/// bypass, so this deliberately over-collects candidate paths.
fn write_target_paths(workspace: &Path, input: &Value) -> Vec<String> {
    let mut targets = Vec::new();
    for key in codewhale_tools::param_alias::path_argument_keys().chain(["target", "destination"]) {
        if let Some(path) = input.get(key).and_then(Value::as_str) {
            push_normalized(&mut targets, workspace, path);
        }
    }
    match normalize_apply_patch_input(input) {
        Ok(NormalizedApplyPatchInput::Replacement { entries, .. }) => {
            for change in entries {
                if let Some(path) = change.get("path").and_then(Value::as_str) {
                    push_normalized(&mut targets, workspace, path);
                }
            }
        }
        Ok(NormalizedApplyPatchInput::Patch(patch)) => {
            let mut pending_old: Option<String> = None;
            for line in patch.lines() {
                if let Some(rest) = line.strip_prefix("*** Update File: ") {
                    push_normalized(&mut targets, workspace, rest.trim());
                } else if let Some(rest) = line.strip_prefix("*** Add File: ") {
                    push_normalized(&mut targets, workspace, rest.trim());
                } else if let Some(rest) = line.strip_prefix("*** Delete File: ") {
                    push_normalized(&mut targets, workspace, rest.trim());
                } else if let Some(rest) = line.strip_prefix("--- ") {
                    // Old path: remember it so a `+++ /dev/null` deletion still
                    // holds the file being removed.
                    pending_old = diff_header_path(rest);
                    if let Some(ref p) = pending_old {
                        push_normalized(&mut targets, workspace, p);
                    }
                } else if let Some(rest) = line.strip_prefix("+++ ") {
                    match diff_header_path(rest) {
                        Some(new_path) => push_normalized(&mut targets, workspace, &new_path),
                        // `+++ /dev/null` → deletion; the target is the old path.
                        None => {
                            if let Some(old) = pending_old.take() {
                                push_normalized(&mut targets, workspace, &old);
                            }
                        }
                    }
                }
            }
        }
        Err(_) => {}
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Parse a unified-diff header path: strip an optional `a/`/`b/` prefix and a
/// tab-delimited timestamp suffix. Returns `None` for `/dev/null` (absence).
fn diff_header_path(rest: &str) -> Option<String> {
    // Headers may carry a "\t<timestamp>" suffix; the path is the first field.
    let path = rest.split('\t').next().unwrap_or(rest).trim();
    if path.is_empty() || path == "/dev/null" {
        return None;
    }
    let stripped = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    Some(stripped.to_string())
}

/// Normalize to a forward-slash, workspace-relative string so globs written
/// as `crates/x/**` match regardless of how the tool spelled the path. Crucially
/// this collapses `.`/`..` path components the same way the write tools'
/// `resolve_path` does, so an interior `crates/./protocol/x` or
/// `x/../crates/protocol/x` cannot spell its way past a glob (a confirmed
/// bypass before this).
fn push_normalized(targets: &mut Vec<String>, workspace: &Path, raw: &str) {
    let trimmed = raw.trim().replace('\\', "/");
    if trimmed.is_empty() {
        return;
    }
    // Make workspace-relative when the tool gave an absolute path inside it.
    let path = Path::new(&trimmed);
    let relative = path.strip_prefix(workspace).unwrap_or(path);

    // Lexically collapse CurDir (`.`) and ParentDir (`..`) components, and
    // drop any leading root/empty component. An absolute path outside the
    // workspace keeps its tail (e.g. `/etc/passwd` -> `etc/passwd`) so a
    // `**/passwd` glob still matches while a workspace-anchored glob does not.
    let mut parts: Vec<String> = Vec::new();
    for component in relative.to_string_lossy().split('/') {
        match component {
            "" | "." => {}
            ".." => {
                // A `..` that pops above the root escapes the workspace; keep
                // an explicit marker so it can never match a workspace-relative
                // glob, and the ordinary approval/sandbox gates still govern it.
                if parts.pop().is_none() {
                    parts.push("..".to_string());
                }
            }
            other => parts.push(other.to_string()),
        }
    }
    let normalized = parts.join("/");
    if !normalized.is_empty() {
        targets.push(normalized);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_law(workspace: &Path, body: &str) {
        let dir = workspace.join(".codewhale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("constitution.json"), body).unwrap();
    }

    const LAW: &str = r#"{
        "authority": ["AGENTS.md"],
        "protected_invariants": [
            "Keep DeepSeek support first-class.",
            { "text": "The wire format is frozen", "paths": ["crates/protocol/**"], "action": "block" },
            { "text": "Release notes need human review", "paths": ["CHANGELOG.md"] }
        ]
    }"#;

    #[test]
    fn advisory_only_law_never_holds() {
        let tmp = TempDir::new().unwrap();
        write_law(
            tmp.path(),
            r#"{"protected_invariants": ["Prose only, no paths."]}"#,
        );
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            ),
            None
        );
    }

    #[test]
    fn block_action_denies_protected_write() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "crates/protocol/wire.rs", "content": "x"}),
        );
        let Some(RepoLawPlanDecision::Block(reason)) = decision else {
            panic!("expected block, got {decision:?}");
        };
        assert!(reason.contains("The wire format is frozen"), "{reason}");
        assert!(reason.contains("crates/protocol/wire.rs"), "{reason}");
        assert!(reason.contains(".codewhale/constitution.json"), "{reason}");
    }

    #[test]
    fn ask_action_force_prompts_and_names_the_law() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let decision = repo_law_plan_decision(
            tmp.path(),
            "edit_file",
            &json!({"path": "CHANGELOG.md", "old": "a", "new": "b"}),
        );
        let Some(RepoLawPlanDecision::ForcePrompt(reason)) = decision else {
            panic!("expected force prompt, got {decision:?}");
        };
        assert!(
            reason.contains("Release notes need human review"),
            "{reason}"
        );
    }

    #[test]
    fn canonical_file_write_and_edit_actions_receive_the_same_holds() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);

        let blocked = repo_law_plan_decision(
            tmp.path(),
            "File",
            &json!({
                "action": "write",
                "path": "crates/protocol/wire.rs",
                "content": "x"
            }),
        );
        assert!(matches!(blocked, Some(RepoLawPlanDecision::Block(_))));

        let held = repo_law_plan_decision(
            tmp.path(),
            "File",
            &json!({
                "action": "edit",
                "path": "CHANGELOG.md",
                "search": "before",
                "replace": "after"
            }),
        );
        assert!(matches!(held, Some(RepoLawPlanDecision::ForcePrompt(_))));
    }

    #[test]
    fn path_alias_spellings_receive_the_same_holds() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        for (tool, input) in [
            (
                "write_file",
                json!({"filePath": "crates/protocol/wire.rs", "content": "x"}),
            ),
            (
                "write_file",
                json!({"file_path": "crates/protocol/wire.rs", "content": "x"}),
            ),
            (
                "File",
                json!({"action": "edit", "filePath": "crates/protocol/wire.rs", "search": "a", "replace": "b"}),
            ),
        ] {
            let decision = repo_law_plan_decision(tmp.path(), tool, &input);
            assert!(
                matches!(decision, Some(RepoLawPlanDecision::Block(_))),
                "{tool} {input}: {decision:?}"
            );
        }
    }

    #[test]
    fn unprotected_writes_and_non_write_tools_pass() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            ),
            None
        );
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "read_file",
                &json!({"path": "crates/protocol/wire.rs"}),
            ),
            None
        );
    }

    #[test]
    fn apply_patch_targets_are_extracted_from_all_shapes() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        // Canonical replace[].path shape.
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"replace": [{"path": "crates/protocol/msg.rs"}]}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        // Legacy changes[].path shape must receive the same hold.
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"changes": [{"path": "crates/protocol/msg.rs"}]}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        // unified diff shape
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"patch": "--- a/crates/protocol/msg.rs\n+++ b/crates/protocol/msg.rs\n@@\n"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        // codex envelope shape
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"patch": "*** Begin Patch\n*** Update File: crates/protocol/msg.rs\n*** End Patch\n"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
    }

    #[test]
    fn block_outranks_ask_when_both_match() {
        let tmp = TempDir::new().unwrap();
        write_law(
            tmp.path(),
            r#"{"protected_invariants": [
                { "text": "ask first", "paths": ["docs/**"] },
                { "text": "never", "paths": ["docs/frozen/**"], "action": "block" }
            ]}"#,
        );
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "docs/frozen/spec.md", "content": "x"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
    }

    #[test]
    fn absolute_and_dot_prefixed_paths_normalize_to_workspace_relative() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let absolute = tmp.path().join("crates/protocol/wire.rs");
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": absolute.to_string_lossy(), "content": "x"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "./CHANGELOG.md", "content": "x"}),
        );
        assert!(matches!(
            decision,
            Some(RepoLawPlanDecision::ForcePrompt(_))
        ));
    }

    #[test]
    fn malformed_law_and_bad_globs_hold_every_write() {
        let tmp = TempDir::new().unwrap();
        let write = json!({"path": "src/unrelated.rs", "content": "x"});
        write_law(tmp.path(), "{ not json");
        let Some(RepoLawPlanDecision::ForcePrompt(reason)) =
            repo_law_plan_decision(tmp.path(), "write_file", &write)
        else {
            panic!("an unparseable constitution must hold writes");
        };
        assert!(reason.contains("constitution.json"), "{reason}");
        write_law(
            tmp.path(),
            r#"{"protected_invariants": [
                { "text": "broken glob", "paths": ["crates/[invalid"] }
            ]}"#,
        );
        let Some(RepoLawPlanDecision::ForcePrompt(reason)) =
            repo_law_plan_decision(tmp.path(), "write_file", &write)
        else {
            panic!("an invariant whose glob does not compile must hold writes");
        };
        assert!(reason.contains("crates/[invalid"), "{reason}");
        // Non-write tools stay unaffected, and an empty file is no law.
        assert_eq!(
            repo_law_plan_decision(tmp.path(), "read_file", &json!({"path": "a.rs"})),
            None
        );
        write_law(tmp.path(), "  \n");
        assert_eq!(
            repo_law_plan_decision(tmp.path(), "write_file", &write),
            None
        );
    }

    #[test]
    fn interior_dot_and_parent_segments_cannot_evade_a_block() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        for path in [
            "crates/./protocol/wire.rs",
            "crates/../crates/protocol/wire.rs",
            "x/../crates/protocol/wire.rs",
            "./crates/protocol/wire.rs",
        ] {
            let decision = repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({ "path": path, "content": "x" }),
            );
            assert!(
                matches!(decision, Some(RepoLawPlanDecision::Block(_))),
                "{path} must be held, got {decision:?}"
            );
        }
    }

    #[test]
    fn fim_edit_is_gated_like_other_write_tools() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let decision = repo_law_plan_decision(
            tmp.path(),
            "fim_edit",
            &json!({ "path": "crates/protocol/wire.rs", "prefix": "a", "suffix": "b" }),
        );
        assert!(
            matches!(decision, Some(RepoLawPlanDecision::Block(_))),
            "{decision:?}"
        );
    }

    #[test]
    fn apply_patch_header_variants_are_all_extracted() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        // no a/ or b/ prefix
        let d = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({ "patch": "--- crates/protocol/wire.rs\n+++ crates/protocol/wire.rs\n@@\n" }),
        );
        assert!(
            matches!(d, Some(RepoLawPlanDecision::Block(_))),
            "no-prefix: {d:?}"
        );
        // deletion: +++ /dev/null, target is the old path
        let d = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({ "patch": "--- a/crates/protocol/wire.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n" }),
        );
        assert!(
            matches!(d, Some(RepoLawPlanDecision::Block(_))),
            "deletion: {d:?}"
        );
        // tab-timestamp suffix on the header
        let d = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({ "patch": "--- a/x\t2026-01-01\n+++ b/crates/protocol/wire.rs\t2026-01-01 10:00:00\n@@\n" }),
        );
        assert!(
            matches!(d, Some(RepoLawPlanDecision::Block(_))),
            "tab-timestamp: {d:?}"
        );
    }

    #[test]
    fn no_law_file_means_no_holds() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "anything.rs", "content": "x"}),
            ),
            None
        );
    }
}

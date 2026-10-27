//! `revert_turn` — agent-callable tool that rewinds the workspace to a
//! prior pre-turn snapshot.
//!
//! The model invokes this when the user says something like "undo the
//! last edit" or "roll back". It mirrors `/restore` but speaks JSON and
//! takes a turn-offset (default 1 = previous turn) instead of a list
//! index, so the model doesn't have to count entries.
//!
//! Approval is `Required` because this mutates the workspace.
//!
//! Known limit: like `/restore`, this is a whole-tree rollback. It restores
//! every path to the pre-turn snapshot, so an edit made after that turn (the
//! user's, or another session's) is overwritten too; it does not have the
//! path scoping, changed-since refusal, uncapped lookup or fork-inherited
//! restore points of `/undo` (#6644).

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};
use codewhale_snapshot::SnapshotRepo;

/// Default offset: revert the most-recent turn (i.e. the last `pre-turn:*`
/// snapshot in history).
const DEFAULT_OFFSET: u64 = 1;
/// Hard cap so the model can't ask to roll back to the dawn of time.
const MAX_OFFSET: u64 = 50;

pub struct RevertTurnTool;

#[async_trait]
impl ToolSpec for RevertTurnTool {
    fn name(&self) -> &str {
        "revert_turn"
    }

    fn description(&self) -> &str {
        "Roll back the workspace files to the snapshot taken before a recent turn. \
         Use when the user explicitly asks to undo, revert, or roll back the most recent edits. \
         `turn_offset` is 1-based: 1 reverts the most recent turn (max 50). Conversation \
         history is NOT modified. The whole workspace is restored, so later edits, including \
         the user's own, are overwritten."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "turn_offset": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_OFFSET,
                    "description": "How many turns back to revert (default 1)."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let offset = optional_u64(&input, "turn_offset", DEFAULT_OFFSET)?;
        if offset == 0 || offset > MAX_OFFSET {
            return Err(ToolError::invalid_input(format!(
                "turn_offset must be between 1 and {MAX_OFFSET}; got {offset}",
            )));
        }

        let workspace = context.workspace.clone();
        let label = format!("revert_turn(offset={offset})");
        let session = context.state_namespace.clone();
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            #[cfg(test)]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let repo = SnapshotRepo::open_or_init(&workspace)
                .map_err(|e| format!("Snapshot repo init failed: {e}"))?;
            // Find pre-turn:* snapshots only — those mark the start of
            // each turn, which is the right rollback target. We pull a
            // generous list and filter so the model's `turn_offset` is
            // counted in turns, not raw snapshots.
            let snapshots = repo
                .list((MAX_OFFSET as usize).saturating_mul(2) + 16)
                .map_err(|e| format!("Snapshot list failed: {e}"))?;
            let pre_turns: Vec<_> = snapshots
                .into_iter()
                .filter(|s| s.label.starts_with("pre-turn:"))
                // Session ownership must be exact. Legacy snapshots are not
                // safe automatic targets because they may belong to another
                // conversation that used this workspace.
                .filter(|s| s.session_id.as_deref() == Some(session.as_str()))
                .collect();
            let target = pre_turns
                .get((offset - 1) as usize)
                .ok_or_else(|| {
                    format!(
                        "Only {} current-session pre-turn snapshot(s) exist; turn_offset={offset} is out of range.",
                        pre_turns.len(),
                    )
                })?
                .clone();
            if repo
                .work_tree_matches_snapshot(&target.id)
                .map_err(|e| format!("Snapshot comparison failed: {e}"))?
            {
                return Err(format!(
                    "NoSnapshotForTurn: target '{}' ({}) already matches the current workspace. \
                     Revert operates at completed turn boundaries; there is no distinct later snapshot to restore.",
                    target.label,
                    short_sha(target.id.as_str()),
                ));
            }
            repo.restore(&target.id)
                .map_err(|e| format!("Restore failed: {e}"))?;
            Ok(format!(
                "{label}: restored '{}' ({}). Workspace files reverted; conversation unchanged.",
                target.label,
                short_sha(target.id.as_str()),
            ))
        })
        .await
        .map_err(|e| ToolError::execution_failed(format!("revert_turn join failed: {e}")))?;

        match result {
            Ok(msg) => Ok(ToolResult::success(msg)),
            Err(e) => Ok(ToolResult::error(e)),
        }
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Seals the user's home onto `home` for the duration of the test. Pinning
    /// `HOME` alone left an ambient `CODEWHALE_HOME` in charge of the snapshot
    /// store.
    fn scoped_home(home: &std::path::Path) -> crate::test_support::SealedHome {
        crate::test_support::SealedHome::at(home)
    }

    #[tokio::test]
    async fn revert_turn_default_offset_restores_pre_turn_one() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        // Setup: create pre-turn:1, post-turn:1 with file modifications.
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"original").unwrap();
        repo.snapshot_with_session("pre-turn:1", Some("workspace"))
            .unwrap();
        std::fs::write(workspace.join("a.txt"), b"modified").unwrap();
        repo.snapshot_with_session("post-turn:1", Some("workspace"))
            .unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(r.success, "expected success: {r:?}");

        let content = std::fs::read_to_string(workspace.join("a.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[tokio::test]
    async fn revert_turn_invalid_offset_rejected() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({"turn_offset": 0}), &ctx).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn revert_turn_rejects_snapshot_matching_current_workspace() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"unchanged").unwrap();
        repo.snapshot_with_session("pre-turn:1", Some("workspace"))
            .unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!r.success);
        assert!(r.content.contains("NoSnapshotForTurn"), "{}", r.content);
    }

    #[tokio::test]
    async fn revert_turn_no_snapshots_returns_error_result() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace);
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!r.success);
        assert!(r.content.contains("out of range"));
    }

    #[tokio::test]
    async fn revert_turn_rejects_legacy_and_foreign_session_snapshots() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let _guard = scoped_home(tmp.path());

        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"legacy").unwrap();
        repo.snapshot("pre-turn:legacy").unwrap();
        std::fs::write(workspace.join("a.txt"), b"foreign").unwrap();
        repo.snapshot_with_session("pre-turn:foreign", Some("other-session"))
            .unwrap();
        std::fs::write(workspace.join("a.txt"), b"current").unwrap();

        let tool = RevertTurnTool;
        let ctx = ToolContext::new(workspace.clone());
        let r = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!r.success);
        assert!(
            r.content.contains("Only 0 current-session"),
            "{}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "current"
        );
    }
}

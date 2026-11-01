//! Workspace snapshots — pre/post-turn safety net.
//!
//! Each turn the engine takes a `pre-turn:<seq>` snapshot of the user's
//! workspace into a side git repo at
//! `<snapshot state dir>/<project_hash>/<worktree_hash>/.git`, then a
//! matching `post-turn:<seq>` snapshot when the turn finishes. Users
//! can roll back via `/restore N` (slash command) or, when the model
//! recognises an "undo my last edit" intent, the `revert_turn` tool.
//!
//! ## Why a side repo?
//!
//! - The user's own `.git` is never touched. `--git-dir` and
//!   `--work-tree` are *always* set together when we shell out to git;
//!   that single invariant is what keeps snapshots and the user's repo
//!   completely independent.
//! - Workspaces without git still get snapshots.
//! - `git`'s own deduplication (object packfiles) keeps the disk
//!   footprint tractable — typical 100 MB workspace × 12 turns ≈ 1.2 GB
//!   uncompressed but git's content-addressed storage usually brings
//!   that down 10-30×. We mitigate further with:
//!     - 7-day default retention (`session_manager` prunes at session
//!       start via [`prune::prune_older_than`]).
//!     - `gc.auto = 0` on the side repo (we don't want background gcs
//!       firing mid-turn) plus an explicit `git gc --prune=now` after
//!       prune.
//!     - Startup cleanup for stale `tmp_pack_*` files left by interrupted
//!       git pack operations.
//!
//! ## Failure model
//!
//! Pre/post-turn snapshot calls are **non-fatal**. If `git` is missing,
//! the disk is full, or the workspace is on a read-only filesystem, the
//! turn proceeds and the engine logs a warning. The snapshot is a
//! safety net, not a correctness gate.
//!
//! Workspaces over the configured size cap (`[snapshots] max_workspace_gb`,
//! default 2 GB of non-excluded content) skip snapshot init entirely. That
//! disable is intentionally loud: the operator is told once that undo is off
//! for the workspace, with the opt-in knobs (raise the cap, or set
//! `max_workspace_gb = 0` to disable the size gate). Scoped snapshot roots are
//! not yet a first-class config; the practical opt-in today is the cap override.

pub mod delta;
pub mod paths;
pub mod prune;
pub mod repo;

#[allow(unused_imports)]
pub use paths::{snapshot_dir_for, snapshot_git_dir};
pub use prune::prune_older_than;

/// Snapshots kept per workspace side-repo, pruned after each new snapshot to
/// cap disk usage (#1112): the newest this many, plus the newest this many
/// turn boundaries (`pre-turn:` / `post-turn:`), so a burst of per-tool
/// snapshots can never push out the restore points of the turns that took
/// them (see `SnapshotRepo::prune_keep_last_n`).
pub const DEFAULT_MAX_SNAPSHOTS: usize = 50;
pub use delta::{DeltaChange, SnapshotDelta};
#[allow(unused_imports)]
pub use repo::{
    GATE_TOO_LARGE_MARKER, GATE_TOO_MANY_ENTRIES_MARKER, GATE_UNSAFE_LOCATION_MARKER,
    PathRestoreAction, PathRestoreOutcome, SIZE_WALK_MAX_ENTRIES, Snapshot, SnapshotId,
    SnapshotPathChange, SnapshotRepo, TakenSnapshot, WorkspaceGate,
    estimate_workspace_size_bounded, workspace_relative_path,
};

/// Which point of a turn a recorded workspace snapshot captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSnapshotKind {
    /// Before the turn touched anything (`pre-turn:` label).
    PreTurn,
    /// Before one file-modifying tool call ran (`tool:<call_id>` label).
    Tool,
    /// After a tool call that had a `tool` snapshot finished
    /// (`post-tool:<call_id>` label). Taken only by hosts that record
    /// restore points, so the span a tool ran in is bounded on both sides.
    PostTool,
    /// After the turn finished (`post-turn:` label).
    PostTurn,
}

impl WorkspaceSnapshotKind {
    /// The label prefix the snapshot repo stores for this kind.
    pub fn label_prefix(self) -> &'static str {
        match self {
            Self::PreTurn => "pre-turn:",
            Self::Tool => "tool:",
            Self::PostTool => "post-tool:",
            Self::PostTurn => "post-turn:",
        }
    }
}

/// Receipt for one workspace snapshot an engine took on behalf of a turn.
///
/// The engine reports it (`Event::WorkspaceSnapshotTaken`) and the Runtime
/// records it on the turn that was running, so a thread owns exactly the
/// restore points recorded on its own turns — including the turns a fork
/// inherited — regardless of which saved-session document the thread is
/// bound to. `tree_id` is the durable identity: a prune rebuilds the side
/// repo's commit chain and rewrites every commit id, but re-commits the same
/// trees. `session_id` is the tag the snapshot was taken under; a restore
/// point only resolves to a snapshot that still carries it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceSnapshotRef {
    pub kind: WorkspaceSnapshotKind,
    /// Commit id when the snapshot was taken. A later prune may rewrite it;
    /// `tree_id` still resolves the snapshot then.
    pub snapshot_id: String,
    /// Root tree of the snapshot.
    pub tree_id: String,
    /// Session tag the snapshot was taken under.
    pub session_id: String,
    /// The tool call that runs from this snapshot to the next one of the
    /// turn: the call a `tool` snapshot preceded, or, on a `pre_turn`
    /// snapshot, the user shell command a shell turn runs. A `post_tool`
    /// snapshot names the call it closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For a `tool` snapshot of a file tool (`write_file`, `edit_file`,
    /// `apply_patch`): the paths the call declared it writes, as it named
    /// them. Absent for a tool whose writes are not declared (a shell
    /// command, a program), which may change any path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_paths: Option<Vec<String>>,
    /// Workspace-relative paths whose content changed since the turn's
    /// previous snapshot: what happened in the span this snapshot closes.
    /// Absent on a `pre_turn` snapshot (nothing precedes it in the turn) and
    /// when it could not be computed (the previous snapshot failed or is
    /// gone), which leaves that span unaccounted for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_paths: Option<Vec<String>>,
}

impl WorkspaceSnapshotRef {
    pub fn new(
        kind: WorkspaceSnapshotKind,
        taken: &TakenSnapshot,
        session_id: &str,
        tool_call_id: Option<&str>,
    ) -> Self {
        Self {
            kind,
            snapshot_id: taken.id.as_str().to_string(),
            tree_id: taken.tree.as_str().to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.map(str::to_string),
            write_paths: None,
            changed_paths: None,
        }
    }

    /// Whether `snapshot` (a row of [`SnapshotRepo::list`]) is this restore
    /// point: same tree, same session tag, same label kind.
    pub fn matches(&self, snapshot: &Snapshot) -> bool {
        snapshot.tree.as_str() == self.tree_id
            && snapshot.session_id.as_deref() == Some(self.session_id.as_str())
            && snapshot.label.starts_with(self.kind.label_prefix())
    }
}

/// Post-turn snapshots this process has promised but not written yet.
///
/// An interactive engine takes its post-turn snapshot after `TurnComplete`,
/// off the input path (#234), so `/undo` typed right after a turn can run
/// before it lands. Without it the newest step would end at the workspace as
/// it is now, taking every edit made since the turn's last restore point for
/// the step's own, and both `git add -A` runs would race on the side repo's
/// index (#6644). The engine reserves the snapshot before `TurnComplete` and
/// `/undo` waits for it with [`wait_for_pending_post_turn_snapshots`].
///
/// In-process only: a post-turn snapshot another process is taking is not
/// seen here.
static PENDING_POST_TURN: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// A reserved post-turn snapshot; dropping it (after the snapshot is written,
/// failed, or abandoned) releases the reservation.
#[must_use = "the reservation is released when this is dropped"]
pub struct PendingPostTurnSnapshot(());

impl PendingPostTurnSnapshot {
    pub fn reserve() -> Self {
        let (count, _) = &PENDING_POST_TURN;
        *count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        Self(())
    }
}

impl Drop for PendingPostTurnSnapshot {
    fn drop(&mut self) {
        let (count, released) = &PENDING_POST_TURN;
        let mut count = count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count = count.saturating_sub(1);
        released.notify_all();
    }
}

/// Wait up to `timeout` for every reserved post-turn snapshot to be written.
/// Returns whether none is still pending.
pub fn wait_for_pending_post_turn_snapshots(timeout: std::time::Duration) -> bool {
    let (count, released) = &PENDING_POST_TURN;
    let count = count
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (count, _) = released
        .wait_timeout_while(count, timeout, |pending| *pending > 0)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *count == 0
}

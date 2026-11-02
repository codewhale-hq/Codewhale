//! Typed host boundaries replacing direct App access in the remaining debug
//! commands. Read-only receipts/diff/change cannot acquire mutation authority.
//! Snapshot planning, safe restoration and history updates remain host-owned;
//! command parsing, fallback decisions and report rendering remain portable.

use super::{Receipt, SessionSyncPayload};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugReceiptError {
    ApprovalLog(String),
    Build(String),
}

pub trait CommandDebugReceiptsContext {
    fn receipt(&self, turn: Option<&str>) -> Result<Receipt, DebugReceiptError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugChangeProjection {
    pub changelog: &'static str,
    pub is_english: bool,
    pub translation_available: bool,
    pub translation_target: &'static str,
}

pub trait CommandDebugChangeContext {
    fn change_projection(&self) -> DebugChangeProjection;
}

pub trait CommandDebugHistoryContext {
    fn last_user_input(&self) -> Option<String>;
    fn load_composer(&mut self, input: String);
    /// Prepare a last-exchange rollback without mutating the live transcript.
    /// The host applies it only after the Engine acknowledges the truncated
    /// conversation; retry additionally waits for durable persistence (#6788).
    fn undo_conversation(&mut self) -> DebugConversationUndo;
}

/// Prepared conversation-only undo: display cells to remove after Engine
/// acknowledgement, and the conversation to install as its history.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugConversationUndo {
    pub removed: usize,
    pub sync: SessionSyncPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugDiffObservation {
    GitUnavailable,
    /// Nothing to compare against: the session has saved no restore point
    /// for this workspace and the workspace is not a git repository.
    NoBaseline,
    Output {
        names: String,
        stat: String,
        /// The unified patch behind `names` and `stat`, bounded by the host.
        patch: String,
        /// The host cut `patch` at its bound; `names` and `stat` are whole.
        patch_truncated: bool,
    },
    Failed(String),
}

pub trait CommandDebugDiffContext {
    fn diff(&self) -> DebugDiffObservation;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugRestoreAction {
    Modified,
    Recreated,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugRestoredFile {
    pub action: DebugRestoreAction,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DebugUndoRestored {
    /// First line of the undone request's prompt, when its restore point
    /// recorded one.
    pub request: Option<String>,
    /// Whether the host must rewind the matching request and its reply.
    pub rewind_conversation: bool,
    /// The request had no recorded end and `/undo force` ran it, so edits
    /// made to these files since it started went back too.
    pub forced: bool,
    pub files: Vec<DebugRestoredFile>,
    pub skipped: Vec<PathBuf>,
    pub sync: SessionSyncPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DebugUndoOutcome {
    RepoUnavailable {
        workspace: PathBuf,
        error: String,
    },
    SnapshotPending,
    NoSnapshots,
    NoSession,
    NoOwnedSteps,
    NoDifference,
    /// The last request has no recorded end, so undoing it would also put
    /// back edits made since. Nothing was written; `force` runs it.
    OpenEnded,
    CompareFailed(String),
    SnapshotFailed(String),
    ChangedSince {
        paths: Vec<String>,
    },
    ListFailed(String),
    RestoreBlocked(String),
    RestoreFailed(String),
    Restored(DebugUndoRestored),
}

pub trait CommandDebugUndoContext {
    /// Undoes the last request's file changes, and removes that request from
    /// the conversation when it is still its last one. Preserves snapshot
    /// ownership, regular-file checks, backup and preflight, and asks for no
    /// trust: it only puts back, inside the workspace, what that request
    /// changed. `force` never waives owned identity or a complete write record. Never falls
    /// back to chat truncation: the portable handler decides that from the
    /// typed outcome.
    fn undo_files(&mut self, force: bool) -> DebugUndoOutcome;
}

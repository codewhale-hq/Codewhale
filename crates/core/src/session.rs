//! `Thread` / `Session` split (issue #5261).
//!
//! `codewhale`'s `Session` was really a thread. The new split is:
//! - `Thread` — durable, persisted, owns the append-only `Journal` and the
//!   `leafId` cursor. One row in `state.threads`, one directory on disk.
//! - `Session` — ephemeral, per-turn / per-engine-lifetime, owns the
//!   in-memory `TurnContext` plus the live approval/sandbox posture for this
//!   `SessionId`. Many sessions can attach to one thread over time, but only
//!   one `Session` drives a turn for a given `ThreadId` at a time.
//!
//! The canonical RuntimeThreadManager creates sessions without a TUI and drives
//! the same Engine turn loop used by mounted clients. These types preserve the
//! shared model, append log, prefix stability and message revision state; the
//! compatibility Core Runtime owns only configuration, hooks and job bookkeeping.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, ThreadId};
use crate::journal::Journal;

/// Durable thread (the former `Session`). One per conversation, persisted in
/// `state.threads`. The only new field vs the old `Session` is `leaf_id` — the
/// journal cursor — plus the typed `ThreadId`. All other fields keep their
/// persisted JSON shape unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub thread_id: ThreadId,
    /// Active branch tip. `None` before the first journal header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
    /// Journal (append-only). In-memory projection of the persisted
    /// `threads/turns/items/events` layout is derived root→leaf.
    #[serde(default)]
    pub journal: Journal,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub workspace: PathBuf,
    #[serde(default)]
    pub ephemeral: bool,
}

impl Thread {
    #[must_use]
    pub fn new(thread_id: ThreadId, workspace: PathBuf, model: impl Into<String>) -> Self {
        Self {
            thread_id,
            leaf_id: None,
            journal: Journal::new(),
            model: model.into(),
            reasoning_effort: None,
            workspace,
            ephemeral: false,
        }
    }

    #[must_use]
    pub fn leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }
}

/// Ephemeral session within a thread (one engine lifetime / one turn's
/// live posture). The TUI's `EngineHandle` and the headless `exec` both
/// hold a `Session` that points at the same `ThreadId` but with different
/// `SessionId`s.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    /// Model for this session's next turn (may differ from thread default).
    pub model: String,
    pub workspace: PathBuf,
    /// Monotonic `messages_revision` for prefix-cache memoization (carried
    /// from the former `Session::messages_revision`).
    pub messages_revision: u64,
}

impl Session {
    #[must_use]
    pub fn new(thread_id: ThreadId, workspace: PathBuf, model: impl Into<String>) -> Self {
        Self {
            session_id: SessionId::new(),
            thread_id,
            model: model.into(),
            workspace,
            messages_revision: 0,
        }
    }

    pub fn bump_revision(&mut self) {
        self.messages_revision = self.messages_revision.wrapping_add(1);
    }
}

/// Split helper: derive a `Session` from an existing `Thread` without
/// cloning the journal. Headless and TUI call the same constructor so
/// the request shape stays identical.
#[must_use]
pub fn session_for_thread(thread: &Thread, workspace: PathBuf) -> Session {
    Session::new(thread.thread_id.clone(), workspace, thread.model.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_and_session_ids_are_distinct_scopes() {
        let t = Thread::new(ThreadId::new(), PathBuf::from("/tmp"), "deepseek-v4-flash");
        let s1 = Session::new(t.thread_id.clone(), PathBuf::from("/tmp"), &t.model);
        let s2 = Session::new(t.thread_id.clone(), PathBuf::from("/tmp"), &t.model);
        assert_eq!(s1.thread_id, s2.thread_id);
        assert_ne!(s1.session_id, s2.session_id);
    }

    #[test]
    fn leaf_is_moved_not_rewritten() {
        let mut t = Thread::new(ThreadId::new(), PathBuf::from("/tmp"), "m");
        let a = t.journal.append("header", serde_json::json!({}));
        let b = t.journal.append("user", serde_json::json!("b"));
        t.leaf_id = t.journal.leaf_id.clone();
        assert_eq!(t.leaf_id.as_deref(), Some(b.as_str()));
        assert!(t.journal.branch_to(&a));
        t.leaf_id = t.journal.leaf_id.clone();
        assert_eq!(t.leaf_id.as_deref(), Some(a.as_str()));
        assert_eq!(t.journal.len(), 2); // history never rewritten; branching only moved the leaf
    }
}

//! FEAT-023 Phase 4/6 test support: a deterministic canned implementation of
//! `CommandSessionLifecycleContext` so portable handlers are unit-tested for
//! exact message/action composition without host state.

use std::cell::Cell;

use codewhale_command_contract::facets::{
    CommandSessionLifecycleContext, SessionArchiveReceipt, SessionBranchOutcome,
    SessionForkFromReceipt, SessionForkReceipt, SessionNewReceipt, SessionSaveReceipt,
    SessionSyncPayload, TreeBodyProjection,
};
use std::path::PathBuf;

/// Every delegate returns the canned value set by the test and records its
/// arguments so handler-routing assertions cannot pass without the expected
/// facet call. Unconfigured result slots return a descriptive canned error.
pub(crate) struct CannedLifecycle {
    pub blocked: bool,
    pub blockers: Vec<String>,
    pub transition_checks: Cell<usize>,
    pub leaf_hint: Option<String>,
    pub branch: Result<SessionBranchOutcome, String>,
    pub tree: Result<TreeBodyProjection, String>,
    pub save: Result<SessionSaveReceipt, String>,
    pub fork_active: Result<SessionForkReceipt, String>,
    pub fork_from: Result<SessionForkFromReceipt, String>,
    pub fresh: Result<SessionNewReceipt, String>,
    pub load: Result<PathBuf, String>,
    pub archived: Result<SessionArchiveReceipt, String>,
    pub prune: Result<usize, String>,
    pub branch_entries: Vec<String>,
    pub save_paths: Vec<Option<String>>,
    pub fork_sources: Vec<String>,
    pub fresh_forces: Vec<bool>,
    pub load_paths: Vec<String>,
    pub picker_calls: Vec<Option<String>>,
    pub archive_calls: Vec<(String, bool)>,
    pub prune_days: Vec<u64>,
}

impl Default for CannedLifecycle {
    fn default() -> Self {
        Self {
            blocked: false,
            blockers: Vec::new(),
            transition_checks: Cell::new(0),
            leaf_hint: None,
            branch: Err("canned: branch_to not configured".to_string()),
            tree: Ok(TreeBodyProjection::NoSession),
            save: Err("canned: save not configured".to_string()),
            fork_active: Err("canned: fork_active not configured".to_string()),
            fork_from: Err("canned: fork_from not configured".to_string()),
            fresh: Err("canned: fresh_session not configured".to_string()),
            load: Err("canned: load not configured".to_string()),
            archived: Err("canned: set_archived not configured".to_string()),
            prune: Err("canned: prune not configured".to_string()),
            branch_entries: Vec::new(),
            save_paths: Vec::new(),
            fork_sources: Vec::new(),
            fresh_forces: Vec::new(),
            load_paths: Vec::new(),
            picker_calls: Vec::new(),
            archive_calls: Vec::new(),
            prune_days: Vec::new(),
        }
    }
}

pub(crate) fn sync_payload(session_id: &str) -> SessionSyncPayload {
    SessionSyncPayload {
        session_id: Some(session_id.to_string()),
        messages: vec![],
        system_prompt: None,
        model: "test-model".to_string(),
        workspace: PathBuf::from("/workspace"),
        mode: codewhale_command_contract::types::CommandMode::Agent,
    }
}

impl CommandSessionLifecycleContext for CannedLifecycle {
    fn transition_blocked(&self) -> bool {
        self.transition_checks
            .set(self.transition_checks.get().saturating_add(1));
        self.blocked
    }
    fn transition_blockers(&self) -> Vec<String> {
        self.blockers.clone()
    }
    fn branch_current_leaf_hint(&self) -> Option<String> {
        self.leaf_hint.clone()
    }
    fn branch_to(&mut self, entry_id: &str) -> Result<SessionBranchOutcome, String> {
        self.branch_entries.push(entry_id.to_string());
        self.branch.clone()
    }
    fn tree_body(&self) -> Result<TreeBodyProjection, String> {
        self.tree.clone()
    }
    fn save_session(&mut self, path: Option<String>) -> Result<SessionSaveReceipt, String> {
        self.save_paths.push(path);
        self.save.clone()
    }
    fn fork_active(&mut self) -> Result<SessionForkReceipt, String> {
        self.fork_active.clone()
    }
    fn fork_from(&mut self, id: &str) -> Result<SessionForkFromReceipt, String> {
        self.fork_sources.push(id.to_string());
        self.fork_from.clone()
    }
    fn fresh_session(&mut self, force: bool) -> Result<SessionNewReceipt, String> {
        self.fresh_forces.push(force);
        self.fresh.clone()
    }
    fn load_session(&mut self, path: &str) -> Result<PathBuf, String> {
        self.load_paths.push(path.to_string());
        self.load.clone()
    }
    fn open_picker(&mut self, preselected: Option<String>) {
        self.picker_calls.push(preselected);
    }
    fn set_archived(&mut self, id: &str, archived: bool) -> Result<SessionArchiveReceipt, String> {
        self.archive_calls.push((id.to_string(), archived));
        self.archived.clone()
    }
    fn prune_sessions(&mut self, days: u64) -> Result<usize, String> {
        self.prune_days.push(days);
        self.prune.clone()
    }
}

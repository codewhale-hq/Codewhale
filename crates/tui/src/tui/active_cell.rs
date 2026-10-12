//! Active in-flight tool/exec cell — single mutable group that buffers parallel
//! tool work for the current turn.
//!
//! ## Why
//!
//! When the model issues parallel tool calls in a single assistant turn (e.g.
//! two `read_file` and one `grep_files` running concurrently), naively
//! appending each tool start as its own history cell makes the transcript
//! "bounce" as completions arrive out of order. Codex's pattern is to keep all
//! in-flight tool work in ONE active cell that mutates in place; once the turn
//! resolves the active cell finalizes into the transcript.
//!
//! ## Contract
//!
//! - At most one [`ActiveCell`] per turn. It holds zero or more
//!   [`HistoryCell`]s that are still being mutated (status `Running`, output
//!   pending, etc.).
//! - The owning [`crate::tui::app::App`] renders the active cell's contents
//!   AFTER `App.history` so they appear at the live tail.
//! - Cell indices used by helpers like `tool_cells` / `tool_details_by_cell`
//!   address the virtual sequence `App.history ++ active_cell.entries`. Each
//!   entry's index is `App.history.len() + entry_offset`.
//! - When a tool completes whose `tool_id` does not match any active entry
//!   (orphan), the caller pushes a finalized standalone cell into `App.history`
//!   instead of mutating the active group. This keeps `active_cell` a stable
//!   reflection of what was actually started, and avoids merging unrelated
//!   tool work.
//! - On `TurnComplete` (or cancellation) the active cell is "flushed":
//!   in-progress entries are marked with the supplied terminal status, then
//!   every entry is appended to `App.history`. Companion maps
//!   (`tool_cells`, `tool_details_by_cell`) are rewritten to point at the new
//!   `App.history` indices.
//!
//! ## Revision counter
//!
//! Cells inside the active group mutate without changing pointer identity, so
//! the transcript cache cannot rely on enum-equality for invalidation. We
//! expose `revision()` and `bump_revision()`; the renderer combines this with
//! `App.history_version` when computing per-cell revisions for the cache.

use crate::tui::history::{ExploringCell, ExploringEntry, HistoryCell, ToolCell, ToolStatus};

/// In-flight active cell: a sequence of mutable [`HistoryCell`] entries.
///
/// Conceptually a single "live tail" cell in the Codex sense: it appears as
/// one logical block at the end of the transcript, but internally it is
/// composed of one or more entries (each rendered as its own
/// [`HistoryCell`]). The reason we keep them as separate entries — rather
/// than fusing into a single conceptual block — is that they may have
/// different shapes (an `ExecCell`, an `ExploringCell` aggregate, an MCP
/// tool result, …) and the existing renderers already know how to draw each
/// shape correctly. Coalescing into a single render path would duplicate
/// logic we already have.
#[derive(Debug, Clone, Default)]
pub struct ActiveCell {
    entries: Vec<HistoryCell>,
    /// Tool ids currently associated with this active cell. The map values are
    /// indices into [`Self::entries`]. Multiple tool ids can map to the same
    /// entry (the existing `ExploringCell` aggregates several reads into a
    /// single entry).
    tool_to_entry: std::collections::HashMap<String, usize>,
    /// Index of the current `ExploringCell` entry (when present), so additional
    /// exploring tool starts append to it instead of creating new cells.
    exploring_entry: Option<usize>,
    /// Bumped on every mutation. Used by the transcript cache to know that
    /// the active cell needs re-rendering even though its position in the
    /// virtual cell list is unchanged.
    revision: u64,
}

impl ActiveCell {
    /// Create an empty active cell.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries (each rendered as its own [`HistoryCell`]).
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Whether the active cell has any entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read-only access to the underlying entries (for rendering).
    #[must_use]
    pub fn entries(&self) -> &[HistoryCell] {
        &self.entries
    }

    /// Mutable access to a specific entry. Bumps the revision counter so the
    /// renderer knows the cached lines are stale.
    pub fn entry_mut(&mut self, index: usize) -> Option<&mut HistoryCell> {
        if index < self.entries.len() {
            self.bump_revision();
            self.entries.get_mut(index)
        } else {
            None
        }
    }

    /// Current revision counter. Wraps on overflow which is fine for cache
    /// invalidation; the chance of a wrap-around collision is astronomical
    /// over a single session and any miss only causes one extra re-render.
    #[must_use]
    #[allow(dead_code)] // Used by App::bump_active_cell_revision and future cache wiring.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Increment the revision counter. Call any time an entry is mutated.
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Add a tool entry to the active cell.
    ///
    /// Returns the entry index (which the caller can record in
    /// `tool_cells_in_active`). If the cell is an exploring tool start and
    /// there is already an exploring entry in the active group, the entry is
    /// appended to that aggregate instead of creating a new entry.
    ///
    /// `tool_id` is registered for the new (or updated) entry so future
    /// completion lookups can find it.
    pub fn push_tool(&mut self, tool_id: impl Into<String>, cell: HistoryCell) -> usize {
        let tool_id = tool_id.into();
        // If this is an exploring start and we already have an exploring
        // entry, append to that entry rather than creating a new cell.
        if let HistoryCell::Tool(ToolCell::Exploring(new_cell)) = &cell
            && let Some(entry_idx) = self.exploring_entry
            && let Some(HistoryCell::Tool(ToolCell::Exploring(existing))) =
                self.entries.get_mut(entry_idx)
        {
            // The caller hands us a brand-new ExploringCell with one entry.
            // Move that entry into the existing aggregate.
            for explore_entry in &new_cell.entries {
                let _ = existing.insert_entry(explore_entry.clone());
            }
            self.tool_to_entry.insert(tool_id, entry_idx);
            self.bump_revision();
            return entry_idx;
        }

        // Otherwise, push a new entry.
        let entry_idx = self.entries.len();
        if matches!(cell, HistoryCell::Tool(ToolCell::Exploring(_))) {
            self.exploring_entry = Some(entry_idx);
        }
        self.entries.push(cell);
        self.tool_to_entry.insert(tool_id, entry_idx);
        self.bump_revision();
        entry_idx
    }

    /// Push an entry with no tool id binding. Approval notices use this path
    /// so they can remain in the transcript without shifting the virtual
    /// indices of tools that are still running in `active_cell`.
    pub fn push_untracked(&mut self, cell: HistoryCell) -> usize {
        let entry_idx = self.entries.len();
        self.entries.push(cell);
        self.bump_revision();
        entry_idx
    }

    /// Push a thinking entry as a new active-cell entry. Sibling to
    /// [`Self::push_tool`] but for `HistoryCell::Thinking` content. Returns the
    /// entry index. Thinking entries do not participate in `tool_to_entry` or
    /// the exploring aggregation — each thinking block stands on its own.
    ///
    /// P2.3: thinking lives in the active cell so a `Thinking → Tool → Tool`
    /// sequence renders as one logical "Working…" block until the next
    /// assistant prose chunk flushes the group into history.
    pub fn push_thinking(&mut self, cell: HistoryCell) -> usize {
        debug_assert!(
            matches!(cell, HistoryCell::Thinking { .. }),
            "push_thinking expects HistoryCell::Thinking",
        );
        let entry_idx = self.entries.len();
        self.entries.push(cell);
        self.bump_revision();
        entry_idx
    }

    /// Look up the entry index that holds the given tool id.
    #[must_use]
    pub fn entry_index_for_tool(&self, tool_id: &str) -> Option<usize> {
        self.tool_to_entry.get(tool_id).copied()
    }

    /// Append an [`ExploringEntry`] to the existing exploring aggregate (if
    /// any), binding the supplied tool id to it. Returns
    /// `(entry_index, entry_within_exploring)` on success.
    ///
    /// Used when a second exploring tool starts during the same active group:
    /// rather than allocating another ExploringCell entry in the active group
    /// we extend the one that's already there.
    pub fn append_to_exploring(
        &mut self,
        tool_id: impl Into<String>,
        explore_entry: ExploringEntry,
    ) -> Option<(usize, usize)> {
        let entry_idx = self.exploring_entry?;
        let HistoryCell::Tool(ToolCell::Exploring(cell)) = self.entries.get_mut(entry_idx)? else {
            return None;
        };
        let inner_idx = cell.insert_entry(explore_entry);
        self.tool_to_entry.insert(tool_id.into(), entry_idx);
        self.bump_revision();
        Some((entry_idx, inner_idx))
    }

    /// Ensure an [`ExploringCell`] exists in the active group; create it if
    /// not. Returns its entry index.
    pub fn ensure_exploring(&mut self) -> usize {
        if let Some(idx) = self.exploring_entry {
            return idx;
        }
        let idx = self.entries.len();
        self.entries
            .push(HistoryCell::Tool(ToolCell::Exploring(ExploringCell {
                entries: Vec::new(),
            })));
        self.exploring_entry = Some(idx);
        self.bump_revision();
        idx
    }

    /// Drain every entry, returning them in insertion order. Resets internal
    /// state (revision is bumped via `bump_revision`).
    ///
    /// Callers use this on `TurnComplete` (or cancellation) to flush the
    /// active group into `App.history`.
    pub fn drain(&mut self) -> Vec<HistoryCell> {
        let entries = std::mem::take(&mut self.entries);
        self.tool_to_entry.clear();
        self.exploring_entry = None;
        self.bump_revision();
        entries
    }

    /// Mark every still-running tool entry as `Failed` (used when the turn is
    /// cancelled mid-flight). Entries that already completed are left alone.
    ///
    /// `Failed` is the closest existing variant for "interrupted"; the cell's
    /// surrounding context (turn-status banner) tells the user it was a
    /// cancellation rather than a tool error.
    pub fn mark_in_progress_as_interrupted(&mut self) {
        for cell in &mut self.entries {
            mark_running_as_interrupted(cell);
        }
        self.bump_revision();
    }
}

fn mark_running_as_interrupted(cell: &mut HistoryCell) {
    if let HistoryCell::Thinking {
        streaming,
        duration_secs,
        ..
    } = cell
    {
        // A thinking cell stuck mid-stream should stop spinning when the turn
        // is cancelled. Leave `duration_secs` as-is if it's already populated;
        // otherwise the renderer simply omits the duration badge.
        *streaming = false;
        let _ = duration_secs;
        return;
    }
    let HistoryCell::Tool(tool_cell) = cell else {
        return;
    };
    match tool_cell {
        ToolCell::Exec(exec) if exec.status == ToolStatus::Running => {
            exec.status = ToolStatus::Failed;
        }
        ToolCell::Exploring(explore) => {
            for entry in &mut explore.entries {
                if entry.status == ToolStatus::Running {
                    entry.status = ToolStatus::Failed;
                }
            }
        }
        ToolCell::PlanUpdate(plan) if plan.status == ToolStatus::Running => {
            plan.status = ToolStatus::Failed;
        }
        ToolCell::PatchSummary(patch) if patch.status == ToolStatus::Running => {
            patch.status = ToolStatus::Failed;
        }
        ToolCell::Review(review) if review.status == ToolStatus::Running => {
            review.status = ToolStatus::Failed;
        }
        ToolCell::Mcp(mcp) if mcp.status == ToolStatus::Running => {
            mcp.status = ToolStatus::Failed;
        }
        ToolCell::WebSearch(search) if search.status == ToolStatus::Running => {
            search.status = ToolStatus::Failed;
        }
        ToolCell::Generic(generic) if generic.status == ToolStatus::Running => {
            generic.status = ToolStatus::Failed;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::history::{
        ExecCell, ExecSource, ExploringCell, ExploringEntry, GenericToolCell,
    };
    use std::time::Instant;

    fn exec_cell(command: &str) -> HistoryCell {
        HistoryCell::Tool(ToolCell::Exec(ExecCell {
            command: command.to_string(),
            status: ToolStatus::Running,
            output: None,
            live_output: None,
            shell_task_id: None,
            owner_agent_id: None,
            owner_agent_name: None,
            started_at: Some(Instant::now()),
            duration_ms: None,
            stale_elapsed_since_output_ms: None,
            source: ExecSource::Assistant,
            interaction: None,
            output_summary: None,
        }))
    }

    fn exploring_cell_with(label: &str) -> HistoryCell {
        HistoryCell::Tool(ToolCell::Exploring(ExploringCell {
            entries: vec![ExploringEntry {
                label: label.to_string(),
                status: ToolStatus::Running,
                error: None,
            }],
        }))
    }

    fn generic_cell(name: &str) -> HistoryCell {
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: name.to_string(),
            status: ToolStatus::Running,
            input_summary: None,
            output: None,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }))
    }

    #[test]
    fn push_tool_records_entry_and_revision_advances() {
        let mut cell = ActiveCell::new();
        let r0 = cell.revision();
        let idx = cell.push_tool("t1", exec_cell("ls"));
        assert_eq!(idx, 0);
        assert_eq!(cell.entry_count(), 1);
        assert!(cell.revision() != r0);
        assert_eq!(cell.entry_index_for_tool("t1"), Some(0));
    }

    #[test]
    fn parallel_exploring_starts_share_one_entry() {
        let mut cell = ActiveCell::new();
        let idx_a = cell.push_tool("a", exploring_cell_with("Read foo.rs"));
        let idx_b = cell.push_tool("b", exploring_cell_with("Read bar.rs"));
        assert_eq!(
            idx_a, idx_b,
            "both exploring starts should land in same entry"
        );
        assert_eq!(cell.entry_count(), 1);
        let HistoryCell::Tool(ToolCell::Exploring(explore)) = &cell.entries()[0] else {
            panic!("expected exploring cell")
        };
        assert_eq!(explore.entries.len(), 2);
    }

    #[test]
    fn drain_resets_state_and_returns_in_order() {
        let mut cell = ActiveCell::new();
        cell.push_tool("a", exec_cell("ls"));
        cell.push_tool("b", generic_cell("foo"));
        let drained = cell.drain();
        assert_eq!(drained.len(), 2);
        assert!(cell.is_empty());
        assert_eq!(cell.entry_index_for_tool("a"), None);
    }

    #[test]
    fn interrupt_marks_running_entries_failed() {
        let mut cell = ActiveCell::new();
        cell.push_tool("a", exec_cell("ls"));
        cell.mark_in_progress_as_interrupted();
        let HistoryCell::Tool(ToolCell::Exec(exec)) = &cell.entries()[0] else {
            panic!("expected exec")
        };
        assert_eq!(exec.status, ToolStatus::Failed);
    }

    fn thinking_cell(content: &str, streaming: bool) -> HistoryCell {
        HistoryCell::Thinking {
            content: content.to_string(),
            streaming,
            duration_secs: None,
        }
    }

    #[test]
    fn push_thinking_records_entry_at_tail() {
        let mut cell = ActiveCell::new();
        let r0 = cell.revision();
        let idx = cell.push_thinking(thinking_cell("planning…", true));
        assert_eq!(idx, 0);
        assert_eq!(cell.entry_count(), 1);
        assert!(cell.revision() != r0);
    }

    #[test]
    fn thinking_then_tools_group_in_one_active_cell() {
        // P2.3: a turn that emits Thinking → Tool → Tool keeps everything in
        // one active cell until the next prose chunk flushes the group.
        let mut cell = ActiveCell::new();
        cell.push_thinking(thinking_cell("plan…", true));
        cell.push_tool("t-1", exec_cell("ls"));
        cell.push_tool("t-2", exploring_cell_with("Read foo.rs"));
        assert_eq!(
            cell.entry_count(),
            3,
            "thinking, exec, and exploring entries coexist in one active cell"
        );
        assert!(matches!(cell.entries()[0], HistoryCell::Thinking { .. }));
        assert!(matches!(
            cell.entries()[1],
            HistoryCell::Tool(ToolCell::Exec(_))
        ));
        assert!(matches!(
            cell.entries()[2],
            HistoryCell::Tool(ToolCell::Exploring(_))
        ));
    }

    #[test]
    fn drain_flushes_thinking_alongside_tools_in_order() {
        let mut cell = ActiveCell::new();
        cell.push_thinking(thinking_cell("plan…", false));
        cell.push_tool("t", exec_cell("ls"));
        let drained = cell.drain();
        assert_eq!(drained.len(), 2);
        assert!(matches!(drained[0], HistoryCell::Thinking { .. }));
        assert!(matches!(drained[1], HistoryCell::Tool(ToolCell::Exec(_))));
    }

    #[test]
    fn interrupt_stops_streaming_thinking_spinner() {
        let mut cell = ActiveCell::new();
        cell.push_thinking(thinking_cell("plan…", true));
        cell.mark_in_progress_as_interrupted();
        let HistoryCell::Thinking { streaming, .. } = &cell.entries()[0] else {
            panic!("expected thinking cell")
        };
        assert!(
            !*streaming,
            "interrupted thinking should stop streaming so the spinner exits"
        );
    }
}

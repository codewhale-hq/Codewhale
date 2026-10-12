//! Cached transcript rendering for the TUI.
//!
//! ## Per-cell revision caching
//!
//! A whole-transcript cache would re-wrap every cell whenever the streaming
//! Assistant mutates. Instead each index has a paired revision and unchanged
//! cells reuse their wrapped lines. Width, options, fold state, or destructive
//! identity changes bust the affected cache. Streaming therefore scales with
//! changed cells rather than history; width/option changes still bust all
//! cells because wrapping and visibility depend on them.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::tui::app::TranscriptSpacing;
use crate::tui::history::{
    CellFoldAction, CellFoldActionTarget, HistoryCell, TranscriptActionOwner, TranscriptFold,
    TranscriptRenderOptions,
};
use crate::tui::scrolling::TranscriptLineMeta;
use crate::tui::ui_text::CopyLineSeparator;
use codewhale_localization::{MessageId, tr};

/// Revision-bound render output. Arcs keep cache enumeration O(cells) instead
/// of deep-cloning every rendered line on ambient frames (issue #78); the
/// flattened output owns the only per-frame line copy.
#[derive(Debug)]
struct CachedCell {
    /// Revision at which lines and metadata were rendered.
    revision: u64,
    /// Lines and aligned metadata; no inter-cell spacers. OSC 8 targets never
    /// enter the ratatui cell buffer. Copy separators preserve source hard
    /// newlines while allowing copy to remove visual soft-wrap breaks; prefix
    /// widths strip visual rails. All four vectors remain index-aligned.
    lines: Arc<Vec<Line<'static>>>,
    links: Arc<Vec<Vec<crate::tui::osc8::LineLink>>>,
    copy_separators: Arc<Vec<CopyLineSeparator>>,
    copy_prefix_widths: Arc<Vec<usize>>,
    /// Empty/blank facts keep spacing decisions independent of rendered text.
    /// A block ending blank has paid for separation and must not get another.
    is_empty: bool,
    ends_blank: bool,
    /// Semantic role and tool grouping feed the explicit boundary matrix, so
    /// spacing never depends on strings, palette, terminal depth, or motion.
    kind: TranscriptBlockKind,
    is_tool_groupable: bool,
    fold_action: Option<CellFoldAction>,
    /// Only the changing Assistant cell carries incremental parser state;
    /// stable lines stay above its replaceable-tail index.
    incremental_markdown: Option<Box<crate::tui::markdown_render::IncrementalMarkdownRenderCache>>,
    /// Settled form of the animation-mutated hot tail, restored on append so
    /// the stable prefix is not reparsed.
    hot_tail_original: Option<(usize, Line<'static>)>,
}

/// Proof that a live Assistant source only gained appended bytes. Visual-only
/// revision bumps can therefore reuse it; revisions are transformed exactly
/// as they are for `ensure_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamingSourceReceipt {
    pub cell_index: usize,
    pub from_revision: u64,
    pub to_revision: u64,
    pub content_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptBlockKind {
    User,
    Reasoning,
    Answer,
    ToolAction,
    DurableWork,
    Notice,
}

impl TranscriptBlockKind {
    fn for_cell(cell: &HistoryCell) -> Self {
        match cell {
            HistoryCell::User { .. } => Self::User,
            HistoryCell::Thinking { .. } => Self::Reasoning,
            HistoryCell::Assistant { .. } => Self::Answer,
            HistoryCell::Tool(tool) if tool.is_durable_work_receipt() => Self::DurableWork,
            HistoryCell::Tool(_) | HistoryCell::SubAgent(_) => Self::ToolAction,
            HistoryCell::System { .. }
            | HistoryCell::Error { .. }
            | HistoryCell::Automation(_)
            | HistoryCell::ArchivedContext { .. } => Self::Notice,
        }
    }
}

/// A visible boundary costs one row, because terminal separator rows displace
/// content and two rows add no extra legibility. Only an opt-in spacious turn
/// costs two.
const BLOCK_SEPARATOR_ROWS: usize = 1;

/// Complete transcript spacing vocabulary; no blanket per-cell padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptBoundary {
    /// Successive cells are literally one reasoning/answer phase.
    Joined,
    /// Adjacent tool cells share one compact rail with no per-call padding.
    GroupedTool,
    /// Transition between response phases, tools, Work, or notices.
    Activity,
    /// Human turn boundary; visible even at compact density.
    Turn,
}

#[derive(Debug)]
pub struct TranscriptViewCache {
    width: u16,
    options: TranscriptRenderOptions,
    /// Explicit per-cell fold intent affects rendering without changing cell
    /// revisions. Keyed by original virtual cell index.
    cell_folds: HashMap<usize, TranscriptFold>,
    /// Index of the newest durable Work receipt (checklist / plan snapshot)
    /// in the last pass. When a new one lands the previous newest must
    /// re-render collapsed, and its revision alone would not say so.
    newest_work_receipt: Option<usize>,
    /// Index of the newest user turn in the last pass. Only it carries the
    /// elevated-surface background; when a new prompt lands the previous
    /// newest must re-render on the bare ground, and its revision alone
    /// would not say so.
    newest_user_turn: Option<usize>,
    fold_action_target: Option<CellFoldActionTarget>,
    transcript_action_owner: Option<TranscriptActionOwner>,
    identity_epoch: Option<u64>,
    fold_action_rendered_cell: Option<usize>,
    /// Per-cell renders plus flattened lines and index-aligned link/selection
    /// metadata. Rail prefix widths strip decoration without glyph guessing
    /// (#1163); deterministic counters measure the production cache path.
    per_cell: Vec<CachedCell>,
    /// `per_cell[i].revision`, kept contiguous. The steady-state check and the
    /// first-changed-cell search compare this against the caller's revisions
    /// without touching a single cached render (#6652).
    seen_revisions: Vec<u64>,
    lines: Vec<Line<'static>>,
    line_links: Vec<Vec<crate::tui::osc8::LineLink>>,
    line_meta: Vec<TranscriptLineMeta>,
    /// Visual-only prefix widths let selection copy strip rails without glyph guesses.
    rail_prefix_widths: Vec<usize>,
    /// Flat line index at which each cell's rendered lines begin (hidden
    /// cells record where their successor starts). Lets a suffix rebuild
    /// truncate and an action-hint move repaint without scanning every row.
    cell_line_starts: Vec<usize>,
    streaming_source_receipt: Option<StreamingSourceReceipt>,
    streaming_lines_reflattened: u64,
    streaming_meta_rows_scanned: u64,
    /// Cells (re)rendered over the cache's lifetime; proves a settled frame
    /// renders nothing.
    #[cfg(test)]
    cells_rendered: u64,
}

impl TranscriptViewCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            width: 0,
            options: TranscriptRenderOptions::default(),
            cell_folds: HashMap::new(),
            newest_work_receipt: None,
            newest_user_turn: None,
            fold_action_target: None,
            transcript_action_owner: None,
            identity_epoch: None,
            fold_action_rendered_cell: None,
            per_cell: Vec::new(),
            seen_revisions: Vec::new(),
            lines: Vec::new(),
            line_links: Vec::new(),
            line_meta: Vec::new(),
            rail_prefix_widths: Vec::new(),
            cell_line_starts: Vec::new(),
            streaming_source_receipt: None,
            streaming_lines_reflattened: 0,
            streaming_meta_rows_scanned: 0,
            #[cfg(test)]
            cells_rendered: 0,
        }
    }

    pub(crate) fn set_streaming_source_receipt(&mut self, receipt: Option<StreamingSourceReceipt>) {
        self.streaming_source_receipt = receipt;
    }

    #[cfg(test)]
    pub(crate) fn streaming_render_work(
        &self,
        cell_index: usize,
    ) -> Option<crate::tui::markdown_render::MarkdownRenderWork> {
        self.per_cell
            .get(cell_index)?
            .incremental_markdown
            .as_ref()
            .map(|cache| cache.work())
    }

    pub(crate) fn take_transcript_action(
        &mut self,
    ) -> Option<(TranscriptActionOwner, Option<CellFoldActionTarget>)> {
        Some((
            self.transcript_action_owner.take()?,
            self.fold_action_target.take(),
        ))
    }

    pub(crate) fn retarget(
        &mut self,
        owner: Option<TranscriptActionOwner>,
        original_index_map: Option<&[usize]>,
    ) {
        // Scrolling moves the owner to the newest visible cell, so this runs
        // on scroll frames. The hint only rewrites one line of the previous
        // and next owner, so repaint those rows in place instead of
        // re-flattening the transcript tail below them (#6652).
        if let Some(change) = self.set_action_owner(owner, original_index_map)
            && !self.repaint_action_hints(change)
        {
            self.flatten_from(
                self.options.spacing,
                first_hint_cell(change).saturating_sub(1),
            );
        }
    }

    /// Convenience entry point; the live path uses shards to avoid cloning.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn ensure(
        &mut self,
        cells: &[HistoryCell],
        cell_revisions: &[u64],
        width: u16,
        options: TranscriptRenderOptions,
    ) {
        self.ensure_split(
            &[cells],
            cell_revisions,
            width,
            options,
            &HashMap::new(),
            None,
            None,
        );
    }

    /// Ensure logically concatenated shards without cloning history plus the
    /// active tail each frame. Explicit cache inputs keep identity/fold/map indices
    /// attached to original virtual cells even when filtering changes positions.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_split(
        &mut self,
        cell_shards: &[&[HistoryCell]],
        cell_revisions: &[u64],
        width: u16,
        options: TranscriptRenderOptions,
        cell_folds: &HashMap<usize, TranscriptFold>,
        original_index_map: Option<&[usize]>,
        action_owner: Option<TranscriptActionOwner>,
    ) {
        let total_cells: usize = cell_shards.iter().map(|s| s.len()).sum();
        self.ensure_iter(
            total_cells,
            |index| {
                let mut remaining = index;
                for shard in cell_shards {
                    if let Some(cell) = shard.get(remaining) {
                        return cell;
                    }
                    remaining -= shard.len();
                }
                unreachable!("cell index {index} is below total_cells")
            },
            cell_revisions,
            width,
            options,
            cell_folds,
            original_index_map,
            action_owner,
        );
    }

    /// Ensure an already-filtered list plus its original-index map. Collapse
    /// may skip cells or substitute tool summaries, so the map is the contract
    /// that keeps fold/action state attached to original virtual indices
    /// rather than positional rendered indices (#3896).
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_filtered(
        &mut self,
        cells: &[&HistoryCell],
        cell_revisions: &[u64],
        width: u16,
        options: TranscriptRenderOptions,
        cell_folds: &HashMap<usize, TranscriptFold>,
        original_index_map: Option<&[usize]>,
        action_owner: Option<TranscriptActionOwner>,
    ) {
        self.ensure_iter(
            cells.len(),
            |index| cells[index],
            cell_revisions,
            width,
            options,
            cell_folds,
            original_index_map,
            action_owner,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_iter<'a>(
        &mut self,
        total_cells: usize,
        cell_at: impl Fn(usize) -> &'a HistoryCell,
        cell_revisions: &[u64],
        width: u16,
        options: TranscriptRenderOptions,
        cell_folds: &HashMap<usize, TranscriptFold>,
        original_index_map: Option<&[usize]>,
        action_owner: Option<TranscriptActionOwner>,
    ) {
        let identity_changed = action_owner.is_some_and(|owner| {
            self.identity_epoch
                .is_some_and(|epoch| epoch != owner.identity_epoch)
        });
        if let Some(owner) = action_owner {
            self.identity_epoch = Some(owner.identity_epoch);
        }
        self.transcript_action_owner = action_owner;
        // Collapsed reasoning has a fixed preview budget; viewport height
        // does not participate in wrapping or cached cell identity.
        let layout_changed = self.width != width || self.options != options || identity_changed;
        let folded_changed = self.cell_folds != *cell_folds;
        let revisions_match = cell_revisions.len() == total_cells;
        // Cells `0..unchanged` were rendered at exactly these revisions, so
        // their cached output (and what they are: kind, supersession) is
        // current. Layout, fold, or a mismatched revision vector (never
        // trusted) leaves no such prefix. In steady state this is all of
        // them, and the frame ends below without rendering, moving, or even
        // looking at a cell (#6652).
        let mut unchanged = if layout_changed || folded_changed || !revisions_match {
            0
        } else {
            self.seen_revisions
                .iter()
                .zip(cell_revisions)
                .position(|(seen, current)| seen != current)
                .unwrap_or_else(|| self.seen_revisions.len().min(total_cells))
        };
        // `todo_write` replaces the whole list on every call, so only the
        // newest snapshot is worth a full card (#5871). When a new one lands
        // the previous newest must re-render collapsed; its own revision has
        // not changed, so supersession has to invalidate the cache itself.
        let newest_work_receipt = newest_matching_cell(
            total_cells,
            unchanged,
            self.newest_work_receipt,
            &cell_at,
            |cell| matches!(cell, HistoryCell::Tool(tool) if tool.is_durable_work_receipt()),
        );
        let work_receipt_changed = self.newest_work_receipt != newest_work_receipt;
        self.newest_work_receipt = newest_work_receipt;
        // Same supersession shape as the work receipt: the highlight lives on
        // the newest user turn only, so a newly sent prompt must un-highlight
        // its predecessor even though that cell's own revision never moved.
        let newest_user_turn = newest_matching_cell(
            total_cells,
            unchanged,
            self.newest_user_turn,
            &cell_at,
            |cell| matches!(cell, HistoryCell::User { .. }),
        );
        let user_turn_changed = self.newest_user_turn != newest_user_turn;
        self.newest_user_turn = newest_user_turn;
        if layout_changed || folded_changed || work_receipt_changed || user_turn_changed {
            self.per_cell.clear();
            self.seen_revisions.clear();
            unchanged = 0;
        }
        self.width = width;
        self.options = options;
        // Cloning the map every frame is wasted work for the usual unchanged
        // case (#6652); `folded_changed` already compared the two.
        if folded_changed {
            self.cell_folds.clone_from(cell_folds);
        }
        let previous_rendered_target = self.fold_action_rendered_cell;

        // Same-index revision reuse is intentional: insert/remove shifts must
        // cold-render rather than attach cached lines to another cell. The
        // destructive identity epoch also prevents revision reuse after an
        // index is removed and later filled by a different cell.
        let old_len = self.per_cell.len();
        let mut any_dirty = layout_changed
            || folded_changed
            || work_receipt_changed
            || user_turn_changed
            || old_len != total_cells;
        let mut first_dirty: Option<usize> = if old_len != total_cells {
            Some(old_len.min(total_cells))
        } else {
            None
        };

        // Update the cache where it stands: a frame that changes only the tail
        // renders only the tail instead of moving every cached cell out and
        // back (#6652).
        self.per_cell.truncate(total_cells);
        self.seen_revisions.truncate(total_cells);
        let mut dirty_cells = 0usize;
        let mut streaming_tail_update = None;

        for idx in unchanged..total_cells {
            // A mismatched revision vector is never trusted.
            let current_rev = cell_revisions
                .get(idx)
                .copied()
                .filter(|_| revisions_match)
                .unwrap_or(u64::MAX);
            if revisions_match && self.seen_revisions.get(idx) == Some(&current_rev) {
                continue;
            }
            let cell = cell_at(idx);
            let original_idx = original_index_map
                .map(|m| *m.get(idx).unwrap_or(&idx))
                .unwrap_or(idx);
            let is_tool_groupable = matches!(cell, HistoryCell::Tool(_));
            let render_width = if is_tool_groupable {
                width.saturating_sub(2).max(1)
            } else {
                width
            };
            let fold = cell_folds.get(&original_idx).copied();

            any_dirty = true;
            dirty_cells = dirty_cells.saturating_add(1);
            #[cfg(test)]
            {
                self.cells_rendered += 1;
            }
            first_dirty = Some(first_dirty.map_or(idx, |current| current.min(idx)));

            if fold != Some(TranscriptFold::Collapsed)
                && matches!(
                    cell,
                    HistoryCell::Assistant {
                        streaming: true,
                        ..
                    }
                )
            {
                if idx >= self.per_cell.len() {
                    self.per_cell.push(CachedCell {
                        revision: current_rev,
                        lines: Arc::new(Vec::new()),
                        links: Arc::new(Vec::new()),
                        copy_separators: Arc::new(Vec::new()),
                        copy_prefix_widths: Arc::new(Vec::new()),
                        is_empty: true,
                        ends_blank: false,
                        kind: TranscriptBlockKind::Answer,
                        is_tool_groupable: false,
                        fold_action: None,
                        incremental_markdown: Some(Box::default()),
                        hot_tail_original: None,
                    });
                    self.seen_revisions.push(current_rev);
                }
                let receipt = self.streaming_source_receipt;
                let cached = &mut self.per_cell[idx];
                let shape_before = (cached.kind, cached.is_tool_groupable, cached.is_empty);
                if let Some((line_index, original)) = cached.hot_tail_original.take()
                    && let Some(line) = Arc::make_mut(&mut cached.lines).get_mut(line_index)
                {
                    *line = original;
                }
                let content_len = match cell {
                    HistoryCell::Assistant { content, .. } => content.len(),
                    _ => 0,
                };
                let verified_append = receipt.is_some_and(|receipt| {
                    receipt.cell_index == original_idx
                        && receipt.from_revision == cached.revision
                        && receipt.to_revision == current_rev
                        && receipt.content_len == content_len
                });
                let incremental = cached.incremental_markdown.get_or_insert_with(Box::default);
                let replace_from = cell
                    .update_incremental_streaming_render(
                        render_width,
                        options,
                        verified_append,
                        incremental,
                        Arc::make_mut(&mut cached.lines),
                        Arc::make_mut(&mut cached.links),
                        Arc::make_mut(&mut cached.copy_separators),
                        Arc::make_mut(&mut cached.copy_prefix_widths),
                    )
                    .expect("streaming Assistant matched above");
                let cached_lines = Arc::make_mut(&mut cached.lines);
                let last_index = cached_lines.len().checked_sub(1);
                if let Some((index, last)) = last_index
                    .and_then(|index| cached_lines.get_mut(index).map(|line| (index, line)))
                {
                    cached.hot_tail_original = Some((index, last.clone()));
                    crate::tui::history::apply_hot_tail_to_line(last, options.low_motion);
                }
                cached.revision = current_rev;
                cached.is_empty = cached.lines.is_empty();
                cached.ends_blank = last_line_is_blank(&cached.lines);
                cached.kind = TranscriptBlockKind::Answer;
                cached.is_tool_groupable = false;
                cached.fold_action = None;
                // The spacer and group rail between this cell and its
                // predecessor depend on what this cell is. A tool, hidden, or
                // other cell becoming a streaming answer in place (a filter
                // shifting rows under it) changes them, so only an answer that
                // stays an answer may take the tail-only shortcut.
                let same_shape =
                    shape_before == (cached.kind, cached.is_tool_groupable, cached.is_empty);
                self.seen_revisions[idx] = current_rev;
                // Hot-tail styling can affect the preceding settled line.
                streaming_tail_update = same_shape.then_some((idx, replace_from.saturating_sub(1)));
                continue;
            }

            let mut cell_options = options;
            cell_options.superseded_work_receipt = matches!(
                cell,
                HistoryCell::Tool(tool) if tool.is_durable_work_receipt()
            ) && newest_work_receipt
                .is_some_and(|newest| newest != idx);
            cell_options.newest_user_turn = matches!(cell, HistoryCell::User { .. })
                && newest_user_turn.is_some_and(|newest| newest == idx);
            let rendered = render_cached_cell(cell, current_rev, width, cell_options, fold);
            if let Some(slot) = self.per_cell.get_mut(idx) {
                *slot = rendered;
                self.seen_revisions[idx] = current_rev;
            } else {
                self.per_cell.push(rendered);
                self.seen_revisions.push(current_rev);
            }
        }
        debug_assert_eq!(self.per_cell.len(), total_cells);
        debug_assert_eq!(self.seen_revisions.len(), total_cells);
        // Rows from here down are rebuilt this frame; rows above it are
        // untouched and still sit at their recorded `cell_line_starts`.
        let mut rebuild_from = if !any_dirty {
            usize::MAX
        } else if layout_changed {
            0
        } else {
            self.visible_rebuild_start(first_dirty.unwrap_or(0).saturating_sub(1))
        };
        let mut hint_settled = true;
        if let Some(change) = self.set_action_owner(action_owner, original_index_map) {
            // Scrolling moves the owner to the newest visible cell, including
            // while a reply streams and its cell is dirty every frame. A
            // hinted cell inside the rebuilt suffix is re-flattened with the
            // new hint anyway; one above it keeps its geometry, so repaint its
            // single hint row instead of rebuilding the tail below it (#6652).
            let above = |cell: Option<usize>| cell.filter(|&cell| cell < rebuild_from);
            let in_place = HintChange {
                previous: above(change.previous),
                next: above(change.next),
            };
            let repainted = self.repaint_action_hints(in_place);
            if !repainted {
                any_dirty = true;
                rebuild_from = self.visible_rebuild_start(
                    rebuild_from.min(first_hint_cell(change).saturating_sub(1)),
                );
            }
            hint_settled = repainted && in_place == change;
        }

        if !any_dirty {
            return;
        }

        if !layout_changed
            && !folded_changed
            && (hint_settled || previous_rendered_target == self.fold_action_rendered_cell)
            && old_len == total_cells
            && dirty_cells == 1
            && let Some((cell_index, line_from)) = streaming_tail_update
            && cell_index + 1 == total_cells
            && self.flatten_streaming_tail(cell_index, line_from)
        {
            return;
        }

        self.flatten_from(options.spacing, rebuild_from);
    }

    /// A hidden cell has no line boundary at which to truncate. Rebuild from
    /// a visible predecessor so appearance/disappearance cannot leave its old
    /// spacer or the following cell's boundary behind.
    fn visible_rebuild_start(&self, mut from: usize) -> usize {
        while from > 0 && self.per_cell.get(from).is_some_and(|cell| cell.is_empty) {
            from -= 1;
        }
        from
    }

    /// Returns the previous and next hinted cells when the hint moved.
    fn set_action_owner(
        &mut self,
        owner: Option<TranscriptActionOwner>,
        original_index_map: Option<&[usize]>,
    ) -> Option<HintChange> {
        self.transcript_action_owner = owner;
        let rendered = owner.and_then(|owner| match original_index_map {
            Some(map) => map.iter().position(|&index| index == owner.cell_index),
            None => (owner.cell_index < self.per_cell.len()).then_some(owner.cell_index),
        });
        self.fold_action_target = owner.and_then(|owner| {
            Some(CellFoldActionTarget {
                owner,
                action: self.per_cell.get(rendered?)?.fold_action?,
            })
        });
        let next = self
            .fold_action_target
            .filter(|target| target.action == CellFoldAction::Expand)
            .and(rendered);
        let previous = self.fold_action_rendered_cell;
        self.fold_action_rendered_cell = next;
        (previous != next).then_some(HintChange { previous, next })
    }

    /// Rewrite the affordance row of the cells whose hint changed. The hint
    /// only swaps the final span of a cell's header, so geometry, spacers,
    /// and every other row are unchanged. Returns false when the flat index
    /// cannot vouch for a cell, and the caller falls back to a rebuild.
    fn repaint_action_hints(&mut self, change: HintChange) -> bool {
        let hint = self.action_hint();
        for cell_index in [change.previous, change.next].into_iter().flatten() {
            let Some(cached) = self.per_cell.get(cell_index) else {
                return false;
            };
            if cached.lines.is_empty() {
                continue;
            }
            let line_in_cell = 0;
            let Some(at) = self
                .cell_line_starts
                .get(cell_index)
                .map(|start| start + line_in_cell)
            else {
                return false;
            };
            if self
                .line_meta
                .get(at)
                .and_then(TranscriptLineMeta::cell_line)
                != Some((cell_index, line_in_cell))
            {
                return false;
            }
            let (line, links, rail_prefix_width, meta) =
                self.flat_cell_line(hint.as_deref(), cell_index, line_in_cell);
            self.lines[at] = line;
            self.line_links[at] = links;
            self.rail_prefix_widths[at] = rail_prefix_width;
            self.line_meta[at] = meta;
            self.streaming_lines_reflattened = self.streaming_lines_reflattened.saturating_add(1);
        }
        true
    }

    /// Localized Space affordance, or None when it cannot fit the width.
    fn action_hint(&self) -> Option<String> {
        let hint = format!(
            "Space:{}",
            tr(self.options.locale, MessageId::TranscriptReasoningExpand)
        );
        (unicode_width::UnicodeWidthStr::width(hint.as_str())
            <= usize::from(self.width).saturating_sub(2))
        .then_some(hint)
    }

    /// Final flat form of one cached cell line: hint, group rail, links, and
    /// copy metadata. Shared by flattening and in-place hint repaint.
    fn flat_cell_line(
        &self,
        hint: Option<&str>,
        cell_index: usize,
        line_in_cell: usize,
    ) -> (
        Line<'static>,
        Vec<crate::tui::osc8::LineLink>,
        usize,
        TranscriptLineMeta,
    ) {
        let cached = &self.per_cell[cell_index];
        let rendered_line_count = cached.lines.len();
        let line = &cached.lines[line_in_cell];
        let hint = hint.filter(|hint| {
            self.fold_action_rendered_cell == Some(cell_index)
                && line_in_cell == 0
                && line
                    .width()
                    .saturating_sub(line.spans.last().map_or(0, Span::width))
                    + unicode_width::UnicodeWidthStr::width(*hint)
                    < usize::from(self.width)
        });
        let is_hint = hint.is_some();
        let hinted = hint.map(|hint| {
            let mut hinted = line.clone();
            if let Some(span) = hinted.spans.last_mut() {
                span.content = format!(" {hint}").into();
            }
            hinted
        });
        let line = hinted.as_ref().unwrap_or(line);
        let rail = tool_group_rail(
            self.per_cell.as_slice(),
            cell_index,
            line_in_cell,
            rendered_line_count,
        );
        let final_line = line_with_group_rail(line, rail, usize::from(self.width));
        let final_links = if is_hint {
            Vec::new()
        } else {
            links_with_group_rail(
                cached.links.get(line_in_cell).map_or(&[], Vec::as_slice),
                rail,
                usize::from(self.width),
            )
        };
        let rail_prefix_width = compute_rail_prefix_width(&final_line);
        let copy_prefix_width = if is_hint {
            final_line.width().saturating_sub(rail_prefix_width)
        } else {
            cached
                .copy_prefix_widths
                .get(line_in_cell)
                .copied()
                .unwrap_or(0)
        };
        let meta = TranscriptLineMeta::CellLine {
            cell_index,
            line_in_cell,
            copy_prefix_width,
            copy_separator_after: cached
                .copy_separators
                .get(line_in_cell)
                .copied()
                .unwrap_or(CopyLineSeparator::Newline),
        };
        (final_line, final_links, rail_prefix_width, meta)
    }

    fn flatten(&mut self, spacing: TranscriptSpacing) {
        self.lines.clear();
        self.line_links.clear();
        self.line_meta.clear();
        self.rail_prefix_widths.clear();
        self.cell_line_starts.clear();
        self.append_flattened_cells(spacing, 0);
    }

    /// Rebuild only a suffix while preserving its predecessor spacer.
    /// Streaming normally changes only the active tail; rebuilding from the
    /// previous cell preserves boundary correctness without flattening all
    /// transcript lines on every token chunk.
    fn flatten_from(&mut self, spacing: TranscriptSpacing, first_cell: usize) {
        if first_cell == 0 || self.lines.is_empty() || self.line_meta.is_empty() {
            self.flatten(spacing);
            return;
        }

        let truncate_at = match self.cell_line_starts.get(first_cell) {
            Some(&start) => start,
            None => self
                .line_meta
                .iter()
                .position(|meta| match meta {
                    TranscriptLineMeta::CellLine { cell_index, .. } => *cell_index >= first_cell,
                    TranscriptLineMeta::Spacer { .. } => false,
                })
                .unwrap_or(self.lines.len()),
        };
        self.cell_line_starts
            .truncate(first_cell.min(self.cell_line_starts.len()));
        self.lines.truncate(truncate_at);
        self.line_links.truncate(truncate_at);
        self.line_meta.truncate(truncate_at);
        self.rail_prefix_widths.truncate(truncate_at);
        self.append_flattened_cells(spacing, first_cell);
    }

    /// Replace only the final streaming cell's changing Markdown tail. Search
    /// backward from the old hot tail, so append-only updates scan the small
    /// replaceable suffix; return false when no canonical boundary exists.
    fn flatten_streaming_tail(&mut self, cell_index: usize, line_from: usize) -> bool {
        let mut truncate_at = None;
        for (index, meta) in self.line_meta.iter().enumerate().rev() {
            self.streaming_meta_rows_scanned = self.streaming_meta_rows_scanned.saturating_add(1);
            if matches!(
                meta,
                TranscriptLineMeta::CellLine {
                    cell_index: candidate,
                    line_in_cell,
                    ..
                } if *candidate == cell_index && *line_in_cell == line_from
            ) {
                truncate_at = Some(index);
                break;
            }
        }
        let Some(truncate_at) = truncate_at else {
            return false;
        };
        self.lines.truncate(truncate_at);
        self.line_links.truncate(truncate_at);
        self.line_meta.truncate(truncate_at);
        self.rail_prefix_widths.truncate(truncate_at);

        let Some(cached) = self.per_cell.get(cell_index) else {
            return false;
        };
        let rendered_line_count = cached.lines.len();
        for line_in_cell in line_from..rendered_line_count {
            let line = &cached.lines[line_in_cell];
            let rail = tool_group_rail(
                self.per_cell.as_slice(),
                cell_index,
                line_in_cell,
                rendered_line_count,
            );
            let final_line = line_with_group_rail(line, rail, usize::from(self.width));
            let final_links = links_with_group_rail(
                cached.links.get(line_in_cell).map_or(&[], Vec::as_slice),
                rail,
                usize::from(self.width),
            );
            self.rail_prefix_widths
                .push(compute_rail_prefix_width(&final_line));
            self.lines.push(final_line);
            self.line_links.push(final_links);
            self.line_meta.push(TranscriptLineMeta::CellLine {
                cell_index,
                line_in_cell,
                copy_prefix_width: cached
                    .copy_prefix_widths
                    .get(line_in_cell)
                    .copied()
                    .unwrap_or(0),
                copy_separator_after: cached
                    .copy_separators
                    .get(line_in_cell)
                    .copied()
                    .unwrap_or(CopyLineSeparator::Newline),
            });
            self.streaming_lines_reflattened = self.streaming_lines_reflattened.saturating_add(1);
        }
        true
    }

    fn append_flattened_cells(&mut self, spacing: TranscriptSpacing, start_cell: usize) {
        let hint = self.action_hint();
        // A start beyond the recorded prefix is only reachable through the
        // fallback scan; pad so indices stay aligned with cells.
        let pad_to = start_cell.min(self.per_cell.len());
        while self.cell_line_starts.len() < pad_to {
            self.cell_line_starts.push(self.lines.len());
        }
        for cell_index in start_cell..self.per_cell.len() {
            self.cell_line_starts.push(self.lines.len());
            let cached = &self.per_cell[cell_index];
            if cached.is_empty {
                continue;
            }
            for line_in_cell in 0..cached.lines.len() {
                let (final_line, final_links, rail_prefix_width, meta) =
                    self.flat_cell_line(hint.as_deref(), cell_index, line_in_cell);
                self.rail_prefix_widths.push(rail_prefix_width);
                self.lines.push(final_line);
                self.line_links.push(final_links);
                self.line_meta.push(meta);
                self.streaming_lines_reflattened =
                    self.streaming_lines_reflattened.saturating_add(1);
            }

            let cached = &self.per_cell[cell_index];
            if let Some(next) = next_visible_cell(&self.per_cell, cell_index) {
                let separator = separator_between(cached, next, spacing);
                let rail = separator
                    .railed
                    .then_some(crate::tui::widgets::tool_card::CardRail::Middle);
                for _ in 0..separator.rows {
                    let line = line_with_group_rail(&Line::from(""), rail, usize::from(self.width));
                    let copy_prefix_width = compute_rail_prefix_width(&line);
                    self.rail_prefix_widths.push(copy_prefix_width);
                    self.lines.push(line);
                    self.line_links.push(Vec::new());
                    self.line_meta
                        .push(TranscriptLineMeta::Spacer { copy_prefix_width });
                }
            }
        }
    }

    #[must_use]
    pub fn lines(&self) -> &[Line<'static>] {
        &self.lines
    }

    #[must_use]
    pub fn line_links(&self) -> &[Vec<crate::tui::osc8::LineLink>] {
        &self.line_links
    }

    #[must_use]
    pub fn line_meta(&self) -> &[TranscriptLineMeta] {
        &self.line_meta
    }

    #[must_use]
    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn rail_prefix_width(&self, line_index: usize) -> usize {
        self.rail_prefix_widths
            .get(line_index)
            .copied()
            .unwrap_or(0)
    }
}

/// Index of the last cell satisfying `is_match`.
///
/// The previous pass already classified cells `0..unchanged` (they are
/// rendered at the same revisions), so only the changed suffix is searched
/// and a settled transcript costs nothing. A prefix answer is reused only
/// while it still names a cell inside the prefix; otherwise the prefix is
/// searched too (#6652).
fn newest_matching_cell<'a>(
    total_cells: usize,
    unchanged: usize,
    previous: Option<usize>,
    cell_at: &impl Fn(usize) -> &'a HistoryCell,
    is_match: impl Fn(&HistoryCell) -> bool,
) -> Option<usize> {
    let found = (unchanged..total_cells)
        .rev()
        .find(|&index| is_match(cell_at(index)))
        .or_else(|| match previous {
            None => None,
            Some(index) if index < unchanged => Some(index),
            Some(_) => (0..unchanged).rev().find(|&index| is_match(cell_at(index))),
        });
    // The shortcut above is only as good as the revision contract. Tests
    // prove it against the full scan; production never pays for it.
    #[cfg(test)]
    assert_eq!(
        found,
        (0..total_cells)
            .rev()
            .find(|&index| is_match(cell_at(index))),
        "a cell changed without its revision changing"
    );
    found
}

/// Previous and next cells carrying the Space affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HintChange {
    previous: Option<usize>,
    next: Option<usize>,
}

/// Earliest cell whose hint changed; rebuilds start one cell earlier.
fn first_hint_cell(change: HintChange) -> usize {
    [change.previous, change.next]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(0)
}

fn render_cached_cell(
    cell: &HistoryCell,
    revision: u64,
    width: u16,
    options: TranscriptRenderOptions,
    fold: Option<TranscriptFold>,
) -> CachedCell {
    let is_tool_groupable = matches!(cell, HistoryCell::Tool(_));
    let render_width = if is_tool_groupable {
        width.saturating_sub(2).max(1)
    } else {
        width
    };
    let (rendered, fold_action) = cell.lines_with_copy_metadata_folded(render_width, options, fold);
    let mut lines = Vec::with_capacity(rendered.len());
    let mut links = Vec::with_capacity(rendered.len());
    let mut copy_separators = Vec::with_capacity(rendered.len());
    let mut copy_prefix_widths = Vec::with_capacity(rendered.len());
    for rendered_line in rendered {
        let mut line = rendered_line.line;
        if is_tool_groupable {
            strip_cell_local_tool_rail(&mut line);
        }
        lines.push(line);
        links.push(rendered_line.links);
        copy_prefix_widths.push(rendered_line.copy_prefix_width);
        copy_separators.push(rendered_line.copy_separator_after);
    }
    if fold_action == Some(CellFoldAction::Expand)
        && let Some(line) = lines.first()
    {
        let prefix = line.width().saturating_sub(compute_rail_prefix_width(line));
        *copy_prefix_widths
            .first_mut()
            .expect("fold affordance header") = prefix;
        links.first_mut().expect("fold affordance header").clear();
    }
    let is_empty = lines.is_empty();
    let ends_blank = last_line_is_blank(&lines);
    CachedCell {
        revision,
        lines: Arc::new(lines),
        links: Arc::new(links),
        copy_separators: Arc::new(copy_separators),
        copy_prefix_widths: Arc::new(copy_prefix_widths),
        is_empty,
        ends_blank,
        kind: TranscriptBlockKind::for_cell(cell),
        is_tool_groupable,
        fold_action,
        incremental_markdown: None,
        hot_tail_original: None,
    }
}

/// Strip the cell-local rail because the flat cache owns cross-cell grouping;
/// retaining both produces doubled prefixes such as `╭ ╭`.
fn strip_cell_local_tool_rail(line: &mut Line<'static>) {
    if line
        .spans
        .first()
        .is_some_and(|span| matches!(span.content.as_ref(), "─ " | "╭ " | "│ " | "╰ "))
    {
        line.spans.remove(0);
    }
}

fn last_line_is_blank(lines: &[Line<'static>]) -> bool {
    lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockSeparator {
    rows: usize,
    railed: bool,
}

fn separator_between(
    current: &CachedCell,
    next: &CachedCell,
    spacing: TranscriptSpacing,
) -> BlockSeparator {
    let boundary = transcript_boundary(
        current.kind,
        next.kind,
        same_tool_activity_group(current, next),
    );
    let mut rows = spacer_rows_for_boundary(boundary, spacing);
    // A cell ending blank already paid for separation; a grouped tool rail is
    // not blank and must remain continuous.
    if !current.ends_blank {
        return BlockSeparator {
            rows,
            railed: boundary == TranscriptBoundary::GroupedTool,
        };
    }
    if boundary == TranscriptBoundary::GroupedTool {
        return BlockSeparator { rows, railed: true };
    }
    rows = rows.saturating_sub(1);
    BlockSeparator {
        rows,
        railed: false,
    }
}

fn same_tool_activity_group(current: &CachedCell, next: &CachedCell) -> bool {
    // Durable Work receipts are persisted state, not another transient tool
    // action; crossing that semantic seam closes the rail even at compact.
    current.is_tool_groupable && next.is_tool_groupable && current.kind == next.kind
}

fn transcript_boundary(
    current: TranscriptBlockKind,
    next: TranscriptBlockKind,
    same_tool_group: bool,
) -> TranscriptBoundary {
    if same_tool_group {
        debug_assert_eq!(current, next);
        // Distinct calls sharing a rail are one compact activity group. The
        // rail itself carries the grouping; padding every low-level call would
        // recreate the density problem this boundary matrix exists to solve.
        return TranscriptBoundary::GroupedTool;
    }

    // User cells are the only unambiguous turn delimiter. Keep prompt→tool
    // distinct too: a model need not emit answer prose before acting.
    if current == TranscriptBlockKind::User || next == TranscriptBlockKind::User {
        return TranscriptBoundary::Turn;
    }

    // Successive reasoning or answer cells are one phase split across cells;
    // a blank row there would jitter the row budget during streaming.
    if current == next
        && matches!(
            current,
            TranscriptBlockKind::Reasoning | TranscriptBlockKind::Answer
        )
    {
        return TranscriptBoundary::Joined;
    }

    // Every other phase transition is reader-visible. In particular,
    // reasoning running into final prose was the density bug this matrix
    // exists to prevent.
    TranscriptBoundary::Activity
}

const fn spacer_rows_for_boundary(
    boundary: TranscriptBoundary,
    spacing: TranscriptSpacing,
) -> usize {
    match (boundary, spacing) {
        (TranscriptBoundary::Joined | TranscriptBoundary::GroupedTool, _) => 0,
        (TranscriptBoundary::Activity, TranscriptSpacing::Compact) => 0,
        (TranscriptBoundary::Activity, _) => BLOCK_SEPARATOR_ROWS,
        (TranscriptBoundary::Turn, TranscriptSpacing::Compact | TranscriptSpacing::Comfortable) => {
            BLOCK_SEPARATOR_ROWS
        }
        (TranscriptBoundary::Turn, TranscriptSpacing::Spacious) => BLOCK_SEPARATOR_ROWS + 1,
    }
}

fn previous_visible_cell(cells: &[CachedCell], cell_index: usize) -> Option<&CachedCell> {
    cells[..cell_index].iter().rev().find(|cell| !cell.is_empty)
}

fn next_visible_cell(cells: &[CachedCell], cell_index: usize) -> Option<&CachedCell> {
    cells
        .get(cell_index + 1..)?
        .iter()
        .find(|cell| !cell.is_empty)
}

fn tool_group_rail(
    cells: &[CachedCell],
    cell_index: usize,
    line_in_cell: usize,
    rendered_line_count: usize,
) -> Option<crate::tui::widgets::tool_card::CardRail> {
    let cached = cells.get(cell_index)?;
    if !cached.is_tool_groupable || rendered_line_count == 0 {
        return None;
    }

    let previous_shares_group = previous_visible_cell(cells, cell_index)
        .is_some_and(|previous| same_tool_activity_group(previous, cached));
    let next_shares_group = next_visible_cell(cells, cell_index)
        .is_some_and(|next| same_tool_activity_group(cached, next));
    let first_line_in_group = !previous_shares_group && line_in_cell == 0;
    let last_line_in_group = !next_shares_group && line_in_cell + 1 == rendered_line_count;

    let rail = match (first_line_in_group, last_line_in_group) {
        (true, true) if rendered_line_count == 1 => {
            crate::tui::widgets::tool_card::CardRail::Single
        }
        (true, _) => crate::tui::widgets::tool_card::CardRail::Top,
        (_, true) => crate::tui::widgets::tool_card::CardRail::Bottom,
        _ => crate::tui::widgets::tool_card::CardRail::Middle,
    };
    Some(rail)
}

fn line_with_group_rail(
    line: &Line<'static>,
    rail: Option<crate::tui::widgets::tool_card::CardRail>,
    max_width: usize,
) -> Line<'static> {
    let Some(rail) = rail else {
        return line.clone();
    };
    let glyph = crate::tui::widgets::tool_card::rail_glyph(rail);
    if glyph.is_empty() {
        let mut rendered = line.clone();
        rendered.spans = truncate_spans_to_width(rendered.spans, max_width);
        return rendered;
    }

    let mut rendered = line.clone();
    let mut spans = Vec::with_capacity(rendered.spans.len() + 1);
    spans.push(Span::styled(
        format!("{glyph} "),
        Style::default().fg(codewhale_palette::TEXT_DIM),
    ));
    spans.extend(rendered.spans);
    rendered.spans = truncate_spans_to_width(spans, max_width);
    rendered
}

fn links_with_group_rail(
    links: &[crate::tui::osc8::LineLink],
    rail: Option<crate::tui::widgets::tool_card::CardRail>,
    max_width: usize,
) -> Vec<crate::tui::osc8::LineLink> {
    let shift = rail
        .map(crate::tui::widgets::tool_card::rail_glyph)
        .filter(|glyph| !glyph.is_empty())
        .map_or(0, |glyph| unicode_width::UnicodeWidthStr::width(glyph) + 1);
    links
        .iter()
        .map(|link| link.shifted(shift))
        .filter(|link| link.col_start < max_width)
        .map(|mut link| {
            link.col_end = link.col_end.min(max_width.saturating_sub(1));
            link
        })
        .collect()
}

/// Return the display-column count of consecutive visual-only decorative
/// spans at the start of a rendered transcript line. Iterates through
/// leading spans matching either of two patterns:
///
/// * Pattern A — span is `"<glyph>[<glyph>…]<space>"` where every character
///   except the trailing space is a rail-drawing character (e.g. `▏ `,
///   `▶ `, `⋮⋮ `). The entire span width is accumulated.
/// * Pattern B — span is `"<glyph>"` (1 drawing char) followed by a lone
///   space span `" "` (e.g. `●` then ` `, `▎` then ` `).
///
/// Stops at the first non-matching span. Every decorated glyph used by the
/// TUI is a single display-column character, so char-count = display width.
///
/// Returns `0` for lines whose first span is not a decorative prefix.
fn compute_rail_prefix_width(line: &Line<'static>) -> usize {
    let spans = line.spans.as_slice();
    let mut total = 0;
    let mut i = 0;

    while i < spans.len() {
        let content = spans[i].content.as_ref();
        let n_chars = content.chars().count();

        // Pattern A — span "<glyph>[<glyph>…]<space>" (≥ 2 chars, trailing
        // space, all preceding chars are drawing chars).
        if n_chars >= 2
            && content.ends_with(' ')
            && content
                .chars()
                .take(n_chars.saturating_sub(1))
                .all(is_rail_drawing_char)
        {
            total += n_chars;
            i += 1;
            continue;
        }

        // Pattern B — span "<glyph>" (1 drawing char) + next span " ".
        if n_chars == 1
            && content.chars().next().is_some_and(is_rail_drawing_char)
            && spans.get(i + 1).is_some_and(|s| s.content.as_ref() == " ")
        {
            total += 2;
            i += 2;
            continue;
        }

        break;
    }

    total
}

/// Characters that serve as decoration glyphs in the TUI left-rail and
/// tool-header prefix system. All are single display-column characters.
fn is_rail_drawing_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{2500}'..='\u{257F}'   // Box Drawing (╭ ╮ ╰ ╯ │ ╎ …)
        | '\u{2580}'..='\u{259F}' // Block Elements (▏ ▎ ▍ ▌ …)
        | '\u{25A0}'..='\u{25FF}' // Geometric Shapes (● ▶ ▷ ◆ ◐ …)
        | '\u{2022}'              // • bullet (tool status / generic tool)
        | '\u{2026}'              // … ellipsis (reasoning opener)
        | '\u{00B7}'              // · middle dot (tool running symbol)
        | '\u{2315}'              // ⌕ telephone recorder (find/search tool)
        | '\u{22EE}'              // ⋮ vertical ellipsis (fanout/rlm tool)
    )
}

fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    if max_width == 0 || spans.is_empty() {
        return Vec::new();
    }
    let current_width: usize = spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if current_width <= max_width {
        return spans;
    }

    let ellipsis = if max_width > 3 { "..." } else { "" };
    let content_budget = max_width.saturating_sub(ellipsis.len());
    let mut used = 0usize;
    let mut truncated = Vec::with_capacity(spans.len() + usize::from(!ellipsis.is_empty()));
    let mut last_style = Style::default();

    'outer: for span in spans {
        last_style = span.style;
        let mut content = String::new();
        for ch in span.content.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > content_budget {
                if !content.is_empty() {
                    truncated.push(Span::styled(content, span.style));
                }
                break 'outer;
            }
            content.push(ch);
            used += width;
        }
        if !content.is_empty() {
            truncated.push(Span::styled(content, span.style));
        }
    }

    if !ellipsis.is_empty() {
        truncated.push(Span::styled(ellipsis.to_string(), last_style));
    }
    truncated
}

#[cfg(test)]
#[path = "transcript/tests.rs"]
mod tests;

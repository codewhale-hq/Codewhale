//! Scroll state tracking for transcript rendering.
//!
//! The transcript view uses a flat line-index scroll model: a single `offset`
//! into the rendered line-meta buffer points at the top visible line, with
//! `usize::MAX` reserved as a sentinel meaning "stuck to the live tail."
//!
//! Why a flat offset, not cell anchors? An earlier design anchored the
//! viewport to a `(cell_index, line_in_cell)` pair on the assumption that
//! the cell list was append-only. It is not — content rewrites (RLM `repl`
//! blocks expanding into `Thinking + Text`, tool result replacements, and
//! compaction) can renumber or remove cells underneath the user. When the
//! anchor cell vanished the viewport teleported to the bottom (issue #56)
//! or "got stuck" because the next keypress would resolve from `max_start`.
//!
//! Codex's pager uses the same line-offset shape; see
//! `codex-rs/tui/src/pager_overlay.rs::PagerView`.

use std::time::{Duration, Instant};

use crate::ui_text::CopyLineSeparator;

const TRACKPAD_EVENT_WINDOW: Duration = Duration::from_millis(35);
const WHEEL_LINES_PER_TICK: i32 = 3;
const TRACKPAD_BASE_LINES_PER_TICK: i32 = 1;
const TRACKPAD_MID_LINES_PER_TICK: i32 = 2;
const TRACKPAD_MAX_LINES_PER_TICK: i32 = 3;

// === Transcript Line Metadata ===

/// Metadata describing how rendered transcript lines map to history cells.
///
/// The scroll state itself does not consult this — it only stores a flat
/// line offset — but other render-time helpers (selection painting,
/// send-flash, jump-to-tool, scrollbar percent) still need the
/// line→cell mapping the cache exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptLineMeta {
    CellLine {
        cell_index: usize,
        line_in_cell: usize,
        copy_prefix_width: usize,
        copy_separator_after: CopyLineSeparator,
    },
    /// A block separator row inserted between two cells. Usually empty, but
    /// separators inside a tool-card rail group carry the rail glyph so the
    /// card box survives the gap — hence a copy prefix to strip.
    Spacer { copy_prefix_width: usize },
}

impl TranscriptLineMeta {
    /// Return cell/line indices if this entry is a cell line.
    #[must_use]
    pub fn cell_line(&self) -> Option<(usize, usize)> {
        match *self {
            TranscriptLineMeta::CellLine {
                cell_index,
                line_in_cell,
                ..
            } => Some((cell_index, line_in_cell)),
            TranscriptLineMeta::Spacer { .. } => None,
        }
    }

    #[must_use]
    pub fn copy_separator_after(&self) -> CopyLineSeparator {
        match *self {
            TranscriptLineMeta::CellLine {
                copy_separator_after,
                ..
            } => copy_separator_after,
            TranscriptLineMeta::Spacer { .. } => CopyLineSeparator::Newline,
        }
    }

    #[must_use]
    pub fn copy_prefix_width(&self) -> usize {
        match *self {
            TranscriptLineMeta::CellLine {
                copy_prefix_width, ..
            } => copy_prefix_width,
            TranscriptLineMeta::Spacer { copy_prefix_width } => copy_prefix_width,
        }
    }
}

// === Transcript Scroll State ===

/// Sentinel offset meaning "stuck to live tail" — the renderer translates
/// this to `max_start` at draw time, so newly appended lines pull the view
/// down with them.
const TAIL_SENTINEL: usize = usize::MAX;

/// Flat line-offset scroll state for the transcript view.
///
/// Stores the index of the top visible line into the cache's `line_meta`
/// buffer, or `TAIL_SENTINEL` (`usize::MAX`) to mean "stuck to bottom."
/// The renderer resolves the sentinel against the current line count and
/// viewport height every frame, so content rewrites simply clamp the
/// user's offset rather than triggering anchor recovery heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptScroll {
    offset: usize,
}

impl Default for TranscriptScroll {
    /// Default state is "stuck to live tail" — matches the historical
    /// `TranscriptScroll::ToBottom` behaviour callers already depend on.
    fn default() -> Self {
        Self::to_bottom()
    }
}

impl TranscriptScroll {
    /// State that follows the live tail (default).
    #[must_use]
    pub const fn to_bottom() -> Self {
        Self {
            offset: TAIL_SENTINEL,
        }
    }

    /// State pinned to a specific line index.
    #[must_use]
    pub const fn at_line(offset: usize) -> Self {
        Self { offset }
    }

    /// Returns true when the view is following the live tail.
    #[must_use]
    pub const fn is_at_tail(self) -> bool {
        self.offset == TAIL_SENTINEL
    }

    /// Resolve the scroll state to a concrete top line index.
    ///
    /// `max_start` is `total_lines.saturating_sub(visible_lines)`. The
    /// returned `Self` is the canonicalized state — if the resolved top
    /// reached the tail (or the transcript fits in one screen) we collapse
    /// to [`TranscriptScroll::to_bottom`], so the caller can treat the
    /// returned state as authoritative.
    ///
    /// `line_meta` is accepted for API compatibility with the previous
    /// cell-anchored implementation. It is unused here because the flat
    /// offset model needs no cell-index lookup; we just clamp.
    #[must_use]
    pub fn resolve_top(self, line_meta: &[TranscriptLineMeta], max_start: usize) -> (Self, usize) {
        let _ = line_meta;
        if self.offset == TAIL_SENTINEL {
            return (Self::to_bottom(), max_start);
        }
        let top = self.offset.min(max_start);
        if top >= max_start {
            (Self::to_bottom(), max_start)
        } else {
            (Self::at_line(top), top)
        }
    }

    /// Apply a scroll delta and return the updated state.
    ///
    /// `delta_lines` is signed: negative scrolls up (toward the start),
    /// positive scrolls down (toward the tail). When the resolved offset
    /// hits `max_start` we snap to [`TranscriptScroll::to_bottom`] so
    /// subsequent appended content pulls the view along.
    ///
    /// `line_meta` is accepted for API compatibility; only its length is
    /// consulted. `visible_lines` controls the page size for clamping.
    #[must_use]
    pub fn scrolled_by(
        self,
        delta_lines: i32,
        line_meta: &[TranscriptLineMeta],
        visible_lines: usize,
    ) -> Self {
        if delta_lines == 0 {
            return self;
        }

        let total_lines = line_meta.len();
        if total_lines <= visible_lines {
            // Whole transcript fits; only "tail" is meaningful.
            return Self::to_bottom();
        }

        let max_start = total_lines.saturating_sub(visible_lines);
        let current_top = if self.offset == TAIL_SENTINEL {
            max_start
        } else {
            self.offset.min(max_start)
        };

        let new_top = if delta_lines < 0 {
            current_top.saturating_sub(delta_lines.unsigned_abs() as usize)
        } else {
            let delta = usize::try_from(delta_lines).unwrap_or(usize::MAX);
            current_top.saturating_add(delta).min(max_start)
        };

        if new_top >= max_start {
            Self::to_bottom()
        } else {
            Self::at_line(new_top)
        }
    }

    /// Pin the scroll state to a specific line index in the rendered
    /// transcript (saturating to the meta buffer length).
    ///
    /// Returns `None` if `line_meta` is empty (caller should default to
    /// [`TranscriptScroll::to_bottom`] in that case).
    #[must_use]
    pub fn anchor_for(line_meta: &[TranscriptLineMeta], start: usize) -> Option<Self> {
        if line_meta.is_empty() {
            return None;
        }
        let clamped = start.min(line_meta.len().saturating_sub(1));
        Some(Self::at_line(clamped))
    }
}

/// Direction for mouse scroll input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    fn sign(self) -> i32 {
        match self {
            ScrollDirection::Up => -1,
            ScrollDirection::Down => 1,
        }
    }
}

/// Stateful tracker for mouse scroll accumulation.
#[derive(Debug, Default)]
pub struct MouseScrollState {
    last_event_at: Option<Instant>,
    last_direction: Option<ScrollDirection>,
    rapid_same_direction_ticks: u8,
}

/// A computed scroll delta from user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollUpdate {
    pub delta_lines: i32,
}

impl MouseScrollState {
    /// Create a new scroll state tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a scroll event and return the resulting delta.
    pub fn on_scroll(&mut self, direction: ScrollDirection) -> ScrollUpdate {
        let now = Instant::now();
        self.on_scroll_at(direction, now)
    }

    fn on_scroll_at(&mut self, direction: ScrollDirection, now: Instant) -> ScrollUpdate {
        let is_trackpad = self
            .last_event_at
            .is_some_and(|last| now.saturating_duration_since(last) < TRACKPAD_EVENT_WINDOW);
        let same_direction = self.last_direction == Some(direction);

        self.last_event_at = Some(now);
        self.last_direction = Some(direction);

        let lines_per_tick = if is_trackpad {
            if same_direction {
                self.rapid_same_direction_ticks = self.rapid_same_direction_ticks.saturating_add(1);
            } else {
                self.rapid_same_direction_ticks = 1;
            }
            match self.rapid_same_direction_ticks {
                0..=2 => TRACKPAD_BASE_LINES_PER_TICK,
                3..=5 => TRACKPAD_MID_LINES_PER_TICK,
                _ => TRACKPAD_MAX_LINES_PER_TICK,
            }
        } else {
            self.rapid_same_direction_ticks = 0;
            WHEEL_LINES_PER_TICK
        };

        ScrollUpdate {
            delta_lines: direction.sign() * lines_per_tick,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_line(cell_index: usize, line_in_cell: usize) -> TranscriptLineMeta {
        TranscriptLineMeta::CellLine {
            cell_index,
            line_in_cell,
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        }
    }

    /// Build a synthetic line-meta array for a transcript with `cell_count`
    /// cells, each `lines_per_cell` lines tall, separated by spacers.
    fn synth_line_meta(cell_count: usize, lines_per_cell: usize) -> Vec<TranscriptLineMeta> {
        let mut meta = Vec::new();
        for cell in 0..cell_count {
            for line in 0..lines_per_cell {
                meta.push(cell_line(cell, line));
            }
            if cell + 1 < cell_count {
                meta.push(TranscriptLineMeta::Spacer {
                    copy_prefix_width: 0,
                });
            }
        }
        meta
    }

    /// Default state follows the live tail. Resolving against any
    /// `max_start` returns `max_start` and the canonical tail state.
    #[test]
    fn default_state_is_tail() {
        let state = TranscriptScroll::default();
        assert!(state.is_at_tail());
        let meta = synth_line_meta(5, 3);
        let max_start = 6;
        let (resolved, top) = state.resolve_top(&meta, max_start);
        assert!(resolved.is_at_tail());
        assert_eq!(top, max_start);
    }

    /// A pinned offset below `max_start` resolves to itself unchanged.
    /// (Originally: "anchor cell still exists" — same intent: scroll
    /// position is preserved when it is still valid.)
    #[test]
    fn resolve_top_keeps_position_when_offset_in_range() {
        let meta = synth_line_meta(5, 3); // 19 entries
        let max_start = meta.len().saturating_sub(8);
        let state = TranscriptScroll::at_line(9);
        let (resolved, top) = state.resolve_top(&meta, max_start);
        assert_eq!(resolved, TranscriptScroll::at_line(9));
        assert_eq!(top, 9);
    }

    /// Regression for issue #56: when a content rewrite shrinks the
    /// transcript so the user's offset is past the new `max_start`, we
    /// clamp to the new max — we must NOT teleport to the top, and we
    /// must NOT silently lose the position by sending the user to the
    /// raw bottom of pre-rewrite content. Snapping to the tail is the
    /// correct behaviour because the user's intended position no longer
    /// has any content under it.
    #[test]
    fn resolve_top_clamps_when_offset_past_max_start() {
        let meta = synth_line_meta(3, 2); // 8 entries (cells 0..3, 2 lines + 2 spacers)
        let max_start = meta.len().saturating_sub(4);
        // User had scrolled to a line that no longer exists post-rewrite.
        let state = TranscriptScroll::at_line(15);
        let (resolved, top) = state.resolve_top(&meta, max_start);
        // Past max_start collapses to tail (which is the right answer:
        // there is no content beyond max_start to show).
        assert!(resolved.is_at_tail());
        assert_eq!(top, max_start);
    }

    /// Regression for the new bug we are guarding against in this
    /// refactor: scrolling up to mid-transcript, having the content
    /// rewrite under us, and then drawing again must preserve the
    /// offset (clamped if needed) and NOT teleport to top or to bottom
    /// when the offset is still in-range.
    #[test]
    fn resolve_top_preserves_midway_offset_after_content_rewrite() {
        // Pre-rewrite transcript: 10 cells × 3 lines + 9 spacers = 39 lines.
        let pre = synth_line_meta(10, 3);
        let visible = 8;
        let pre_max_start = pre.len().saturating_sub(visible);

        // User scrolls up to a midway line (line 12).
        let state = TranscriptScroll::at_line(12);
        let (state, top_before) = state.resolve_top(&pre, pre_max_start);
        assert_eq!(top_before, 12);
        assert_eq!(state, TranscriptScroll::at_line(12));

        // Content rewrite: cell 4 expanded by two lines (e.g. inline
        // RLM `repl` block became Thinking + Text). Total grows.
        let mut post = pre.clone();
        post.insert(13, cell_line(4, 3));
        post.insert(14, cell_line(4, 4));
        let post_max_start = post.len().saturating_sub(visible);
        let (state2, top_after) = state.resolve_top(&post, post_max_start);
        // Critical: still at line 12, not pulled to bottom or top.
        assert_eq!(state2, TranscriptScroll::at_line(12));
        assert_eq!(top_after, 12);

        // Content rewrite shrunk transcript below the offset.
        let post_shrunk = synth_line_meta(3, 3); // 11 lines total
        let shrunk_max_start = post_shrunk.len().saturating_sub(visible);
        let (state3, top_shrunk) = state.resolve_top(&post_shrunk, shrunk_max_start);
        // Offset 12 > 11; we clamp to tail (no content beyond max_start).
        assert!(state3.is_at_tail());
        assert_eq!(top_shrunk, shrunk_max_start);
    }

    /// `scrolled_by` from a stale offset: pressing Up should still move
    /// the user up, not lock them at the bottom. The flat-offset model
    /// makes this trivial — the offset is simply clamped to `max_start`
    /// before applying the delta.
    #[test]
    fn scrolled_by_does_not_teleport_on_stale_offset() {
        let meta = synth_line_meta(3, 2); // 8 entries
        let visible = 4;
        let max_start = meta.len().saturating_sub(visible);
        // User had scrolled past the new end of transcript.
        let stale = TranscriptScroll::at_line(20);
        let new_state = stale.scrolled_by(-1, &meta, visible);
        // Either ends up Scrolled near the bottom (max_start - 1) or
        // already at tail if max_start was 0.
        if meta.len() > visible {
            // Should be at max_start - 1 = 3.
            assert_eq!(new_state, TranscriptScroll::at_line(max_start - 1));
        }
    }

    /// When the transcript fits entirely in the viewport, scrolled_by
    /// always collapses to tail.
    #[test]
    fn scrolled_by_collapses_to_bottom_when_view_fits() {
        let meta = synth_line_meta(2, 2);
        let visible = meta.len() + 5;
        let state = TranscriptScroll::at_line(0);
        let new_state = state.scrolled_by(-1, &meta, visible);
        assert!(new_state.is_at_tail());
    }

    /// `scrolled_by` from tail with positive delta stays at tail (we
    /// can't scroll past the bottom).
    #[test]
    fn scrolled_by_from_tail_down_stays_at_tail() {
        let meta = synth_line_meta(5, 3);
        let visible = 6;
        let state = TranscriptScroll::to_bottom();
        let new_state = state.scrolled_by(5, &meta, visible);
        assert!(new_state.is_at_tail());
    }

    /// `scrolled_by` from tail with negative delta moves up by |delta|
    /// from `max_start`.
    #[test]
    fn scrolled_by_from_tail_up_walks_back_from_max_start() {
        let meta = synth_line_meta(5, 3); // 19 entries
        let visible = 6;
        let max_start = meta.len().saturating_sub(visible);
        let state = TranscriptScroll::to_bottom();
        let new_state = state.scrolled_by(-3, &meta, visible);
        assert_eq!(new_state, TranscriptScroll::at_line(max_start - 3));
    }

    /// `anchor_for` clamps the requested start into the meta range and
    /// produces a pinned state.
    #[test]
    fn anchor_for_clamps_start_into_range() {
        let meta = synth_line_meta(4, 1);
        let anchor = TranscriptScroll::anchor_for(&meta, 0).expect("non-empty");
        assert_eq!(anchor, TranscriptScroll::at_line(0));

        let anchor = TranscriptScroll::anchor_for(&meta, 1_000_000).expect("non-empty");
        assert_eq!(
            anchor,
            TranscriptScroll::at_line(meta.len().saturating_sub(1))
        );
    }

    /// Empty `line_meta` returns `None` so callers can fall back to
    /// [`TranscriptScroll::to_bottom`].
    #[test]
    fn anchor_for_empty_returns_none() {
        let meta: Vec<TranscriptLineMeta> = Vec::new();
        assert!(TranscriptScroll::anchor_for(&meta, 0).is_none());
    }

    /// Tail state resolves to `max_start` regardless of the `line_meta`
    /// contents.
    #[test]
    fn to_bottom_resolves_to_max_start() {
        let meta = synth_line_meta(5, 2);
        let max_start = 7;
        let (state, top) = TranscriptScroll::to_bottom().resolve_top(&meta, max_start);
        assert!(state.is_at_tail());
        assert_eq!(top, max_start);
    }

    #[test]
    fn mouse_scroll_single_wheel_tick_moves_three_lines() {
        let mut state = MouseScrollState::new();
        let start = Instant::now();

        assert_eq!(
            state.on_scroll_at(ScrollDirection::Down, start).delta_lines,
            3
        );
        assert_eq!(
            state.on_scroll_at(ScrollDirection::Up, start).delta_lines,
            -1,
            "same timestamp is treated as a rapid precise input"
        );
    }

    #[test]
    fn mouse_scroll_rapid_same_direction_accelerates_but_caps() {
        let mut state = MouseScrollState::new();
        let start = Instant::now();

        let deltas = [
            state.on_scroll_at(ScrollDirection::Down, start).delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(10))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(20))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(30))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(40))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(50))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(60))
                .delta_lines,
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(70))
                .delta_lines,
        ];

        assert_eq!(deltas, [3, 1, 1, 2, 2, 2, 3, 3]);
    }

    #[test]
    fn mouse_scroll_direction_change_resets_acceleration() {
        let mut state = MouseScrollState::new();
        let start = Instant::now();

        for step in 0..8 {
            let _ = state.on_scroll_at(
                ScrollDirection::Down,
                start + Duration::from_millis(step * 10),
            );
        }

        assert_eq!(
            state
                .on_scroll_at(ScrollDirection::Up, start + Duration::from_millis(90))
                .delta_lines,
            -1
        );
    }

    #[test]
    fn mouse_scroll_slow_gap_resets_to_wheel_tick() {
        let mut state = MouseScrollState::new();
        let start = Instant::now();

        assert_eq!(
            state.on_scroll_at(ScrollDirection::Down, start).delta_lines,
            3
        );
        assert_eq!(
            state
                .on_scroll_at(ScrollDirection::Down, start + Duration::from_millis(100))
                .delta_lines,
            3
        );
    }
}

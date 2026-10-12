//! Text selection state for the transcript view.

use std::time::Instant;

// === Types ===

/// A selection endpoint in the transcript (line/column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptSelectionPoint {
    pub line_index: usize,
    pub column: usize,
}

/// Current selection state in the transcript view.
#[derive(Debug, Clone, Copy, Default)]
pub struct TranscriptSelection {
    pub anchor: Option<TranscriptSelectionPoint>,
    pub head: Option<TranscriptSelectionPoint>,
    pub dragging: bool,
    pub(crate) owner: Option<crate::tui::history::TranscriptActionOwner>,
}

/// Drag-past-edge auto-scroll state. While the user holds the left button
/// and the cursor is above or below the transcript rect, the main loop
/// advances `pending_scroll_delta` and extends the selection head on a
/// fixed cadence so a long passage can be selected in one drag (#1163).
#[derive(Debug, Clone, Copy)]
pub struct SelectionAutoscroll {
    /// `-1` scrolls up, `+1` scrolls down. Never `0`.
    pub direction: i32,
    /// Last in-bounds mouse column, in absolute terminal coordinates.
    pub column: u16,
    /// When the next tick is allowed to fire.
    pub next_tick: Instant,
}

impl TranscriptSelection {
    /// Clear any active selection.
    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
        self.owner = None;
    }

    /// Whether a full selection is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    /// Return selection endpoints ordered from start to end.
    #[must_use]
    pub fn ordered_endpoints(
        &self,
    ) -> Option<(TranscriptSelectionPoint, TranscriptSelectionPoint)> {
        let anchor = self.anchor?;
        let head = self.head?;
        if (head.line_index, head.column) < (anchor.line_index, anchor.column) {
            Some((head, anchor))
        } else {
            Some((anchor, head))
        }
    }
}

//! Wrapped-line cache for the live transcript overlay (#94).
//!
//! Each cell's rendered output is cached under a `(CellId, width, revision)`
//! key. The revision portion comes from `App.history_revisions` (or the
//! synthetic active-cell revision); the cache invalidates entries the moment
//! a cell mutates because the upstream tag changes. Width changes invalidate
//! everything for that cell because wrap layout depends on width.
//!
//! Live cells (the streaming assistant body, in-flight tool entries) bump
//! their revision on every mutation, so the cache always reflects the latest
//! frame of their output without ever paying for a re-wrap of unrelated
//! cells. Resize-driven re-wrap is bounded to the cells whose width key just
//! changed; nothing else is invalidated.
//!
//! The cache is bounded to keep memory predictable on long sessions.
//! Eviction is a simple insertion-order scheme — a strict LRU would be
//! overkill for the access pattern (full sweep on every render frame) —
//! and the owner sizes the cap to the sweep with
//! [`TranscriptCache::ensure_capacity`] so a sweep never evicts itself.

use std::collections::HashMap;
use std::collections::VecDeque;

use ratatui::text::Line;

#[derive(Debug, Clone)]
pub(crate) struct CachedTranscriptLine {
    pub line: Line<'static>,
    pub links: Vec<crate::tui::osc8::LineLink>,
}

/// Soft cap on the number of cached entries before insertion-order eviction
/// kicks in. Sized for the worst-case "5,000-line transcript at 200 cells,
/// resize twice" pattern; well under a megabyte even with 10 KB cells.
const DEFAULT_CAPACITY: usize = 512;

/// Identifier for a transcript cell within a live render. `History(idx)`
/// addresses a finalized history cell at the given index;
/// `Active(entry_idx)` addresses the synthetic active-cell entry while a
/// turn is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellId {
    History(usize),
    Active(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    cell: CellId,
    width: u16,
    revision: u64,
}

/// Bounded cache of wrapped lines. Keyed by `(cell_id, width, revision)` —
/// any change to a cell's revision (mutation), the terminal width (resize),
/// or the cell's identity (insert/delete shifting indices) misses the cache.
#[derive(Debug)]
pub struct TranscriptCache {
    capacity: usize,
    entries: HashMap<Key, Vec<CachedTranscriptLine>>,
    /// Insertion order so we can evict the oldest entry when full. Two-step
    /// (HashMap + VecDeque) so insertion is O(1) and lookup stays O(1).
    insertion_order: VecDeque<Key>,
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl TranscriptCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::with_capacity(capacity.max(1)),
            insertion_order: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    /// Grow the cap to at least `min_capacity`; never shrinks. The overlay
    /// sweeps every cell each rebuild, and insertion-order eviction under a
    /// cap smaller than that sweep evicts each entry before its next use —
    /// every lookup misses. Sizing to the sweep keeps the scheme simple and
    /// the hit rate whole.
    pub fn ensure_capacity(&mut self, min_capacity: usize) {
        self.capacity = self.capacity.max(min_capacity);
    }

    /// Look up wrapped lines previously rendered at this exact key. Returns
    /// `None` if the cell never wrapped at this width/revision before.
    #[must_use]
    pub fn get(&self, cell: CellId, width: u16, revision: u64) -> Option<&[CachedTranscriptLine]> {
        let key = Key {
            cell,
            width,
            revision,
        };
        self.entries.get(&key).map(Vec::as_slice)
    }

    /// Cache a fresh wrap result. If the cache is at capacity the oldest
    /// inserted entry is evicted first.
    pub fn insert(
        &mut self,
        cell: CellId,
        width: u16,
        revision: u64,
        lines: Vec<CachedTranscriptLine>,
    ) {
        let key = Key {
            cell,
            width,
            revision,
        };
        // Replace an existing key in place — keep its position in the
        // insertion-order queue so we don't trigger spurious eviction.
        if self.entries.insert(key, lines).is_some() {
            return;
        }
        if self.entries.len() > self.capacity
            && let Some(oldest) = self.insertion_order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.insertion_order.push_back(key);
    }

    /// Drop every cached entry. Used when the underlying transcript shape
    /// changes drastically (e.g. session reset).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn line(s: &str) -> CachedTranscriptLine {
        CachedTranscriptLine {
            line: Line::from(Span::raw(s.to_string())),
            links: Vec::new(),
        }
    }

    #[test]
    fn miss_returns_none() {
        let cache = TranscriptCache::new();
        assert!(cache.get(CellId::History(0), 80, 1).is_none());
    }

    #[test]
    fn round_trip_returns_inserted_lines() {
        let mut cache = TranscriptCache::new();
        let lines = vec![line("hello"), line("world")];
        cache.insert(CellId::History(0), 80, 1, lines.clone());
        let got = cache
            .get(CellId::History(0), 80, 1)
            .expect("entry should be cached");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].line.spans[0].content, "hello");
    }

    #[test]
    fn revision_bump_invalidates_cell() {
        let mut cache = TranscriptCache::new();
        cache.insert(CellId::History(0), 80, 1, vec![line("v1")]);
        // Hit at rev=1
        assert!(cache.get(CellId::History(0), 80, 1).is_some());
        // Miss at rev=2 — caller is expected to re-wrap and insert again.
        assert!(cache.get(CellId::History(0), 80, 2).is_none());
    }

    #[test]
    fn width_change_invalidates_cell() {
        let mut cache = TranscriptCache::new();
        cache.insert(CellId::History(0), 80, 1, vec![line("v1")]);
        assert!(cache.get(CellId::History(0), 80, 1).is_some());
        assert!(cache.get(CellId::History(0), 100, 1).is_none());
    }

    #[test]
    fn active_cells_are_distinct_from_history() {
        let mut cache = TranscriptCache::new();
        cache.insert(CellId::History(0), 80, 1, vec![line("history")]);
        cache.insert(CellId::Active(0), 80, 1, vec![line("active")]);
        assert_eq!(
            cache.get(CellId::History(0), 80, 1).unwrap()[0].line.spans[0].content,
            "history"
        );
        assert_eq!(
            cache.get(CellId::Active(0), 80, 1).unwrap()[0].line.spans[0].content,
            "active"
        );
    }

    #[test]
    fn reinsert_same_key_does_not_evict() {
        // Capacity 2 — re-inserting an existing key must not cause the other
        // entry to be evicted; otherwise re-rendering the same cell on every
        // frame would churn unrelated entries out of the cache.
        let mut cache = TranscriptCache::with_capacity(2);
        cache.insert(CellId::History(0), 80, 1, vec![line("a")]);
        cache.insert(CellId::History(1), 80, 1, vec![line("b")]);
        cache.insert(CellId::History(0), 80, 1, vec![line("a-prime")]);
        assert!(cache.get(CellId::History(1), 80, 1).is_some());
    }

    #[test]
    fn capacity_evicts_oldest_on_overflow() {
        let mut cache = TranscriptCache::with_capacity(2);
        cache.insert(CellId::History(0), 80, 1, vec![line("a")]);
        cache.insert(CellId::History(1), 80, 1, vec![line("b")]);
        cache.insert(CellId::History(2), 80, 1, vec![line("c")]);
        // Oldest (History(0)) should be gone; the two newer keys remain.
        assert!(cache.get(CellId::History(0), 80, 1).is_none());
        assert!(cache.get(CellId::History(1), 80, 1).is_some());
        assert!(cache.get(CellId::History(2), 80, 1).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn clear_drops_everything() {
        let mut cache = TranscriptCache::new();
        cache.insert(CellId::History(0), 80, 1, vec![line("v1")]);
        cache.clear();
        assert!(cache.get(CellId::History(0), 80, 1).is_none());
        assert_eq!(cache.len(), 0);
    }
}

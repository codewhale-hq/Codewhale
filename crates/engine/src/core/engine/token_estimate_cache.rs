//! Process-local memoization for [`crate::compaction::estimate_input_tokens_for_pressure`].
//!
//! This is the engine's one pressure number: the same un-inflated estimate the
//! auto-compaction gate, the compaction preflight, the TUI context meter and
//! the `/context` headline read, so compaction receipts and the context-budget
//! snapshot agree with them (0.10.1 item 9). The 1.5x-inflated
//! `estimate_input_tokens_conservative` is request-overflow protection only
//! and is deliberately not cached here.
//!
//! The token estimator walks the full [`codewhale_models::Message`] history and the
//! active system prompt, which is by far the most expensive per-turn CPU cost
//! in the engine hot path. The same input data is queried from at least five
//! sites per turn: capacity pre/post tool checkpoints, error escalation,
//! the seam manager, and the trimmed-message budget check, plus four more
//! from the TUI footer, `/status`, `/debug`, and the context inspector.
//!
//! Without memoization, a 200-message history with 5 KB of tool results costs
//! ~2 ms per call; that is 20 ms of pure waste on a single turn. The estimator
//! itself is a pure function of `(messages, system_prompt)`, so a
//! content-versioned cache is safe: the caller bumps `messages_revision`
//! on every mutation, and we also include a fast fingerprint of the system
//! prompt as part of the key.
//!
//! The cache is process-local only — cross-session persistence is intentionally
//! out of scope (see PR #2520 for the cross-session prompt-base disk cache).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::compaction::estimate_input_tokens_for_pressure;
use codewhale_models::{Message, SystemPrompt};

/// Default capacity for the rolling audit ring. Sized so a 64-entry window
/// covers a full capacity controller observation cycle without unbounded
/// growth on long-running sessions.
const AUDIT_RING_CAPACITY: usize = 64;

/// Process-local memoization for `estimate_input_tokens_for_pressure`.
///
/// The cache is keyed on the `(messages_revision, system_fingerprint)`
/// pair, both of which the engine bumps on every content change. On a hit
/// the previously stored token estimate is returned without re-walking the
/// message list. On a miss, the estimator runs and the result is stored
/// alongside the audit ring entry.
#[derive(Debug, Default, Clone)]
pub struct TokenEstimateCache {
    /// Monotonic counter bumped by the engine on every message mutation.
    messages_revision: u64,
    /// Stable 64-bit hash of the current system prompt text. Computed once
    /// per `lookup_or_compute` call when the cache misses.
    system_fingerprint: u64,
    /// Cached token count, valid iff both keys match the current inputs.
    cached_tokens: Option<usize>,
    /// Audit ring of recent (revision, tokens) pairs. The most recent entry
    /// is the tail; the oldest is dropped when capacity is exceeded. Used by
    /// observability to surface cache effectiveness to `/status`.
    audit_ring: Vec<(u64, usize)>,
    /// Number of cache hits since the cache was last cleared. Saturates at
    /// `u64::MAX` (effectively never in practice).
    hits: u64,
    /// Number of cache misses since the cache was last cleared.
    misses: u64,
}

impl TokenEstimateCache {
    /// Construct a fresh, empty cache. `messages_revision` defaults to 0; the
    /// engine must call [`bump_messages_revision`](Self::bump_messages_revision)
    /// whenever a mutation occurs so the next lookup correctly invalidates.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached token estimate, recomputing on miss.
    ///
    /// `messages_revision` is the engine's monotonic counter; bump it on
    /// every add/remove/clear. `system_prompt` may be `None`. `messages` is
    /// borrowed for the duration of the call so a miss can re-tokenize.
    pub fn lookup_or_compute(
        &mut self,
        messages_revision: u64,
        system_prompt: Option<&SystemPrompt>,
        messages: &[Message],
    ) -> usize {
        let system_fingerprint = fingerprint_system_prompt(system_prompt);

        if self.messages_revision == messages_revision
            && self.system_fingerprint == system_fingerprint
            && let Some(tokens) = self.cached_tokens
        {
            self.hits = self.hits.saturating_add(1);
            return tokens;
        }

        let tokens = estimate_input_tokens_for_pressure(messages, system_prompt);
        self.messages_revision = messages_revision;
        self.system_fingerprint = system_fingerprint;
        self.cached_tokens = Some(tokens);
        self.misses = self.misses.saturating_add(1);
        self.push_audit(messages_revision, tokens);
        tokens
    }

    /// Record a messages-revision bump. The engine calls this whenever
    /// `session.messages` is mutated. Calling it with a value smaller than
    /// the current value is a no-op (the cache is monotonic).
    #[allow(dead_code)] // exposed for future wiring of /clear and reset paths; tests exercise it
    pub fn bump_messages_revision(&mut self, revision: u64) {
        if revision > self.messages_revision {
            self.messages_revision = revision;
            self.cached_tokens = None;
        }
    }

    /// Forget all cached state. Used by `/clear` and session reset paths.
    #[allow(dead_code)] // exposed for future wiring of /clear and reset paths; tests exercise it
    pub fn invalidate(&mut self) {
        self.cached_tokens = None;
        self.system_fingerprint = 0;
        self.audit_ring.clear();
        self.hits = 0;
        self.misses = 0;
    }

    /// Returns `(hits, misses)` counters since the last `invalidate` call.
    #[allow(dead_code)] // surfaced via /status in a follow-up; tests exercise it
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Returns the most recent `(revision, tokens)` audit entries, newest
    /// first. Bounded by [`AUDIT_RING_CAPACITY`].
    #[allow(dead_code)] // surfaced via /status in a follow-up; tests exercise it
    #[must_use]
    pub fn recent_audit(&self) -> &[(u64, usize)] {
        &self.audit_ring
    }

    fn push_audit(&mut self, revision: u64, tokens: usize) {
        if self.audit_ring.len() >= AUDIT_RING_CAPACITY {
            self.audit_ring.remove(0);
        }
        self.audit_ring.push((revision, tokens));
    }
}

/// Stable 64-bit hash of the system prompt text. Walks the same shape the
/// estimator consumes: a `Text` variant or a list of `Blocks`. Returns 0
/// for `None` so the empty case is distinguishable but cheap to compare.
fn fingerprint_system_prompt(system: Option<&SystemPrompt>) -> u64 {
    let Some(system) = system else {
        return 0;
    };
    let mut hasher = DefaultHasher::new();
    match system {
        SystemPrompt::Text(text) => {
            "text".hash(&mut hasher);
            text.hash(&mut hasher);
        }
        SystemPrompt::Blocks(blocks) => {
            "blocks".hash(&mut hasher);
            blocks.len().hash(&mut hasher);
            for block in blocks {
                block.block_type.hash(&mut hasher);
                block.text.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, SystemBlock};

    fn user_text(s: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: s.to_string(),
                cache_control: None,
            }],
        }
    }

    fn sys_text(s: &str) -> SystemPrompt {
        SystemPrompt::Text(s.to_string())
    }

    #[test]
    fn first_call_is_a_miss() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hello world")];
        let tokens = cache.lookup_or_compute(1, None, &messages);
        let (hits, misses) = cache.stats();
        assert!(tokens > 0);
        assert_eq!(hits, 0);
        assert_eq!(misses, 1);
    }

    #[test]
    fn repeated_call_with_same_revision_is_a_hit() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hello world")];
        let _ = cache.lookup_or_compute(1, None, &messages);
        let _ = cache.lookup_or_compute(1, None, &messages);
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
    }

    #[test]
    fn revision_bump_invalidates() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hi")];
        let a = cache.lookup_or_compute(1, None, &messages);
        let b = cache.lookup_or_compute(2, None, &messages);
        let (hits, misses) = cache.stats();
        // Both calls were misses (different revisions), neither hit the cache.
        assert_eq!(a, b);
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
    }

    #[test]
    fn system_prompt_change_invalidates() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hi")];
        let _ = cache.lookup_or_compute(1, Some(&sys_text("alpha")), &messages);
        let _ = cache.lookup_or_compute(1, Some(&sys_text("beta")), &messages);
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
    }

    #[test]
    fn bump_messages_revision_clears_cache() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("x")];
        let _ = cache.lookup_or_compute(1, None, &messages);
        cache.bump_messages_revision(2);
        let _ = cache.lookup_or_compute(2, None, &messages);
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
    }

    #[test]
    fn bump_to_smaller_revision_is_noop() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("x")];
        let _ = cache.lookup_or_compute(5, None, &messages);
        cache.bump_messages_revision(2);
        // revision went down, cache should still be valid for revision 5
        let _ = cache.lookup_or_compute(5, None, &messages);
        let (hits, _) = cache.stats();
        assert_eq!(hits, 1, "downward revision bumps must not invalidate");
    }

    #[test]
    fn invalidate_resets_state() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("x")];
        let _ = cache.lookup_or_compute(1, None, &messages);
        let _ = cache.lookup_or_compute(1, None, &messages);
        cache.invalidate();
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn blocks_system_prompt_yields_distinct_fingerprint() {
        let blocks_a = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "alpha".to_string(),
            cache_control: None,
        }]);
        let blocks_b = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "beta".to_string(),
            cache_control: None,
        }]);
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hi")];
        let _ = cache.lookup_or_compute(1, Some(&blocks_a), &messages);
        let _ = cache.lookup_or_compute(1, Some(&blocks_b), &messages);
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 2);
    }

    #[test]
    fn audit_ring_records_recent_pairs() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hi")];
        for rev in 1..=5 {
            let _ = cache.lookup_or_compute(rev, None, &messages);
        }
        let ring = cache.recent_audit();
        assert_eq!(ring.len(), 5);
        assert_eq!(ring.last().copied(), Some((5, ring.last().unwrap().1)));
    }

    #[test]
    fn audit_ring_bounded_by_capacity() {
        let mut cache = TokenEstimateCache::new();
        let messages = vec![user_text("hi")];
        for rev in 1..=(AUDIT_RING_CAPACITY + 10) as u64 {
            let _ = cache.lookup_or_compute(rev, None, &messages);
        }
        let ring = cache.recent_audit();
        assert_eq!(ring.len(), AUDIT_RING_CAPACITY);
        // newest entry should be the most recent revision we asked for
        assert_eq!(ring.last().unwrap().0, (AUDIT_RING_CAPACITY + 10) as u64);
    }
}

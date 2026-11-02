use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::tui::history::OutputProjection;

const DEFAULT_CAPACITY: usize = 256;
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_POLICY_VARIANTS: usize = 4;

#[derive(Debug)]
struct CacheEntry {
    rows: Arc<OutputProjection>,
    bytes: usize,
}

#[derive(Debug)]
struct OutputRowsCacheInner {
    capacity: usize,
    bytes: usize,
    by_key: HashMap<RowsKey, CacheEntry>,
    insertion_order: VecDeque<RowsKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RowsKey {
    content_hash: [u8; 32],
    width: u16,
    line_limit: usize,
    transcript: bool,
}

impl OutputRowsCacheInner {
    fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, DEFAULT_CAPACITY);
        Self {
            capacity,
            bytes: 0,
            by_key: HashMap::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
        }
    }

    fn container_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.by_key
                    .capacity()
                    .saturating_add(1)
                    .next_power_of_two()
                    .saturating_mul(std::mem::size_of::<(RowsKey, CacheEntry)>() + 1),
            )
            .saturating_add(16)
            .saturating_add(
                self.insertion_order
                    .capacity()
                    .saturating_mul(std::mem::size_of::<RowsKey>()),
            )
    }

    fn remove(&mut self, key: &RowsKey) {
        if let Some(entry) = self.by_key.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
        if let Some(index) = self
            .insertion_order
            .iter()
            .position(|candidate| candidate == key)
        {
            self.insertion_order.remove(index);
        }
    }

    fn get_or_compute_rows<F>(
        &mut self,
        content_hash: [u8; 32],
        width: u16,
        line_limit: usize,
        transcript: bool,
        compute: F,
    ) -> Arc<OutputProjection>
    where
        F: FnOnce() -> OutputProjection,
    {
        let key = RowsKey {
            content_hash,
            width,
            line_limit,
            transcript,
        };
        if let Some(entry) = self.by_key.get(&key) {
            let rows = Arc::clone(&entry.rows);
            if let Some(index) = self
                .insertion_order
                .iter()
                .position(|candidate| *candidate == key)
            {
                self.insertion_order.remove(index);
            }
            self.insertion_order.push_back(key);
            return rows;
        }
        let rows = Arc::new(compute());
        let bytes = rows
            .allocated_bytes()
            .saturating_add(2 * std::mem::size_of::<usize>());
        if bytes.saturating_add(self.container_bytes()) > MAX_CACHE_BYTES {
            return rows;
        }
        while self
            .by_key
            .keys()
            .filter(|candidate| candidate.content_hash == content_hash && candidate.width == width)
            .count()
            >= MAX_POLICY_VARIANTS
        {
            let oldest = self
                .insertion_order
                .iter()
                .find(|candidate| {
                    candidate.content_hash == content_hash && candidate.width == width
                })
                .copied();
            if let Some(oldest) = oldest {
                self.remove(&oldest);
            } else {
                break;
            }
        }
        while self.by_key.len() >= self.capacity
            || self
                .bytes
                .saturating_add(bytes)
                .saturating_add(self.container_bytes())
                > MAX_CACHE_BYTES
        {
            let Some(oldest) = self.insertion_order.front().copied() else {
                break;
            };
            self.remove(&oldest);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.by_key.insert(
            key,
            CacheEntry {
                rows: Arc::clone(&rows),
                bytes,
            },
        );
        self.insertion_order.push_back(key);
        rows
    }
}

thread_local! {
    static GLOBAL_CACHE: RefCell<OutputRowsCacheInner> =
        RefCell::new(OutputRowsCacheInner::new());
}

#[cfg(test)]
pub fn reset_for_tests() {
    GLOBAL_CACHE.with(|c| *c.borrow_mut() = OutputRowsCacheInner::new());
}

#[cfg(test)]
pub fn get_or_compute_rows<F>(output: &str, width: u16, compute: F) -> Arc<OutputProjection>
where
    F: FnOnce() -> OutputProjection,
{
    get_or_compute_rows_with_hash(hash_output(output), width, 20, false, compute)
}

pub fn get_or_compute_rows_with_hash<F>(
    content_hash: [u8; 32],
    width: u16,
    line_limit: usize,
    transcript: bool,
    compute: F,
) -> Arc<OutputProjection>
where
    F: FnOnce() -> OutputProjection,
{
    GLOBAL_CACHE.with(|c| {
        c.borrow_mut()
            .get_or_compute_rows(content_hash, width, line_limit, transcript, compute)
    })
}

pub fn hash_output(output: &str) -> [u8; 32] {
    Sha256::digest(output.as_bytes()).into()
}

pub fn hash_str(s: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash ^= s.len() as u64;
    hash.wrapping_mul(FNV_PRIME)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::history::OutputRow;

    fn row(text: &str) -> OutputRow {
        OutputRow {
            text: text.to_string(),
            intact: false,
            styled: None,
        }
    }

    #[test]
    fn cache_hit_returns_cached_rows() {
        reset_for_tests();

        let calls = std::cell::Cell::new(0u32);
        let compute = || {
            calls.set(calls.get() + 1);
            OutputProjection {
                rows: vec![row("hello"), row("world")],
                ..Default::default()
            }
        };

        let a = get_or_compute_rows("payload", 80, compute);
        let b = get_or_compute_rows("payload", 80, || {
            calls.set(calls.get() + 1);
            OutputProjection {
                rows: vec![row("hello"), row("world")],
                ..Default::default()
            }
        });
        assert_eq!(calls.get(), 1, "second call should hit the cache");
        assert_eq!(a, b);
    }

    #[test]
    fn different_width_invalidates_rows() {
        reset_for_tests();

        let calls = std::cell::Cell::new(0u32);
        let make = || {
            calls.set(calls.get() + 1);
            OutputProjection {
                rows: vec![row("hello")],
                ..Default::default()
            }
        };

        let _ = get_or_compute_rows("payload", 80, make);
        let _ = get_or_compute_rows("payload", 120, make);
        assert_eq!(calls.get(), 2, "different width must miss the cache");
    }

    #[test]
    fn different_output_invalidates_rows() {
        reset_for_tests();

        let calls = std::cell::Cell::new(0u32);
        let make = || {
            calls.set(calls.get() + 1);
            OutputProjection {
                rows: vec![row("x")],
                ..Default::default()
            }
        };

        let _ = get_or_compute_rows("payload-a", 80, make);
        let _ = get_or_compute_rows("payload-b", 80, make);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn projections_cached_per_policy_and_line_limit() {
        reset_for_tests();
        let content_hash = hash_output("payload");
        let mut calls = 0;
        let first = get_or_compute_rows_with_hash(content_hash, 80, 2, false, || {
            calls += 1;
            OutputProjection {
                rows: vec![row("a"), row("e")],
                ..Default::default()
            }
        });
        let repeated = get_or_compute_rows_with_hash(content_hash, 80, 2, false, || {
            calls += 1;
            OutputProjection::default()
        });
        assert_eq!(calls, 1);
        assert!(Arc::ptr_eq(&first, &repeated));
        let _ = get_or_compute_rows_with_hash(content_hash, 80, 3, false, || {
            calls += 1;
            OutputProjection::default()
        });
        let _ = get_or_compute_rows_with_hash(content_hash, 80, 3, true, || {
            calls += 1;
            OutputProjection::default()
        });
        assert_eq!(calls, 3);
    }

    #[test]
    fn capacity_evicts_oldest() {
        // Build a private cache so we can size it tightly.
        let mut cache = OutputRowsCacheInner::with_capacity(2);

        let _ = cache.get_or_compute_rows([1; 32], 80, 20, false, || OutputProjection {
            rows: vec![row("a")],
            ..Default::default()
        });
        let _ = cache.get_or_compute_rows([2; 32], 80, 20, false, || OutputProjection {
            rows: vec![row("b")],
            ..Default::default()
        });
        let _ = cache.get_or_compute_rows([3; 32], 80, 20, false, || OutputProjection {
            rows: vec![row("c")],
            ..Default::default()
        });
        // The first entry (hash 1) should have been evicted.
        let mut compute_calls = 0;
        let _ = cache.get_or_compute_rows([1; 32], 80, 20, false, || {
            compute_calls += 1;
            OutputProjection {
                rows: vec![row("a")],
                ..Default::default()
            }
        });
        assert_eq!(compute_calls, 1, "evicted entry must miss");
    }

    #[test]
    fn hash_str_stable_for_identical_input() {
        assert_eq!(hash_str("hello"), hash_str("hello"));
        assert_ne!(hash_str("hello"), hash_str("world"));
    }

    #[test]
    fn hash_str_differs_on_length_suffix() {
        // A trailing newline is a different content; the hash must differ.
        assert_ne!(hash_str("hello"), hash_str("hello\n"));
    }

    #[test]
    fn hash_str_handles_empty() {
        // Empty string hashes to the FNV offset basis; the result just
        // needs to be stable.
        assert_eq!(hash_str(""), hash_str(""));
    }
}

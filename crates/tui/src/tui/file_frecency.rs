//! @-mention frecency tracking (#441).
//!
//! Records every file the user @-mentions with a timestamp and click count,
//! decays the score over time so a file that was hot last week ranks below
//! one mentioned 5 minutes ago, and re-orders mention-popup completions by
//! the resulting score. Persisted as a single JSONL file at
//! `$CODEWHALE_HOME/file-frecency.jsonl` (normally
//! `~/.codewhale/file-frecency.jsonl`) so frecency survives restarts.
//!
//! Append-only on the wire, rewritten when it grows: a background thread
//! replays each line into a `HashMap` keyed by workspace-scoped path, so
//! filesystem work stays off the UI thread. Unscoped older records remain
//! readable. We cap the in-memory map at 1000 entries and evict the
//! lowest-scored on overflow — same heuristic the OPENCODE source uses.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Hard cap on the number of paths we track (the acceptance criterion for
/// #441). Older / lower-scored entries are evicted when the map exceeds
/// this.
const FRECENCY_CAP: usize = 1000;

/// Half-life of a frecency score, in seconds. After this many seconds the
/// score has decayed to ½ of its peak. 7 days is OPENCODE's default — long
/// enough that a commonly-edited file stays sticky across a workweek but
/// short enough that yesterday's deep-dive doesn't haunt you forever.
const HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 60.0 * 60.0;

const MAX_FRECENCY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const FRECENCY_COMPACT_BYTES: u64 = 512 * 1024;
const FRECENCY_QUEUE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrecencyRecord {
    #[serde(default)]
    workspace: String,
    /// Workspace-relative path string.
    path: String,
    /// Total mentions over the lifetime of the entry.
    count: u32,
    /// Unix timestamp (seconds) of the last mention.
    last_used: u64,
}

#[derive(Debug, Default)]
struct Store {
    by_path: HashMap<String, FrecencyRecord>,
}

struct FrecencyWorker {
    sender: SyncSender<FrecencyJob>,
}

struct FrecencyJob {
    workspace: String,
    path: String,
}

static LOADED_SIGNAL: AtomicBool = AtomicBool::new(false);

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn worker() -> Option<&'static FrecencyWorker> {
    static WORKER: OnceLock<Option<FrecencyWorker>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let path = default_path()?;
            let (sender, receiver) = sync_channel(FRECENCY_QUEUE_CAPACITY);
            let thread_store = store();
            std::thread::Builder::new()
                .name("codewhale-frecency".to_string())
                .spawn(move || {
                    fn worker_main(store: &'static Mutex<Store>, path: PathBuf, jobs: Receiver<FrecencyJob>) {
                        merge_loaded_records(store, load_records(&path));
                        LOADED_SIGNAL.store(true, Ordering::Release);
                        for job in jobs {
                            persist_record(store, &path, &job.workspace, &job.path);
                        }
                    }

                    fn load_records(path: &Path) -> Vec<FrecencyRecord> {
                        if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_FRECENCY_FILE_BYTES) {
                            tracing::debug!(target: "frecency", "ignoring oversized frecency store");
                            return Vec::new();
                        }
                        let Ok(file) = std::fs::File::open(path) else {
                            return Vec::new();
                        };
                        let mut bytes = Vec::new();
                        if file
                            .take(MAX_FRECENCY_FILE_BYTES + 1)
                            .read_to_end(&mut bytes)
                            .is_err()
                        {
                            return Vec::new();
                        }
                        if bytes.len() as u64 > MAX_FRECENCY_FILE_BYTES {
                            tracing::debug!(target: "frecency", "ignoring oversized frecency store");
                            return Vec::new();
                        }
                        let Ok(text) = String::from_utf8(bytes) else {
                            return Vec::new();
                        };
                        text.lines()
                            .filter_map(|line| serde_json::from_str(line).ok())
                            .collect()
                    }

                    fn merge_loaded_records(store: &'static Mutex<Store>, records: Vec<FrecencyRecord>) {
                        let mut loaded = HashMap::new();
                        for record in records {
                            loaded.insert(scoped_key(&record.workspace, &record.path), record);
                        }
                        let Ok(mut store) = store.lock() else {
                            return;
                        };
                        for (key, record) in loaded {
                            match store.by_path.entry(key) {
                                Entry::Occupied(mut entry) => {
                                    let existing = entry.get_mut();
                                    existing.count = existing.count.saturating_add(record.count);
                                    existing.last_used = existing.last_used.max(record.last_used);
                                }
                                Entry::Vacant(entry) => {
                                    entry.insert(record);
                                }
                            }
                        }
                    }

                    fn persist_record(store: &'static Mutex<Store>, store_path: &Path, workspace: &str, path: &str) {
                        let record = {
                            let Ok(store) = store.lock() else {
                                return;
                            };
                            let Some(record) = store.by_path.get(&scoped_key(workspace, path)) else {
                                return;
                            };
                            record.clone()
                        };
                        if let Err(err) = append_record_line(store_path, &record) {
                            tracing::debug!(target: "frecency", "persist failed: {err}");
                            return;
                        }
                        if std::fs::metadata(store_path).is_ok_and(|metadata| metadata.len() > FRECENCY_COMPACT_BYTES) {
                            compact_store(store, store_path);
                        }
                    }

                    fn compact_store(store: &'static Mutex<Store>, store_path: &Path) {
                        let records = {
                            let Ok(store) = store.lock() else {
                                return;
                            };
                            store.by_path.values().cloned().collect::<Vec<_>>()
                        };
                        let mut body = String::new();
                        for record in records {
                            let Ok(line) = serde_json::to_string(&record) else {
                                continue;
                            };
                            if body.len().saturating_add(line.len() + 1) > FRECENCY_COMPACT_BYTES as usize {
                                continue;
                            }
                            body.push_str(&line);
                            body.push('\n');
                        }
                        if let Err(err) = codewhale_util::utils::write_atomic(store_path, body.as_bytes()) {
                            tracing::debug!(target: "frecency", "compact failed: {err}");
                        }
                    }

                    worker_main(thread_store, path, receiver)
                })
                .map_err(|err| {
                    tracing::debug!(target: "frecency", "background store unavailable: {err}");
                })
                .ok()?;
            Some(FrecencyWorker { sender })
        })
        .as_ref()
}

fn default_path() -> Option<PathBuf> {
    // Unit tests exercise mention selection heavily and must never read from or
    // append to a developer's real Codewhale home. Integration and release QA
    // processes use an explicit isolated CODEWHALE_HOME instead.
    #[cfg(test)]
    {
        None
    }

    #[cfg(not(test))]
    {
        codewhale_config::codewhale_home()
            .ok()
            .map(|home| home.join("file-frecency.jsonl"))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Time-decayed frecency score for a record, in arbitrary units. Mentions
/// count linearly; the whole sum is multiplied by an exponential decay
/// factor based on time since `last_used`. Records older than ~5 half-lives
/// score effectively zero.
fn decayed_score(record: &FrecencyRecord, now: u64) -> f64 {
    let age_secs = now.saturating_sub(record.last_used) as f64;
    let lambda = std::f64::consts::LN_2 / HALF_LIFE_SECS;
    (record.count as f64) * (-lambda * age_secs).exp()
}

fn workspace_scope(workspace: &Path) -> String {
    workspace.to_string_lossy().into_owned()
}

fn scoped_key(workspace: &str, path: &str) -> String {
    if workspace.is_empty() {
        path.to_string()
    } else {
        format!("{workspace}\u{0}{path}")
    }
}

pub(crate) fn poll_loaded() -> bool {
    LOADED_SIGNAL.swap(false, Ordering::AcqRel)
}

fn evict_to_cap(store: &mut Store, now: u64) {
    if store.by_path.len() <= FRECENCY_CAP {
        return;
    }
    let target = FRECENCY_CAP;
    let mut scored: Vec<(String, f64)> = store
        .by_path
        .iter()
        .map(|(k, v)| (k.clone(), decayed_score(v, now)))
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let drop_count = store.by_path.len().saturating_sub(target);
    for (key, _) in scored.iter().take(drop_count) {
        store.by_path.remove(key);
    }
}

fn append_record_line(path: &Path, record: &FrecencyRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Record one mention of `path` (a workspace-relative path string). Updates
/// the in-memory store, queues a single JSONL line, and evicts the lowest-
/// scored entry if we just exceeded the cap. Best-effort: I/O failures are
/// logged and swallowed — losing a frecency datapoint is never worth
/// failing the user's `@` autocomplete.
pub fn record_mention(workspace: &Path, path: &str) {
    if path.is_empty() {
        return;
    }
    let worker = worker();
    let store = store();
    let Ok(mut store) = store.lock() else {
        return;
    };
    let now = now_secs();
    let scope = workspace_scope(workspace);
    let key = scoped_key(&scope, path);
    let legacy_count = if scope.is_empty() || store.by_path.contains_key(&key) {
        0
    } else {
        store.by_path.get(path).map_or(0, |record| record.count)
    };
    let entry = store.by_path.entry(key).or_insert_with(|| FrecencyRecord {
        workspace: scope.clone(),
        path: path.to_string(),
        count: legacy_count,
        last_used: now,
    });
    entry.count = entry.count.saturating_add(1);
    entry.last_used = now;
    evict_to_cap(&mut store, now);
    drop(store);
    if let Some(worker) = worker {
        let job = FrecencyJob {
            workspace: scope,
            path: path.to_string(),
        };
        if let Err(err) = worker.sender.try_send(job) {
            tracing::debug!(target: "frecency", "persist queue: {err}");
        }
    }
}

/// Re-sort a candidate list by frecency score (highest first), preserving
/// the original order for ties so the underlying ranker's choices aren't
/// upended. Candidates the store has never seen score zero — they end up
/// at the bottom of the sort, which means a one-time mention will start
/// floating to the top after first use.
#[must_use]
pub fn rerank_by_frecency(workspace: &Path, candidates: Vec<String>) -> Vec<String> {
    if candidates.len() <= 1 {
        return candidates;
    }
    let _worker = worker();
    let store = store();
    let Ok(store) = store.lock() else {
        return candidates;
    };
    let scope = workspace_scope(workspace);
    let now = now_secs();
    let mut scored: Vec<(usize, String, f64)> = candidates
        .into_iter()
        .enumerate()
        .map(|(idx, path)| {
            let score = store
                .by_path
                .get(&scoped_key(&scope, &path))
                .or_else(|| store.by_path.get(&path))
                .map(|record| decayed_score(record, now))
                .unwrap_or(0.0);
            (idx, path, score)
        })
        .collect();
    // Stable sort on (-score, original-index): ties keep the underlying
    // ranker's order.
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored.into_iter().map(|(_, path, _)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recently mentioned paths win against never-mentioned ones; never-mentioned
    /// preserve their original ranker order.
    #[test]
    fn rerank_floats_recent_paths_to_the_top() {
        // Use the global store; reset its state so we don't leak across tests.
        let store = super::store();
        let mut s = store.lock().unwrap();
        s.by_path.clear();
        let now = super::now_secs();
        s.by_path.insert(
            "src/popular.rs".into(),
            FrecencyRecord {
                workspace: String::new(),
                path: "src/popular.rs".into(),
                count: 8,
                last_used: now,
            },
        );
        drop(s);

        let order = super::rerank_by_frecency(
            std::path::Path::new("/workspace"),
            vec![
                "README.md".to_string(),
                "src/popular.rs".to_string(),
                "Cargo.toml".to_string(),
            ],
        );
        assert_eq!(order[0], "src/popular.rs");
        // README.md was first in original order; Cargo.toml second. Both score 0
        // so the original relative order survives.
        assert_eq!(order[1], "README.md");
        assert_eq!(order[2], "Cargo.toml");
    }

    /// Decayed score drops below a freshly-used entry after enough half-lives
    /// that count alone can't carry the older one. With a 7-day half-life,
    /// 8 weeks gives 8 half-lives → ~256× decay; an entry mentioned twice
    /// today comfortably beats one mentioned 50× two months ago.
    #[test]
    fn old_entries_decay_below_recent_ones() {
        let now: u64 = 7 * 24 * 60 * 60 * 8; // 8 weeks (8 half-lives)
        let stale = FrecencyRecord {
            workspace: String::new(),
            path: "x".into(),
            count: 50,
            last_used: 0,
        };
        let fresh = FrecencyRecord {
            workspace: String::new(),
            path: "y".into(),
            count: 2,
            last_used: now,
        };
        assert!(
            super::decayed_score(&fresh, now) > super::decayed_score(&stale, now),
            "fresh={}, stale={}",
            super::decayed_score(&fresh, now),
            super::decayed_score(&stale, now)
        );
    }
}

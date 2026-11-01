//! Background discovery for composer `@`-mention completions.
//!
//! Filesystem traversal never belongs on the TUI thread. This module owns one
//! serialized worker per composer, a single coalescing request slot, and a
//! generation token that prevents a superseded scan from publishing results.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use codewhale_runtime::working_set::Workspace;

/// Keep discovery memory and background work bounded even when the workspace
/// is an unignored drive root. The popup itself renders far fewer rows, but a
/// larger cached pool preserves useful fuzzy matching across keystrokes.
pub(crate) const MAX_MENTION_DISCOVERY_CANDIDATES: usize = 20_000;

const MENTION_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionDiscoveryBehavior {
    Fuzzy,
    Browser { partial: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MentionDiscoveryKey {
    pub workspace: PathBuf,
    pub cwd: Option<PathBuf>,
    pub walk_depth: usize,
    pub follow_links: bool,
    pub behavior: MentionDiscoveryBehavior,
}

impl MentionDiscoveryKey {
    pub(crate) fn fuzzy(
        workspace: PathBuf,
        cwd: Option<PathBuf>,
        walk_depth: usize,
        follow_links: bool,
    ) -> Self {
        Self {
            workspace,
            cwd,
            walk_depth,
            follow_links,
            behavior: MentionDiscoveryBehavior::Fuzzy,
        }
    }

    pub(crate) fn browser(
        workspace: PathBuf,
        cwd: Option<PathBuf>,
        walk_depth: usize,
        follow_links: bool,
        partial: String,
    ) -> Self {
        Self {
            workspace,
            cwd,
            walk_depth,
            follow_links,
            behavior: MentionDiscoveryBehavior::Browser { partial },
        }
    }
}

#[derive(Debug)]
struct MentionDiscoveryRequest {
    generation: u64,
    key: MentionDiscoveryKey,
}

#[derive(Debug)]
struct MentionDiscoveryResult {
    generation: u64,
    key: MentionDiscoveryKey,
    entries: Vec<String>,
    collected_at: Instant,
}

struct WorkerShared {
    pending: Mutex<Option<MentionDiscoveryRequest>>,
    wake: Condvar,
    result: Mutex<Option<MentionDiscoveryResult>>,
    latest_generation: AtomicU64,
    closed: AtomicBool,
}

type MentionScanner = dyn Fn(&MentionDiscoveryKey, &dyn Fn() -> bool) -> Vec<String> + Send + Sync;

struct MentionDiscoveryWorker {
    shared: Arc<WorkerShared>,
}

impl MentionDiscoveryWorker {
    fn spawn(scanner: Arc<MentionScanner>) -> std::io::Result<Self> {
        let shared = Arc::new(WorkerShared {
            pending: Mutex::new(None),
            wake: Condvar::new(),
            result: Mutex::new(None),
            latest_generation: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("codewhale-mention-discovery".to_string())
            .spawn(move || worker_loop(&thread_shared, &scanner))?;
        Ok(Self { shared })
    }

    /// Replace the pending slot without waiting on filesystem work. The worker
    /// only holds this mutex long enough to take one request; scanning happens
    /// after it is released, so brief lock contention must not drop the request.
    fn submit(&self, request: MentionDiscoveryRequest) -> bool {
        self.shared
            .latest_generation
            .store(request.generation, Ordering::Release);
        let mut pending = self.shared.pending.lock();
        *pending = Some(request);
        drop(pending);
        self.shared.wake.notify_one();
        true
    }

    fn take_result(&self) -> Option<MentionDiscoveryResult> {
        self.shared
            .result
            .try_lock()
            .and_then(|mut result| result.take())
    }

    fn cancel(&self, generation: u64) {
        self.shared
            .latest_generation
            .store(generation, Ordering::Release);
        if let Some(mut pending) = self.shared.pending.try_lock() {
            *pending = None;
        }
    }
}

impl Drop for MentionDiscoveryWorker {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.latest_generation.fetch_add(1, Ordering::AcqRel);
        self.shared.wake.notify_all();
        // Never join here: a filesystem call already in progress may be slow.
        // Dropping the JoinHandle detached it at spawn time, and the worker
        // exits as soon as that call returns and observes `closed`.
    }
}

fn worker_loop(shared: &WorkerShared, scanner: &Arc<MentionScanner>) {
    loop {
        let request = {
            let mut pending = shared.pending.lock();
            while pending.is_none() && !shared.closed.load(Ordering::Acquire) {
                shared.wake.wait(&mut pending);
            }
            if shared.closed.load(Ordering::Acquire) {
                return;
            }
            pending.take()
        };
        let Some(request) = request else {
            continue;
        };
        let generation = request.generation;
        let cancelled = || {
            shared.closed.load(Ordering::Acquire)
                || shared.latest_generation.load(Ordering::Acquire) != generation
        };
        if cancelled() {
            continue;
        }
        let entries = scanner(&request.key, &cancelled);
        if cancelled() {
            continue;
        }
        *shared.result.lock() = Some(MentionDiscoveryResult {
            generation,
            key: request.key,
            entries,
            collected_at: Instant::now(),
        });
    }
}

fn filesystem_scanner(key: &MentionDiscoveryKey, cancelled: &dyn Fn() -> bool) -> Vec<String> {
    let workspace = Workspace::with_cwd_depth_and_follow_links(
        key.workspace.clone(),
        key.cwd.clone(),
        key.walk_depth,
        key.follow_links,
    );
    match &key.behavior {
        MentionDiscoveryBehavior::Fuzzy => {
            workspace.completion_discovery_candidates(MAX_MENTION_DISCOVERY_CANDIDATES, cancelled)
        }
        MentionDiscoveryBehavior::Browser { partial } => workspace
            .browser_completion_discovery_candidates(
                partial,
                MAX_MENTION_DISCOVERY_CANDIDATES,
                cancelled,
            ),
    }
}

#[derive(Debug)]
struct CachedMentionDiscovery {
    key: MentionDiscoveryKey,
    entries: Vec<String>,
    collected_at: Instant,
}

/// UI-owned handle for the serialized mention-discovery worker.
///
/// `ensure_requested`, `poll`, and `cached_entries` never wait on filesystem
/// discovery. The only synchronous work on the UI thread is key comparison,
/// cloning an already-bounded in-memory result, and briefly locking a request
/// slot that the worker never holds while scanning.
pub(crate) struct MentionDiscovery {
    scanner: Arc<MentionScanner>,
    worker: Option<MentionDiscoveryWorker>,
    generation: u64,
    in_flight: Option<(u64, MentionDiscoveryKey)>,
    cached: Option<CachedMentionDiscovery>,
}

impl Default for MentionDiscovery {
    fn default() -> Self {
        Self::new(Arc::new(filesystem_scanner))
    }
}

impl MentionDiscovery {
    fn new(scanner: Arc<MentionScanner>) -> Self {
        Self {
            scanner,
            worker: None,
            generation: 0,
            in_flight: None,
            cached: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_scanner<F>(scanner: F) -> Self
    where
        F: Fn(&MentionDiscoveryKey, &dyn Fn() -> bool) -> Vec<String> + Send + Sync + 'static,
    {
        Self::new(Arc::new(scanner))
    }

    /// Start or refresh discovery for `key`. Returns immediately even if a
    /// previous generation is stalled inside one filesystem read.
    pub(crate) fn ensure_requested(&mut self, key: MentionDiscoveryKey) {
        let cache_is_fresh = self.cached.as_ref().is_some_and(|cached| {
            cached.key == key && cached.collected_at.elapsed() < MENTION_DISCOVERY_CACHE_TTL
        });
        if cache_is_fresh {
            if self
                .in_flight
                .as_ref()
                .is_some_and(|(_, pending_key)| *pending_key != key)
            {
                self.cancel();
            }
            return;
        }
        if self
            .in_flight
            .as_ref()
            .is_some_and(|(_, pending_key)| *pending_key == key)
        {
            return;
        }

        if self.worker.is_none() {
            match MentionDiscoveryWorker::spawn(Arc::clone(&self.scanner)) {
                Ok(worker) => self.worker = Some(worker),
                Err(err) => {
                    tracing::warn!(error = %err, "failed to start @-mention discovery worker");
                    return;
                }
            }
        }

        let generation = self.next_generation();
        let request = MentionDiscoveryRequest {
            generation,
            key: key.clone(),
        };
        let submitted = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.submit(request));
        if submitted {
            self.in_flight = Some((generation, key));
        }
    }

    /// Apply one completed result if it still belongs to the current
    /// generation. Returns `true` when the visible cache changed.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(result) = self
            .worker
            .as_ref()
            .and_then(MentionDiscoveryWorker::take_result)
        else {
            return false;
        };
        let is_current = result.generation == self.generation
            && self.in_flight.as_ref().is_some_and(|(generation, key)| {
                *generation == result.generation && *key == result.key
            });
        if !is_current {
            return false;
        }
        self.in_flight = None;
        self.cached = Some(CachedMentionDiscovery {
            key: result.key,
            entries: result.entries,
            collected_at: result.collected_at,
        });
        true
    }

    pub(crate) fn cached_entries(&self, key: &MentionDiscoveryKey) -> Option<&[String]> {
        self.cached
            .as_ref()
            .filter(|cached| &cached.key == key)
            .map(|cached| cached.entries.as_slice())
    }

    /// Cached fuzzy-scan candidates for send-time `@`-mention fallback
    /// resolution. Returns `Some` only when the cache holds a completed fuzzy
    /// scan for exactly this workspace/cwd/depth/symlink configuration —
    /// browser-mode caches cover a single directory and are not a usable
    /// resolution index.
    pub(crate) fn fuzzy_candidates(
        &self,
        workspace: &std::path::Path,
        cwd: &Option<PathBuf>,
        walk_depth: usize,
        follow_links: bool,
    ) -> Option<&[String]> {
        let key = MentionDiscoveryKey::fuzzy(
            workspace.to_path_buf(),
            cwd.clone(),
            walk_depth,
            follow_links,
        );
        self.cached_entries(&key)
    }

    /// Cancel the active generation while keeping a same-key cache available
    /// for the next mention.
    pub(crate) fn cancel(&mut self) {
        if self.in_flight.is_none() {
            return;
        }
        let generation = self.next_generation();
        if let Some(worker) = &self.worker {
            worker.cancel(generation);
        }
        self.in_flight = None;
    }

    /// Drop cached and in-flight state after workspace/completion settings
    /// change. Any late worker result is rejected by the new generation.
    pub(crate) fn invalidate(&mut self) {
        let generation = self.next_generation();
        if let Some(worker) = &self.worker {
            worker.cancel(generation);
        }
        self.in_flight = None;
        self.cached = None;
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const TEST_WORKER_TIMEOUT: Duration = Duration::from_secs(10);

    fn key(name: &str) -> MentionDiscoveryKey {
        MentionDiscoveryKey::fuzzy(PathBuf::from(name), None, 10, false)
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
        let started = Instant::now();
        while !predicate() {
            assert!(started.elapsed() < timeout, "timed out waiting for worker");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn request_path_stays_immediate_while_scanner_is_blocked() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let mut discovery = MentionDiscovery::with_scanner(move |_, _| {
            let _ = started_tx.send(());
            let _ = release_rx.lock().recv();
            vec!["ready.rs".to_string()]
        });

        let started = Instant::now();
        discovery.ensure_requested(key("slow"));
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "request submission waited on the blocked scanner"
        );
        started_rx
            .recv_timeout(TEST_WORKER_TIMEOUT)
            .expect("scanner should start in the background");

        let second_started = Instant::now();
        discovery.ensure_requested(key("newer"));
        assert!(
            second_started.elapsed() < Duration::from_millis(50),
            "superseding request waited on the blocked scanner"
        );
        release_tx.send(()).unwrap();
    }

    #[test]
    fn contended_pending_slot_does_not_drop_request() {
        let (scan_started_tx, scan_started_rx) = mpsc::channel();
        let worker = MentionDiscoveryWorker::spawn(Arc::new(move |_, _| {
            let _ = scan_started_tx.send(());
            Vec::new()
        }))
        .expect("worker should start");
        let pending_guard = worker.shared.pending.lock();
        let (attempting_tx, attempting_rx) = mpsc::channel();
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let request = MentionDiscoveryRequest {
            generation: 1,
            key: key("contended"),
        };

        thread::scope(|scope| {
            let worker = &worker;
            scope.spawn(move || {
                attempting_tx.send(()).unwrap();
                submitted_tx.send(worker.submit(request)).unwrap();
            });
            attempting_rx
                .recv_timeout(TEST_WORKER_TIMEOUT)
                .expect("submission should be attempted");
            let submitted_while_contended = submitted_rx.recv_timeout(Duration::from_millis(50));
            drop(pending_guard);
            let submitted = match submitted_while_contended {
                Ok(submitted) => submitted,
                Err(mpsc::RecvTimeoutError::Timeout) => submitted_rx
                    .recv_timeout(TEST_WORKER_TIMEOUT)
                    .expect("submission should finish after contention clears"),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("submission thread disconnected")
                }
            };
            assert!(submitted, "a contended request must not be dropped");
        });

        scan_started_rx
            .recv_timeout(TEST_WORKER_TIMEOUT)
            .expect("the contended request should reach the scanner");
    }

    #[test]
    fn late_result_cannot_replace_new_generation() {
        let (first_started_tx, first_started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let release_first_rx = Mutex::new(release_first_rx);
        let mut discovery = MentionDiscovery::with_scanner(move |key, cancelled| {
            if key.workspace == std::path::Path::new("old") {
                let _ = first_started_tx.send(());
                let _ = release_first_rx.lock().recv();
                // Simulate an uninterruptible filesystem call returning late.
                assert!(cancelled());
                vec!["stale.rs".to_string()]
            } else {
                vec!["current.rs".to_string()]
            }
        });

        let old_key = key("old");
        let new_key = key("new");
        discovery.ensure_requested(old_key.clone());
        first_started_rx
            .recv_timeout(TEST_WORKER_TIMEOUT)
            .expect("old scan should start");
        discovery.ensure_requested(new_key.clone());
        release_first_tx.send(()).unwrap();

        wait_until(TEST_WORKER_TIMEOUT, || discovery.poll());
        assert!(discovery.cached_entries(&old_key).is_none());
        assert_eq!(
            discovery.cached_entries(&new_key),
            Some(["current.rs".to_string()].as_slice())
        );
    }
}

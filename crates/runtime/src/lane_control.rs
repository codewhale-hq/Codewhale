//! Off-event-loop Lane control submission (#1888, #4022).
//!
//! `/lane interrupt` performs Runtime teardown — `tmux kill-session`, worktree
//! TTL cleanup, an advisory lock — none of which may run on the TUI composer
//! thread. Making the verb CLI-only would have been a capability regression, so
//! the slash surface *submits* instead: validation and availability are decided
//! synchronously (so a bad id is still refused immediately), the work is handed
//! to a dedicated worker thread, and the caller gets a typed `queued` receipt
//! carrying a ticket. The terminal receipt — `transitioned`, `no_change`,
//! `conflict`, or `failed` — arrives under that same ticket and is drained by
//! the UI on its next tick.
//!
//! A `queued` receipt is never reported as success. It says exactly what has
//! happened: nothing yet.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use codewhale_lane::control::{
    ControlContext, ControlFailure, ControlFailureKind, ControlOperation, ControlReceipt,
    ControlSurface, execute_lane_control_in, parse_target,
};

/// Maximum submissions that may be in flight before the queue refuses.
///
/// Interrupts are operator-initiated keystrokes, not a stream: a handful of
/// pending teardowns is already pathological, and an unbounded queue would let
/// a stuck `tmux` call accumulate work the operator can neither see nor cancel.
pub const MAX_PENDING: usize = 16;

#[derive(Debug)]
struct Submission {
    ticket: String,
    operation: ControlOperation,
    raw_target: Option<String>,
    registry_root: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct Shared {
    pending: VecDeque<Submission>,
    completed: Vec<ControlReceipt>,
    /// Tickets accepted but not yet completed. Bounds the queue and lets a
    /// duplicate interrupt of the same Lane be answered without re-queuing.
    in_flight: Vec<(String, String)>,
    shutdown: bool,
    /// Whether the draining thread exists yet. Spawned on first submission so
    /// that constructing an `App` (which tests do many times) does not create a
    /// thread that will never be used.
    worker_started: bool,
}

/// A bounded, off-loop worker for durable Lane control verbs.
#[derive(Debug, Clone)]
pub struct LaneControlQueue {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    tickets: Arc<AtomicU64>,
}

impl Default for LaneControlQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneControlQueue {
    /// Create a queue with no worker attached. Submissions accumulate until a
    /// worker drains them, which is what the tests use to observe the queue
    /// deterministically.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(Shared::default()), Condvar::new())),
            tickets: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Start the draining thread if it is not already running.
    ///
    /// A plain OS thread, not a tokio task: the work is blocking by nature
    /// (subprocess + file lock) and must not occupy an async executor slot.
    /// Called on first submission so an idle `App` costs nothing.
    fn ensure_worker(&self, shared: &mut Shared) {
        if shared.worker_started {
            return;
        }
        shared.worker_started = true;
        let worker = self.clone();
        if std::thread::Builder::new()
            .name("lane-control".to_string())
            .spawn(move || worker.run())
            .is_err()
        {
            // Could not spawn: leave the flag clear so a later submission can
            // retry rather than queueing into a queue nothing will drain.
            shared.worker_started = false;
        }
    }

    /// Submit a Lane control verb for off-loop execution.
    ///
    /// Everything that can be decided without blocking is decided here and
    /// returned synchronously: unknown verb, malformed target, unavailable
    /// backend, saturated queue. Only the blocking teardown is deferred.
    #[must_use]
    pub fn submit(
        &self,
        operation: ControlOperation,
        raw_target: Option<&str>,
        registry_root: Option<PathBuf>,
    ) -> ControlReceipt {
        let descriptor = operation.descriptor();
        let surface = ControlSurface::Slash;

        // #1888: a surface must never advertise a backend that does not exist.
        // A verb that is declared but unbuilt (`restart`, `resume`) or not
        // offered on this surface can never succeed no matter what the durable
        // store looks like, so it is refused here with its typed reason rather
        // than answered `queued` for work that will never run. The stores are
        // deliberately assumed present for this check: whether the registry
        // exists is a *runtime* fact the executor probes on the worker thread,
        // and probing it here would put a filesystem answer on the composer
        // thread and make an empty workspace look like an unbuilt feature.
        let availability = descriptor.availability(surface, ControlContext::new(true, true));
        if !availability.is_available() {
            return ControlReceipt::unavailable(descriptor, surface, availability);
        }

        // Validate the target on the calling thread: a typo must be refused
        // now, not silently queued and refused a tick later.
        let target = match parse_target(descriptor, raw_target) {
            Ok(target) => target,
            Err(failure) => {
                return ControlReceipt::rejected(descriptor, surface, None, failure);
            }
        };

        let (lock, condvar) = &*self.shared;
        let mut shared = match lock.lock() {
            Ok(shared) => shared,
            // A poisoned queue means the worker panicked mid-teardown. Refuse
            // rather than submitting into a queue nothing will drain.
            Err(_) => {
                return ControlReceipt::failed(
                    descriptor,
                    surface,
                    target,
                    ControlFailure::backend("Lane control worker is not running"),
                );
            }
        };

        if shared.pending.len() >= MAX_PENDING {
            return ControlReceipt::rejected(
                descriptor,
                surface,
                target,
                ControlFailure::new(
                    ControlFailureKind::Saturated,
                    format!(
                        "Lane control queue is full ({MAX_PENDING} pending); \
                         nothing was submitted"
                    ),
                ),
            );
        }

        // Re-submitting the same Lane while a teardown is in flight is a
        // conflict, not a second teardown.
        if let Some(target) = target.as_ref()
            && let Some((ticket, _)) = shared
                .in_flight
                .iter()
                .find(|(_, id)| *id == target.id)
                .cloned()
        {
            return ControlReceipt::rejected(
                descriptor,
                surface,
                Some(target.clone()),
                ControlFailure::conflict(format!(
                    "{} is already in flight under ticket {ticket}",
                    target.id
                )),
            );
        }

        let ticket = format!("lane-ctl-{}", self.tickets.fetch_add(1, Ordering::Relaxed));
        if let Some(target) = target.as_ref() {
            shared.in_flight.push((ticket.clone(), target.id.clone()));
        }
        shared.pending.push_back(Submission {
            ticket: ticket.clone(),
            operation,
            raw_target: raw_target.map(str::to_string),
            registry_root,
        });
        // Tests drive `run_once` directly and never start the thread; in
        // production the first submission starts it.
        if cfg!(not(test)) {
            self.ensure_worker(&mut shared);
        }
        condvar.notify_one();
        drop(shared);

        ControlReceipt::queued(descriptor, surface, target, ticket)
    }

    /// Take every terminal receipt produced since the last drain.
    #[must_use]
    pub fn drain_completed(&self) -> Vec<ControlReceipt> {
        let (lock, _) = &*self.shared;
        match lock.lock() {
            Ok(mut shared) => std::mem::take(&mut shared.completed),
            Err(_) => Vec::new(),
        }
    }

    /// How many submissions are waiting. Test-only: nothing in the UI reads
    /// backpressure yet, and an unused public accessor would be a claim the
    /// build does not back.
    #[cfg(test)]
    #[must_use]
    pub fn pending_len(&self) -> usize {
        let (lock, _) = &*self.shared;
        lock.lock().map(|shared| shared.pending.len()).unwrap_or(0)
    }

    /// Execute one queued submission if there is one, on the calling thread.
    ///
    /// This is the worker's body, exposed so tests can drain deterministically
    /// without a thread. Returns `false` when the queue was empty.
    pub fn run_once(&self) -> bool {
        let (lock, _) = &*self.shared;
        let Some(submission) = ({
            let Ok(mut shared) = lock.lock() else {
                return false;
            };
            shared.pending.pop_front()
        }) else {
            return false;
        };

        // The blocking call. Deliberately outside the lock so a slow teardown
        // never blocks a submission or a drain.
        let receipt = execute_lane_control_in(
            ControlSurface::Cli,
            submission.operation,
            submission.raw_target.as_deref(),
            submission.registry_root.as_deref(),
        );
        // The work ran off the composer thread, but it was *requested* from the
        // slash surface; the receipt must say so rather than impersonating the
        // CLI.
        let mut receipt = receipt.with_ticket(submission.ticket.clone());
        receipt.surface = ControlSurface::Slash;

        if let Ok(mut shared) = lock.lock() {
            shared
                .in_flight
                .retain(|(ticket, _)| *ticket != submission.ticket);
            shared.completed.push(receipt);
        }
        true
    }

    fn run(&self) {
        let (lock, condvar) = &*self.shared;
        loop {
            {
                let Ok(mut shared) = lock.lock() else {
                    return;
                };
                while shared.pending.is_empty() && !shared.shutdown {
                    let Ok(next) = condvar.wait(shared) else {
                        return;
                    };
                    shared = next;
                }
                if shared.shutdown && shared.pending.is_empty() {
                    return;
                }
            }
            self.run_once();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_lane::{LaneRegistry, LaneStatus, LifecycleOutcome, RuntimeBackendKind};

    fn seeded() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let registry = LaneRegistry::open(dir.path()).unwrap();
        let record = registry
            .create_pending(
                Some("stopship".into()),
                Some("stopship".into()),
                Some("4022".into()),
                None,
                RuntimeBackendKind::Inline,
                None,
            )
            .unwrap();
        let id = record.id.clone();
        (dir, id)
    }

    /// #4022: the production slash path returns immediately with a typed
    /// `queued` receipt and only then performs the blocking teardown.
    #[test]
    fn slash_interrupt_returns_queued_then_completes_off_loop() {
        let (dir, id) = seeded();
        let queue = LaneControlQueue::new();

        let submitted = queue.submit(
            ControlOperation::LaneInterrupt,
            Some(id.as_str()),
            Some(dir.path().to_path_buf()),
        );
        assert_eq!(submitted.outcome, LifecycleOutcome::Queued);
        assert_eq!(submitted.surface, ControlSurface::Slash);
        assert!(submitted.ticket.is_some());
        assert!(
            !submitted.is_error(),
            "queued is not an error, but it is also not success"
        );
        // Nothing has happened to durable state yet.
        assert_eq!(
            LaneRegistry::open(dir.path())
                .unwrap()
                .load(&id)
                .unwrap()
                .status,
            LaneStatus::Pending
        );

        assert!(queue.run_once());
        let completed = queue.drain_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].ticket, submitted.ticket);
        assert_eq!(completed[0].outcome, LifecycleOutcome::Transitioned);
        assert_eq!(
            completed[0].surface,
            ControlSurface::Slash,
            "the receipt reports who asked, not which thread ran it"
        );
        assert_eq!(
            LaneRegistry::open(dir.path())
                .unwrap()
                .load(&id)
                .unwrap()
                .status,
            LaneStatus::Stopped
        );
    }

    /// A malformed id is refused on the calling thread, not queued.
    #[test]
    fn invalid_targets_are_refused_synchronously() {
        let queue = LaneControlQueue::new();
        for bad in [None, Some(""), Some("../escape"), Some("a b")] {
            let receipt = queue.submit(ControlOperation::LaneInterrupt, bad, None);
            assert_eq!(
                receipt.failure.as_ref().map(|failure| failure.kind),
                Some(ControlFailureKind::InvalidTarget),
                "{bad:?}"
            );
            assert!(receipt.ticket.is_none());
        }
        assert_eq!(queue.pending_len(), 0);
    }

    /// #1888: a verb with no backend must be refused on the calling thread.
    /// Queueing it would answer `queued` — a receipt that implies work is
    /// under way — for work that can never run.
    #[test]
    fn declared_but_unbuilt_verbs_are_refused_without_queueing() {
        let queue = LaneControlQueue::new();
        for operation in [ControlOperation::LaneRestart, ControlOperation::LaneResume] {
            let receipt = queue.submit(operation, Some("lane-a1b2c3d4"), None);
            assert_eq!(receipt.outcome, LifecycleOutcome::Rejected);
            assert_eq!(
                receipt.availability.reason(),
                Some(codewhale_lane::UnavailableReason::BackendNotImplemented),
                "{operation:?}"
            );
            assert!(receipt.ticket.is_none());
            assert!(!receipt.retryable);
        }
        assert_eq!(queue.pending_len(), 0, "nothing may have been enqueued");
    }

    /// #1888: a saturated queue refuses with a typed, retryable receipt rather
    /// than growing without bound or blocking the composer.
    #[test]
    fn a_saturated_queue_refuses_without_submitting() {
        let queue = LaneControlQueue::new();
        for index in 0..MAX_PENDING {
            let receipt = queue.submit(
                ControlOperation::LaneInterrupt,
                Some(format!("lane-{index:08}").as_str()),
                None,
            );
            assert_eq!(receipt.outcome, LifecycleOutcome::Queued);
        }
        assert_eq!(queue.pending_len(), MAX_PENDING);

        let refused = queue.submit(ControlOperation::LaneInterrupt, Some("lane-overflow"), None);
        assert_eq!(refused.outcome, LifecycleOutcome::Rejected);
        assert_eq!(
            refused.failure.as_ref().map(|failure| failure.kind),
            Some(ControlFailureKind::Saturated)
        );
        assert!(refused.retryable, "saturation clears on its own");
        assert!(refused.ticket.is_none());
        assert_eq!(
            queue.pending_len(),
            MAX_PENDING,
            "the refused submission must not have been enqueued"
        );
    }

    /// Re-pressing interrupt while a teardown is in flight is a conflict, not
    /// a second teardown of the same Lane.
    #[test]
    fn a_duplicate_submission_for_one_lane_is_a_conflict() {
        let queue = LaneControlQueue::new();
        let first = queue.submit(ControlOperation::LaneInterrupt, Some("lane-a1b2c3d4"), None);
        assert_eq!(first.outcome, LifecycleOutcome::Queued);

        let second = queue.submit(ControlOperation::LaneInterrupt, Some("lane-a1b2c3d4"), None);
        assert_eq!(
            second.failure.as_ref().map(|failure| failure.kind),
            Some(ControlFailureKind::Conflict)
        );
        assert_eq!(queue.pending_len(), 1);
    }

    /// #4022 (TUI responsiveness): submission must not perform the blocking
    /// work. A queue with no worker still returns promptly, which is only
    /// possible if `submit` never touches the Runtime.
    #[test]
    fn submission_does_not_block_on_runtime_teardown() {
        let (dir, id) = seeded();
        let queue = LaneControlQueue::new();
        let started = std::time::Instant::now();
        let receipt = queue.submit(
            ControlOperation::LaneInterrupt,
            Some(id.as_str()),
            Some(dir.path().to_path_buf()),
        );
        let elapsed = started.elapsed();
        assert_eq!(receipt.outcome, LifecycleOutcome::Queued);
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "submission took {elapsed:?}; it must not run teardown inline"
        );
        // Proof it really was deferred: durable state is untouched until a
        // worker runs, and no worker has.
        assert_eq!(
            LaneRegistry::open(dir.path())
                .unwrap()
                .load(&id)
                .unwrap()
                .status,
            LaneStatus::Pending
        );
    }

    /// The fence still refuses under the registry lock when the work finally
    /// runs off-loop.
    #[test]
    fn a_stale_fence_conflicts_when_the_queued_work_runs() {
        let (dir, id) = seeded();
        let queue = LaneControlQueue::new();
        let submitted = queue.submit(
            ControlOperation::LaneInterrupt,
            Some(format!("{id}@99").as_str()),
            Some(dir.path().to_path_buf()),
        );
        assert_eq!(submitted.outcome, LifecycleOutcome::Queued);

        assert!(queue.run_once());
        let completed = queue.drain_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].outcome, LifecycleOutcome::Rejected);
        assert_eq!(
            completed[0].failure.as_ref().map(|failure| failure.kind),
            Some(ControlFailureKind::Conflict)
        );
        assert_eq!(
            LaneRegistry::open(dir.path())
                .unwrap()
                .load(&id)
                .unwrap()
                .status,
            LaneStatus::Pending,
            "a refused fence must not have torn anything down"
        );
    }

    /// A completed ticket frees its slot, so the queue does not leak capacity.
    #[test]
    fn completing_a_submission_frees_its_in_flight_slot() {
        let (dir, id) = seeded();
        let queue = LaneControlQueue::new();
        let _ = queue.submit(
            ControlOperation::LaneInterrupt,
            Some(id.as_str()),
            Some(dir.path().to_path_buf()),
        );
        assert!(queue.run_once());
        let _ = queue.drain_completed();

        let again = queue.submit(
            ControlOperation::LaneInterrupt,
            Some(id.as_str()),
            Some(dir.path().to_path_buf()),
        );
        assert_eq!(
            again.outcome,
            LifecycleOutcome::Queued,
            "the slot must be reusable once the ticket completes"
        );
    }
}

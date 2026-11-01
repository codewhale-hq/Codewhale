//! Dedicated persistence actor for session save / checkpoint I/O.
//!
//! ## Motivation
//!
//! Before this module, `persist_checkpoint` and `persist_session_snapshot` ran
//! synchronously on the tokio worker thread that drives the TUI event loop.
//! Each call serialised all API messages to JSON, wrote a temp file, and
//! renamed it atomically — blocking keyboard input for the duration.
//! `save_session` additionally called `cleanup_old_sessions`, which listed all
//! session files, parsed metadata from every one, sorted, and deleted the
//! oldest — scaling O(session-bytes + file-count) with every turn.
//!
//! ## Design
//!
//! - **One dedicated tokio task** owns disk I/O. The UI only sends requests;
//!   keystrokes never wait for writes.
//! - **Latest-wins coalescing per session**: when multiple `SaveCheckpoint`,
//!   `SessionSnapshot`, or offline-queue requests pile up before the actor's
//!   next write cycle, only the most recent one per session is written.
//!   Checkpoints and clears are keyed by session id, so concurrent sessions
//!   never coalesce into (or clear) each other's slot.
//! - **Durability reporting**: `FlushAndReport` returns accumulated results;
//!   every failed write is also logged with its OS error when it happens.
//! - **Bounded command channel with sender-side coalescing** (#6212):
//!   `try_send` absorbs each request into a shared latest-wins state at send
//!   time and wakes the actor through a small bounded channel, so a paused
//!   consumer retains one snapshot per session instead of one per send.
//!   Queued snapshots retain only the canonical journal; legacy `messages`
//!   are derived at the disk boundary instead of doubling every paused
//!   request.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use codewhale_engine::session_manager::{
    OfflineQueueLease, OfflineQueueState, SavedSession, SessionManager,
};
use codewhale_util::utils::spawn_supervised;

// ---------------------------------------------------------------------------
// Request type
// ---------------------------------------------------------------------------

/// Persistence work item sent to the actor.
#[derive(Debug)]
pub enum PersistRequest {
    /// Write a crash-recovery checkpoint (in-flight turn state) to the
    /// session's own file (`checkpoints/<session_id>.json`).
    SaveCheckpoint { session: SavedSession },
    AcknowledgedCheckpoint {
        session: SavedSession,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Write a full session snapshot (completed turn, durable save).
    SessionSnapshot(SavedSession),
    /// Compound completion commit: write the completed session snapshot,
    /// and only if that write succeeds, clear that same session's
    /// crash-recovery checkpoint. A failed snapshot write RETAINS the
    /// checkpoint as the only surviving recovery record, and the clear is
    /// scoped to the committed session's id — it can never remove another
    /// session's checkpoint. Turn completion must send this instead of a
    /// separate snapshot and checkpoint clear, which the actor could
    /// otherwise apply with the clear first, erasing the recovery record
    /// before the snapshot safely landed.
    CompletedCommit { session: SavedSession },
    /// Write queued/draft offline input for crash recovery.
    OfflineQueue {
        state: OfflineQueueState,
        lease: Arc<OfflineQueueLease>,
    },
    /// Remove the queued/draft offline input file.
    ClearOfflineQueue {
        /// Captures the exact owner and retains its exclusive editor lease
        /// until the removal finishes. An unowned clear is unrepresentable.
        lease: Arc<OfflineQueueLease>,
    },
    /// Flush all pending work now and report durability results through
    /// `reply`. The report aggregates every write/removal result since the
    /// previous report (including background write cycles) — errors are
    /// collected and surfaced, never discarded.
    FlushAndReport { reply: oneshot::Sender<FlushReport> },
    /// Graceful shutdown — flush pending writes, then exit the actor loop.
    Shutdown,
}

/// Aggregated durability results: how many writes/removals completed and
/// which failed (labelled by what was being persisted, with the I/O error
/// kind).
#[derive(Debug, Default)]
pub struct FlushReport {
    pub completed: usize,
    pub failures: Vec<(String, std::io::ErrorKind)>,
}

impl FlushReport {
    /// Upper bound on retained failure entries when accumulating across
    /// write cycles. Every failure is logged at the cycle it happened, so
    /// dropping older-than-bound entries from the reply loses no evidence.
    const MAX_ACCUMULATED_FAILURES: usize = 256;

    fn merge(&mut self, other: FlushReport) {
        self.completed += other.completed;
        self.failures.extend(other.failures);
        if self.failures.len() > Self::MAX_ACCUMULATED_FAILURES {
            let excess = self.failures.len() - Self::MAX_ACCUMULATED_FAILURES;
            self.failures.drain(..excess);
        }
    }
}

#[derive(Debug)]
enum PendingOfflineQueue {
    Save {
        state: Box<OfflineQueueState>,
        lease: Arc<OfflineQueueLease>,
    },
    Clear {
        lease: Arc<OfflineQueueLease>,
    },
}

// ---------------------------------------------------------------------------
// Handle (held by the TUI)
// ---------------------------------------------------------------------------

/// Control commands the actor reacts to. Work itself never crosses the
/// channel: requests are coalesced into the shared [`PendingState`] at send
/// time, so a paused consumer retains at most the latest request per session
/// instead of every snapshot ever sent (#6212).
enum ActorCommand {
    /// The shared pending state has work the actor has not taken yet.
    WorkReady,
    FlushAndReport {
        reply: oneshot::Sender<FlushReport>,
    },
    Shutdown,
}

/// Command-channel capacity. `WorkReady` is deduplicated by the `notified`
/// flag, so only `FlushAndReport`/`Shutdown` can occupy slots unplanned; the
/// capacity exists so those never observe a full channel in practice.
const ACTOR_COMMAND_CAPACITY: usize = 8;

/// The coalescing state shared between senders and the actor. Senders absorb
/// under the lock; the actor `take`s (swap to empty) under the same lock and
/// flushes outside it, so disk I/O never blocks a sender.
#[derive(Debug, Default)]
struct SharedPending {
    closed: bool,
    pending: PendingState,
    /// Whether a `WorkReady` is already queued (or being queued) and no
    /// `take_pending` has observed it since. Reset only by `take_pending` —
    /// the receiver side — so the flag always reflects the channel the actor
    /// drains.
    notified: bool,
}

#[derive(Clone)]
struct PersistRequestSender {
    shared: Arc<std::sync::Mutex<SharedPending>>,
    cmd_tx: mpsc::Sender<ActorCommand>,
    health: SessionSaveHealth,
}

impl std::fmt::Debug for PersistRequestSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistRequestSender")
            .finish_non_exhaustive()
    }
}

struct PersistRequestReceiver {
    shared: Arc<std::sync::Mutex<SharedPending>>,
    cmd_rx: mpsc::Receiver<ActorCommand>,
    health: SessionSaveHealth,
}

/// Single construction seam for the production persistence request channel.
///
/// The ignored backlog measurement uses this same factory with the receiver
/// deliberately paused, so the measurement always characterizes whatever
/// representation the seam actually retains.
fn persistence_request_channel() -> (PersistRequestSender, PersistRequestReceiver) {
    let (cmd_tx, cmd_rx) = mpsc::channel(ACTOR_COMMAND_CAPACITY);
    let shared = Arc::new(std::sync::Mutex::new(SharedPending::default()));
    let health = SessionSaveHealth::default();
    (
        PersistRequestSender {
            shared: Arc::clone(&shared),
            cmd_tx,
            health: health.clone(),
        },
        PersistRequestReceiver {
            shared,
            cmd_rx,
            health,
        },
    )
}

impl Drop for PersistRequestReceiver {
    fn drop(&mut self) {
        let mut guard = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.closed = true;
        guard.notified = false;
        for (_, reply) in std::mem::take(&mut guard.pending.checkpoint_replies) {
            let _ = reply.send(Err(
                "persistence actor shut down before checkpoint write".into()
            ));
        }
    }
}

impl PersistRequestReceiver {
    /// Await the next actor command. `None` means every sender is gone.
    async fn recv(&mut self) -> Option<ActorCommand> {
        self.cmd_rx.recv().await
    }

    /// Atomically take everything coalesced so far. Resets the `notified`
    /// flag so the next absorbed request queues a fresh `WorkReady`.
    fn take_pending(&mut self) -> PendingState {
        let mut guard = self
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.notified = false;
        std::mem::take(&mut guard.pending)
    }
}

/// Lightweight handle that the UI holds to queue persistence work.
#[derive(Debug, Clone)]
pub struct PersistActorHandle {
    tx: PersistRequestSender,
}

impl PersistActorHandle {
    async fn flush(&self, timeout: Duration) -> Result<FlushReport, String> {
        tokio::time::timeout(timeout, async {
            let (reply, receiver) = oneshot::channel();
            self.tx
                .cmd_tx
                .send(ActorCommand::FlushAndReport { reply })
                .await
                .map_err(|_| "session persistence actor is unavailable".to_string())?;
            receiver
                .await
                .map_err(|_| "session persistence stopped before flush acknowledgement".to_string())
        })
        .await
        .map_err(|_| "session persistence flush timed out".to_string())?
    }

    /// Whether the latest save of each session document landed.
    pub(crate) fn session_save_health(&self) -> SaveHealthReading {
        self.tx.health.reading()
    }

    /// Queue a persistence request without blocking. The request is
    /// coalesced into the shared pending state immediately (latest-wins per
    /// session), so repeated snapshots of one session retain only the
    /// newest. Returns `false` when the actor is already shut down.
    pub fn try_send(&self, mut request: PersistRequest) -> bool {
        if self.tx.cmd_tx.is_closed() {
            return false;
        }
        match &mut request {
            PersistRequest::SaveCheckpoint { session }
            | PersistRequest::AcknowledgedCheckpoint { session, .. }
            | PersistRequest::SessionSnapshot(session)
            | PersistRequest::CompletedCommit { session } => {
                session.compact_for_persistence_queue();
            }
            _ => {}
        }
        let control = {
            let mut guard = self
                .tx
                .shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.closed {
                return false;
            }
            guard.pending.absorb(request)
        };
        match control {
            Control::Continue => {
                // A WorkReady the actor has not taken yet is already queued;
                // that take will observe this request too. `notified` resets
                // only when the actor takes, so it always mirrors the
                // channel the actor drains.
                {
                    let mut guard = self
                        .tx
                        .shared
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if guard.notified {
                        return true;
                    }
                    guard.notified = true;
                }
                if self.tx.cmd_tx.try_send(ActorCommand::WorkReady).is_ok() {
                    true
                } else {
                    // Roll the flag back so later sends re-attempt (and
                    // re-fail) honestly instead of riding a dead wake.
                    let mut guard = self
                        .tx
                        .shared
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.notified = false;
                    false
                }
            }
            Control::Flush(reply) => self
                .tx
                .cmd_tx
                .try_send(ActorCommand::FlushAndReport { reply })
                .is_ok(),
            Control::Shutdown => self.tx.cmd_tx.try_send(ActorCommand::Shutdown).is_ok(),
        }
    }
}

// ---------------------------------------------------------------------------
// Global singleton (avoid threading through App)
// ---------------------------------------------------------------------------

static ACTOR_TX: OnceLock<PersistActorHandle> = OnceLock::new();

static SIGNAL_CHECKPOINT: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>> = Mutex::new(None);

pub(crate) fn register_signal_checkpoint() -> oneshot::Receiver<oneshot::Sender<()>> {
    let (tx, rx) = oneshot::channel();
    *SIGNAL_CHECKPOINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    rx
}

pub(crate) async fn flush_for_signal_exit() {
    let Some(handle) = ACTOR_TX.get() else {
        return;
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let owner = SIGNAL_CHECKPOINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(owner) = owner {
        let (acknowledge, settled) = oneshot::channel();
        if owner.send(acknowledge).is_ok()
            && tokio::time::timeout(Duration::from_secs(1), settled)
                .await
                .is_err()
        {
            tracing::warn!("terminal session did not settle within the signal shutdown window");
        }
    }
    match handle
        .flush(deadline.saturating_duration_since(tokio::time::Instant::now()))
        .await
    {
        Ok(report) if !report.failures.is_empty() => tracing::warn!(
            target: "persistence",
            failures = ?report.failures,
            "session persistence reported write failures during signal shutdown",
        ),
        Err(error) => tracing::warn!(
            target: "persistence",
            %error,
            "session persistence did not acknowledge flush during signal shutdown",
        ),
        Ok(_) => {}
    }
}

/// Initialise the global persistence actor handle. Must be called once at
/// startup, before the event loop starts.
pub fn init_actor(handle: PersistActorHandle) {
    let _ = ACTOR_TX.set(handle);
}

/// Queue a persistence request through the global handle. When the request
/// cannot be queued — actor not initialised yet (tests, early startup) or
/// already shut down — the drop is logged instead of discarded silently, so
/// lost session/work-graph state is diagnosable after the fact.
pub fn persist(request: PersistRequest) {
    let _ = persist_checked(request);
}

pub(crate) fn persist_checked(request: PersistRequest) -> Result<(), String> {
    let label = request_label(&request);
    if try_persist(request) {
        return Ok(());
    }
    if ACTOR_TX.get().is_some() {
        tracing::warn!(
            request = label,
            "persistence request dropped: actor channel is closed (shutdown already happened)"
        );
        Err("persistence actor is unavailable".to_string())
    } else {
        tracing::debug!(
            request = label,
            "persistence request dropped: actor not initialised yet"
        );
        Ok(())
    }
}

/// Order synchronous lifecycle saves after all previously queued snapshots.
/// Refuse a single-thread runtime rather than deadlocking its persistence task.
pub(crate) fn flush_before_transition() -> Result<(), String> {
    let Some(handle) = ACTOR_TX.get() else {
        return Ok(());
    };
    let runtime = tokio::runtime::Handle::try_current().ok();
    if runtime
        .as_ref()
        .is_some_and(|r| r.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread)
    {
        return Err("session transition requires an asynchronous persistence barrier".into());
    }
    let (reply, receiver) = oneshot::channel();
    if !handle.try_send(PersistRequest::FlushAndReport { reply }) {
        return Err("session transition could not queue persistence barrier".into());
    }
    let receive = || receiver.blocking_recv();
    let report = if runtime.is_some() {
        tokio::task::block_in_place(receive)
    } else {
        receive()
    }
    .map_err(|_| "session persistence stopped before the transition".to_string())?;
    if !report.failures.is_empty() {
        return Err(format!(
            "session transition refused after persistence failures: {:?}",
            report.failures
        ));
    }
    Ok(())
}

fn request_label(request: &PersistRequest) -> &'static str {
    match request {
        PersistRequest::SaveCheckpoint { .. } => "SaveCheckpoint",
        PersistRequest::AcknowledgedCheckpoint { .. } => "AcknowledgedCheckpoint",
        PersistRequest::SessionSnapshot(_) => "SessionSnapshot",
        PersistRequest::CompletedCommit { .. } => "CompletedCommit",
        PersistRequest::OfflineQueue { .. } => "OfflineQueue",
        PersistRequest::ClearOfflineQueue { .. } => "ClearOfflineQueue",
        PersistRequest::FlushAndReport { .. } => "FlushAndReport",
        PersistRequest::Shutdown => "Shutdown",
    }
}

/// Queue persistence and report whether the actor accepted ownership. Work
/// Graph projections use this acknowledgement as their publish boundary.
/// [`PersistActorHandle::session_save_health`] of the global actor; `None`
/// before it starts.
pub(crate) fn session_save_health() -> Option<SaveHealthReading> {
    ACTOR_TX.get().map(PersistActorHandle::session_save_health)
}

pub fn try_persist(request: PersistRequest) -> bool {
    ACTOR_TX
        .get()
        .is_some_and(|handle| handle.try_send(request))
}

// ---------------------------------------------------------------------------
// Actor spawn
// ---------------------------------------------------------------------------

/// Spawn the persistence actor task and return a handle for the caller to
/// store and initialise.
///
/// The returned handle should be passed to [`init_actor`] so that the
/// `persist()` free function can reach it from anywhere in the TUI.
pub fn spawn_persistence_actor(
    manager: SessionManager,
) -> (PersistActorHandle, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = persistence_request_channel();
    let handle = PersistActorHandle { tx };

    let task = spawn_supervised(
        "persistence-actor",
        std::panic::Location::caller(),
        async move {
            let mut unreported = FlushReport::default();

            // Flush pending work, log new failures, and fold the cycle's
            // results into the unreported accumulator.
            fn flush_cycle(
                manager: &SessionManager,
                pending: &mut PendingState,
                unreported: &mut FlushReport,
                health: &SessionSaveHealth,
            ) {
                let cycle = flush_inner(manager, pending, health);
                unreported.merge(cycle);
            }

            // Work is coalesced at send time into the shared pending state;
            // every command handler takes whatever has accumulated and
            // flushes it outside the sender lock.
            while let Some(command) = rx.recv().await {
                let mut pending = rx.take_pending();
                match command {
                    ActorCommand::WorkReady => {
                        if !pending.is_empty() {
                            flush_cycle(&manager, &mut pending, &mut unreported, &rx.health);
                        }
                    }
                    ActorCommand::FlushAndReport { reply } => {
                        flush_cycle(&manager, &mut pending, &mut unreported, &rx.health);
                        let _ = reply.send(std::mem::take(&mut unreported));
                    }
                    ActorCommand::Shutdown => {
                        flush_cycle(&manager, &mut pending, &mut unreported, &rx.health);
                        return;
                    }
                }
            }
            // Every sender is gone — final flush and exit. Each absorb
            // guarantees a queued WorkReady, so this is normally empty.
            let mut pending = rx.take_pending();
            flush_cycle(&manager, &mut pending, &mut unreported, &rx.health);
        },
    );

    (handle, task)
}

#[derive(Debug)]
enum PendingSession {
    Snapshot(SavedSession),
    Completion(SavedSession),
}

/// Coalesced work waiting for the next write cycle.
#[derive(Debug, Default)]
struct PendingState {
    /// Latest-wins per session id. Crash checkpoints are keyed per session
    /// (mirroring `sessions` below) so concurrent sessions can interleave
    /// saves and clears without clobbering each other.
    checkpoints: BTreeMap<String, SavedSession>,
    checkpoint_replies: BTreeMap<String, oneshot::Sender<Result<(), String>>>,
    /// Latest-wins per session id. Coalescing into one global slot can
    /// drop session A when an immediate `/new` queues session B before
    /// the actor drains.
    sessions: BTreeMap<String, PendingSession>,
    /// Compound completion commits, latest-wins per session id: the
    /// completed snapshot body to write, followed by that session's own
    /// checkpoint clear — the clear only if the write succeeded. A newer
    /// snapshot can replace the body while retaining the clear intent until
    /// an explicit clear changes that ordering.
    completed_commits: BTreeMap<String, PendingSession>,
    /// Latest-wins per session id, for the same reason `sessions` above is:
    /// a single global slot dropped session A's queued text when session B
    /// queued before the actor drained, which defeats the per-session file
    /// naming entirely. Each pending request retains its editor lease, so a
    /// window changing session cannot release ownership ahead of its writes.
    offline_queue: BTreeMap<String, PendingOfflineQueue>,
}

/// What the actor loop should do after absorbing a request.
enum Control {
    Continue,
    Flush(oneshot::Sender<FlushReport>),
    Shutdown,
}

impl PendingState {
    /// True when nothing coalesced is waiting. An empty `WorkReady` take is
    /// skipped instead of running a no-op flush cycle.
    fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
            && self.sessions.is_empty()
            && self.completed_commits.is_empty()
            && self.offline_queue.is_empty()
    }

    fn absorb(&mut self, req: PersistRequest) -> Control {
        let replaced = match &req {
            PersistRequest::SaveCheckpoint { session }
            | PersistRequest::AcknowledgedCheckpoint { session, .. }
            | PersistRequest::SessionSnapshot(session)
            | PersistRequest::CompletedCommit { session } => Some(session.metadata.id.as_str()),
            _ => None,
        };
        if let Some(id) = replaced
            && let Some(reply) = self.checkpoint_replies.remove(id)
        {
            let _ = reply.send(Err("checkpoint superseded before persistence".into()));
        }
        match req {
            PersistRequest::AcknowledgedCheckpoint { session, reply } => {
                let id = session.metadata.id.clone();
                self.absorb(PersistRequest::SaveCheckpoint { session });
                self.checkpoint_replies.insert(id, reply);
            }
            PersistRequest::SaveCheckpoint { session } => {
                // Last-writer-wins per session: a fresh checkpoint supersedes
                // a pending clear for the same session so the two never both
                // apply in one drain (which previously cleared then re-wrote
                // the stale checkpoint, undoing the clear).
                let id = session.metadata.id.clone();
                // A new in-flight checkpoint means newer turn work started
                // after the completion it would have cleared: the compound's
                // clear-on-success intent is stale now (it would erase the
                // fresher recovery record), but the completed snapshot still
                // needs to be saved.
                if let Some(completed) = self.completed_commits.remove(&id) {
                    tracing::debug!(
                        session_id = %id,
                        "pending completed commit retained beside a newer in-flight checkpoint"
                    );
                    self.sessions.insert(id.clone(), completed);
                }
                self.checkpoints.insert(id, session);
            }
            PersistRequest::SessionSnapshot(session) => {
                // A newer full snapshot of a session with a pending
                // completed commit refreshes the commit's body (and keeps
                // its clear-on-success intent): the in-flight checkpoint it
                // guards is already captured by the newer completed state.
                let id = session.metadata.id.clone();
                if let Some(pending) = self.completed_commits.get_mut(&id) {
                    *pending = PendingSession::Snapshot(session);
                } else {
                    self.sessions.insert(id, PendingSession::Snapshot(session));
                }
            }
            PersistRequest::CompletedCommit { session } => {
                let id = session.metadata.id.clone();
                // The compound owns this session's snapshot and clear: a
                // pending plain snapshot is superseded, a pending standalone
                // clear would erase the recovery record even when the save
                // fails, and a pending checkpoint write would re-create the
                // record after the compound cleared it.
                self.sessions.remove(&id);
                self.checkpoints.remove(&id);
                self.completed_commits
                    .insert(id, PendingSession::Completion(session));
            }
            PersistRequest::OfflineQueue { state, lease } => {
                self.offline_queue.insert(
                    lease.session_id().to_string(),
                    PendingOfflineQueue::Save {
                        state: Box::new(state),
                        lease,
                    },
                );
            }
            PersistRequest::ClearOfflineQueue { lease } => {
                // A clear supersedes a pending save for its OWN session only.
                self.offline_queue.insert(
                    lease.session_id().to_string(),
                    PendingOfflineQueue::Clear { lease },
                );
            }
            PersistRequest::FlushAndReport { reply } => return Control::Flush(reply),
            PersistRequest::Shutdown => return Control::Shutdown,
        }
        Control::Continue
    }
}

/// Write all pending work to disk, draining `pending`. Every write and
/// removal result is collected into the returned [`FlushReport`] — failures
/// are reported, never silently discarded.
///
/// Ordering is durability-critical: every session snapshot write (plain or
/// completion commit) happens BEFORE any checkpoint clear. A completion
/// commit clears its session's checkpoint only after that session's own
/// write succeeded, so a failed save always leaves the crash-recovery
/// checkpoint in place.
fn flush_inner(
    manager: &SessionManager,
    pending: &mut PendingState,
    health: &SessionSaveHealth,
) -> FlushReport {
    let mut report = FlushReport::default();
    let mut record = |what: String, result: std::io::Result<()>| match result {
        Ok(()) => report.completed += 1,
        Err(err) => {
            let raw_os_error = err.raw_os_error().or_else(|| {
                std::iter::successors(
                    err.get_ref()
                        .map(|inner| inner as &(dyn std::error::Error + 'static)),
                    |error| error.source(),
                )
                .find_map(|error| error.downcast_ref::<std::io::Error>()?.raw_os_error())
            });
            tracing::warn!(
                target: "persistence",
                what = %what,
                error_kind = ?err.kind(),
                raw_os_error = ?raw_os_error,
                error = %err,
                "persistence write failed",
            );
            report.failures.push((what, err.kind()));
        }
    };

    // Document saves are bounded (#6842): an oversized journal is archived
    // and pruned here, on the actor, never on the UI loop.
    for (session_id, pending_session) in std::mem::take(&mut pending.sessions) {
        let session = match pending_session {
            PendingSession::Snapshot(session) | PendingSession::Completion(session) => session,
        };
        let result = manager.save_session_bounded(session).map(|_| ());
        health.record(&session_id, &result);
        record(format!("session:{session_id}"), result);
    }
    for (session_id, pending_session) in std::mem::take(&mut pending.completed_commits) {
        let session = match pending_session {
            PendingSession::Snapshot(session) | PendingSession::Completion(session) => session,
        };
        let commit_result = manager.save_session_bounded(session);
        health.record(&session_id, &commit_result);
        let save_succeeded = commit_result.is_ok();
        record(
            format!("completed-commit:{session_id}"),
            commit_result.map(|_| ()),
        );
        if save_succeeded {
            // Only the committed session's own checkpoint is cleared, and
            // only because its snapshot safely landed. A failure above
            // retains the checkpoint as the sole recovery record.
            record(
                format!("clear-checkpoint:{session_id}"),
                manager.clear_session_checkpoint(&session_id),
            );
        }
    }
    for (session_id, session) in std::mem::take(&mut pending.checkpoints) {
        let reply = pending.checkpoint_replies.remove(&session_id);
        if reply.as_ref().is_some_and(oneshot::Sender::is_closed) {
            continue;
        }
        let result = manager.save_checkpoint_owned(session).map(|_| ());
        if let Some(reply) = reply {
            let _ = reply.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        }
        record(format!("checkpoint:{session_id}"), result);
    }
    for (_, request) in std::mem::take(&mut pending.offline_queue) {
        match request {
            PendingOfflineQueue::Save { state, lease } => record(
                format!("offline-queue:{}", lease.session_id()),
                manager
                    .save_offline_queue_state(&state, Some(lease.session_id()))
                    .map(|_| ()),
            ),
            PendingOfflineQueue::Clear { lease } => record(
                format!("clear-offline-queue:{}", lease.session_id()),
                manager.clear_offline_queue_state_for(lease.session_id()),
            ),
        }
    }
    report
}

/// Which session documents' latest save failed. Only document saves count: a
/// failed checkpoint, queue or cleanup write does not lose the conversation.
/// A later successful save of the same session clears its entry, so a
/// transient failure does not leave a standing alarm.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionSaveHealth(Arc<std::sync::Mutex<SaveHealthState>>);

#[derive(Debug, Default)]
struct SaveHealthState {
    /// Bumped whenever `failing` changes, so a poller can tell what is new.
    generation: u64,
    failing: BTreeMap<String, std::io::ErrorKind>,
}

/// One reading of [`SessionSaveHealth`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SaveHealthReading {
    pub(crate) generation: u64,
    /// A session whose latest save failed, and how, while any has.
    pub(crate) failing: Option<(String, std::io::ErrorKind)>,
}

impl SessionSaveHealth {
    fn record<T>(&self, session_id: &str, result: &std::io::Result<T>) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let changed = match result {
            Ok(_) => state.failing.remove(session_id).is_some(),
            Err(error) => {
                state.failing.insert(session_id.to_string(), error.kind()) != Some(error.kind())
            }
        };
        if changed {
            state.generation += 1;
        }
    }

    fn reading(&self) -> SaveHealthReading {
        let state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SaveHealthReading {
            generation: state.generation,
            failing: state
                .failing
                .iter()
                .next()
                .map(|(id, kind)| (id.clone(), *kind)),
        }
    }
}

#[cfg(test)]
#[path = "persistence_actor/tests.rs"]
mod backlog_measurement_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use codewhale_engine::session_manager::{OfflineQueueState, QueuedSessionMessage};

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if predicate() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for persistence actor"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn two_sessions_queueing_before_a_drain_both_survive() {
        // Per-session FILENAMES are not enough on their own: the actor
        // coalesces pending work before those names are ever used, and the
        // queue used one global slot while its checkpoint/session neighbours
        // were already keyed per session. Session A's unsent text was
        // therefore dropped whenever session B queued first.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let (handle, task) = spawn_persistence_actor(manager);

        let queue_manager = SessionManager::new(sessions_dir.clone()).expect("queue manager");
        let lease_a = queue_manager
            .acquire_offline_queue_lease("session-A")
            .expect("lease A");
        let lease_b = queue_manager
            .acquire_offline_queue_lease("session-B")
            .expect("lease B");
        for (session, body) in [("session-A", "text from A"), ("session-B", "text from B")] {
            let state = OfflineQueueState {
                messages: vec![QueuedSessionMessage {
                    display: body.to_string(),
                    skill_instruction: None,
                    skill_provenance: None,
                }],
                ..OfflineQueueState::default()
            };
            handle.try_send(PersistRequest::OfflineQueue {
                state,
                lease: Arc::clone(if session == "session-A" {
                    &lease_a
                } else {
                    &lease_b
                }),
            });
        }

        let checkpoints = sessions_dir.join("checkpoints");
        for (session, body) in [("session-A", "text from A"), ("session-B", "text from B")] {
            let path = checkpoints.join(format!("{session}.offline_queue.json"));
            // wait_until panics on timeout, which is the failure signal: a
            // coalesced-away queue never appears.
            wait_until(|| std::fs::read_to_string(&path).is_ok_and(|f| f.contains(body))).await;
        }

        // A clear names its own session and must not touch the other's.
        handle.try_send(PersistRequest::ClearOfflineQueue {
            lease: Arc::clone(&lease_a),
        });
        let a = checkpoints.join("session-A.offline_queue.json");
        wait_until(|| !a.exists()).await;
        assert!(
            checkpoints.join("session-B.offline_queue.json").exists(),
            "clearing one session must not delete another session's queued text"
        );

        drop(handle);
        let _ = task.await;
    }

    #[tokio::test]
    async fn actor_persists_and_clears_offline_queue_requests() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        // The queue is keyed per session now (#5715-adjacent data-loss fix):
        // two concurrent instances used to share one global file and the loser
        // lost its unsent text. The request below carries session-A.
        let queue_path = sessions_dir
            .join("checkpoints")
            .join("session-A.offline_queue.json");
        let lease = manager
            .acquire_offline_queue_lease("session-A")
            .expect("queue lease");
        let (handle, task) = spawn_persistence_actor(manager);

        let state = OfflineQueueState {
            messages: vec![QueuedSessionMessage {
                display: "queued from enter".to_string(),
                skill_instruction: None,
                skill_provenance: None,
            }],
            ..OfflineQueueState::default()
        };

        handle.try_send(PersistRequest::OfflineQueue {
            state,
            lease: Arc::clone(&lease),
        });
        wait_until(|| {
            std::fs::read_to_string(&queue_path)
                .is_ok_and(|body| body.contains("queued from enter"))
        })
        .await;

        handle.try_send(PersistRequest::ClearOfflineQueue {
            lease: Arc::clone(&lease),
        });
        wait_until(|| !queue_path.exists()).await;
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn shutdown_wait_flushes_queued_session_before_returning() {
        let (tx, paused_receiver) = persistence_request_channel();
        let paused = PersistActorHandle { tx };
        assert!(
            paused
                .flush(Duration::from_millis(10))
                .await
                .expect_err("paused actor must time out awaiting acknowledgement")
                .contains("timed out")
        );
        for _ in 1..ACTOR_COMMAND_CAPACITY {
            let (reply, _) = oneshot::channel();
            assert!(paused.try_send(PersistRequest::FlushAndReport { reply }));
        }
        assert_eq!(paused.tx.cmd_tx.capacity(), 0);
        assert!(
            paused
                .flush(Duration::from_millis(10))
                .await
                .expect_err("full command channel must time out while sending")
                .contains("timed out")
        );
        drop(paused_receiver);
        assert!(
            paused
                .flush(Duration::from_secs(2))
                .await
                .expect_err("closed actor must fail without waiting")
                .contains("unavailable")
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let (tx, mut receiver) = persistence_request_channel();
        let retained = PersistActorHandle { tx };
        assert!(retained.try_send(PersistRequest::Shutdown));
        assert!(matches!(
            receiver.recv().await,
            Some(ActorCommand::Shutdown)
        ));
        assert!(receiver.take_pending().is_empty());
        let (reply, late_checkpoint) = oneshot::channel();
        assert!(retained.try_send(PersistRequest::AcknowledgedCheckpoint {
            session: session.clone(),
            reply,
        }));
        drop(receiver);
        assert!(late_checkpoint.await.expect("shutdown rejection").is_err());
        let (reply, after_shutdown) = oneshot::channel();
        assert!(!retained.try_send(PersistRequest::AcknowledgedCheckpoint {
            session: session.clone(),
            reply,
        }));
        assert!(after_shutdown.await.is_err());
        let session_id = session.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(session));
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        let loaded = verification_manager
            .load_session(&session_id)
            .expect("shutdown must flush queued session");
        assert_eq!(loaded.metadata.id, session_id);
    }

    #[tokio::test]
    async fn shutdown_flushes_latest_snapshot_for_each_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let mut first = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        first.metadata.title = "Session A".to_string();
        let mut second = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        second.metadata.title = "Session B".to_string();
        let first_id = first.metadata.id.clone();
        let second_id = second.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(first));
        handle.try_send(PersistRequest::SessionSnapshot(second));
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        assert_eq!(
            verification_manager
                .load_session(&first_id)
                .expect("session A flushed")
                .metadata
                .title,
            "Session A"
        );
        assert_eq!(
            verification_manager
                .load_session(&second_id)
                .expect("session B flushed")
                .metadata
                .title,
            "Session B"
        );
    }

    #[tokio::test]
    async fn flush_and_report_returns_completed_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        for replacement in [
            PersistRequest::SaveCheckpoint {
                session: session.clone(),
            },
            PersistRequest::CompletedCommit {
                session: session.clone(),
            },
        ] {
            let mut pending = PendingState::default();
            let (reply, receiver) = oneshot::channel();
            pending.absorb(PersistRequest::AcknowledgedCheckpoint {
                session: session.clone(),
                reply,
            });
            pending.absorb(replacement);
            assert!(receiver.await.expect("supersession reply").is_err());
            assert!(pending.checkpoint_replies.is_empty());
        }
        let (handle, task) = spawn_persistence_actor(manager);

        assert!(handle.try_send(PersistRequest::SessionSnapshot(session.clone())));
        let (checkpoint_reply, checkpoint_result) = oneshot::channel();
        assert!(handle.try_send(PersistRequest::AcknowledgedCheckpoint {
            session: session.clone(),
            reply: checkpoint_reply
        }));
        let report = handle
            .flush(Duration::from_secs(2))
            .await
            .expect("flush report reply");
        assert!(checkpoint_result.await.expect("checkpoint reply").is_ok());
        // Whether the writes landed in an earlier background cycle or in this
        // flush, both must be counted with no failures; the actor keeps running.
        assert!(report.completed >= 2, "both writes must be counted");
        assert!(report.failures.is_empty(), "no failures expected");
        verification_manager
            .load_session(&session.metadata.id)
            .expect("flush must persist the queued session before shutdown");
        assert!(
            verification_manager
                .load_session_checkpoint(&session.metadata.id)
                .expect("load checkpoint after flush")
                .is_some(),
            "flush must retain the in-flight checkpoint"
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    /// The health the UI polls follows real saves through the actor: a failed
    /// checkpoint does not count as a lost conversation, a failed document
    /// save does, and a later successful save of that session clears it.
    #[tokio::test]
    async fn session_save_health_follows_document_saves_through_the_actor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        std::fs::write(sessions_dir.join("checkpoints"), b"not a directory")
            .expect("block checkpoints dir");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        // A directory where the document goes makes its save fail.
        let blocker = sessions_dir.join(format!("{session_id}.json"));
        std::fs::create_dir(&blocker).expect("block session document");
        let (handle, task) = spawn_persistence_actor(manager);
        let flush = |handle: &PersistActorHandle| {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
            reply_rx
        };

        handle.try_send(PersistRequest::SaveCheckpoint {
            session: session.clone(),
        });
        flush(&handle).await.expect("flush");
        assert_eq!(handle.session_save_health().failing, None);

        handle.try_send(PersistRequest::SessionSnapshot(session.clone()));
        flush(&handle).await.expect("flush");
        let failed = handle.session_save_health();
        assert_eq!(
            failed.failing.as_ref().map(|(id, _)| id.as_str()),
            Some(session_id.as_str())
        );

        std::fs::remove_dir(&blocker).expect("unblock");
        handle.try_send(PersistRequest::SessionSnapshot(session));
        flush(&handle).await.expect("flush");
        let healed = handle.session_save_health();
        assert_eq!(healed.failing, None, "a later save clears the failure");
        assert!(healed.generation > failed.generation);

        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn flush_and_report_propagates_write_failures() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        // Occupy the checkpoints directory path with a regular file so every
        // checkpoint write deterministically fails on all platforms.
        std::fs::write(sessions_dir.join("checkpoints"), b"not a directory")
            .expect("block checkpoints dir");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        let (checkpoint_reply, checkpoint_result) = oneshot::channel();
        assert!(handle.try_send(PersistRequest::AcknowledgedCheckpoint {
            session,
            reply: checkpoint_reply
        }));
        let report = handle
            .flush(Duration::from_secs(2))
            .await
            .expect("flush report reply");
        assert!(checkpoint_result.await.expect("checkpoint reply").is_err());

        assert!(
            report
                .failures
                .iter()
                .any(|(what, _)| what == &format!("checkpoint:{session_id}")),
            "failed checkpoint write must be reported, got: {:?}",
            report.failures
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    /// Pre-write a crash-recovery checkpoint file for `session_id` directly,
    /// so completion-commit tests can assert on its survival without needing
    /// a prior in-flight turn.
    fn seed_checkpoint_file(
        sessions_dir: &std::path::Path,
        session_id: &str,
    ) -> std::path::PathBuf {
        let path = sessions_dir
            .join("checkpoints")
            .join(format!("{session_id}.json"));
        std::fs::create_dir_all(path.parent().expect("checkpoints parent")).expect("mkdir");
        std::fs::write(&path, "{}").expect("seed checkpoint file");
        path
    }

    /// Deterministically fail session-file saves only: a directory at the
    /// session's own `<id>.json` path makes `save_session` fail while the
    /// checkpoints directory stays fully usable.
    fn block_session_file(sessions_dir: &std::path::Path, session_id: &str) {
        std::fs::create_dir_all(sessions_dir.join(format!("{session_id}.json")))
            .expect("block session file path");
    }

    #[tokio::test]
    async fn completed_commit_preserves_checkpoint_when_session_save_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let checkpoint_path = seed_checkpoint_file(&sessions_dir, &session_id);
        block_session_file(&sessions_dir, &session_id);
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::CompletedCommit {
            session: session.clone(),
        });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");

        assert!(
            report
                .failures
                .iter()
                .any(|(what, _)| what == &format!("completed-commit:{session_id}")),
            "failed session save must be reported, got: {:?}",
            report.failures
        );
        assert!(
            !report
                .failures
                .iter()
                .any(|(what, _)| what == &format!("clear-checkpoint:{session_id}")),
            "the checkpoint clear must not be attempted after a failed save"
        );
        assert!(
            checkpoint_path.exists(),
            "a failed session save must retain the crash-recovery checkpoint"
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn completed_commit_clears_only_after_session_save() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir.clone()).expect("verify");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let checkpoint_path = seed_checkpoint_file(&sessions_dir, &session_id);
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::CompletedCommit { session });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");

        assert!(
            report.failures.is_empty(),
            "commit save and its clear must both succeed, got: {:?}",
            report.failures
        );
        let saved = verification_manager
            .load_session(&session_id)
            .expect("completed session must be saved before its checkpoint is cleared");
        assert_eq!(saved.metadata.id, session_id);
        assert!(
            !checkpoint_path.exists(),
            "the checkpoint is cleared only after the session save succeeded"
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn completed_commit_never_clears_another_sessions_checkpoint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir.clone()).expect("verify");
        let committed = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let inflight = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let committed_id = committed.metadata.id.clone();
        let inflight_id = inflight.metadata.id.clone();
        let committed_checkpoint = seed_checkpoint_file(&sessions_dir, &committed_id);
        // The concurrent session's checkpoint must carry a loadable body, so
        // seed it through the real manager instead of a placeholder file.
        let inflight_checkpoint = verification_manager
            .save_checkpoint(&inflight)
            .expect("seed the concurrent session's checkpoint");
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::CompletedCommit { session: committed });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");
        assert!(report.failures.is_empty(), "{:?}", report.failures);

        assert!(
            !committed_checkpoint.exists(),
            "the committed session's own checkpoint must be cleared"
        );
        let survivor = verification_manager
            .load_session_checkpoint(&inflight_id)
            .expect("load the concurrent session's checkpoint")
            .expect("another session's checkpoint must never be cleared");
        assert_eq!(survivor.metadata.id, inflight_id);
        assert!(inflight_checkpoint.exists());
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn shutdown_preserves_inflight_checkpoint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir.clone()).expect("verify");
        let session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let checkpoint_path = seed_checkpoint_file(&sessions_dir, &session_id);
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SaveCheckpoint { session });
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        assert!(
            checkpoint_path.exists(),
            "shutdown must never unconditionally clear an in-flight checkpoint"
        );
        let recovered = verification_manager
            .load_session_checkpoint(&session_id)
            .expect("load checkpoint after shutdown")
            .expect("in-flight work must survive shutdown for recovery review");
        assert_eq!(recovered.metadata.id, session_id);
    }

    #[tokio::test]
    async fn newer_inflight_checkpoint_supersedes_pending_completed_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let mut completed = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        completed.metadata.title = "Completed turn".to_string();
        let mut inflight = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        // The new turn reuses the same session id: a fresh in-flight
        // checkpoint arrives while the previous completion is still queued.
        inflight.metadata.id = completed.metadata.id.clone();
        inflight.metadata.title = "Next turn in progress".to_string();
        let session_id = completed.metadata.id.clone();
        let verification_manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::CompletedCommit { session: completed });
        handle.try_send(PersistRequest::SaveCheckpoint { session: inflight });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");

        assert!(
            !report
                .failures
                .iter()
                .any(|(what, _)| what.starts_with("clear-checkpoint:")),
            "the newer in-flight checkpoint must not be cleared, got: {:?}",
            report.failures
        );
        let checkpoint = std::fs::read_to_string(
            sessions_dir
                .join("checkpoints")
                .join(format!("{session_id}.json")),
        )
        .expect("the newer checkpoint must survive the drain");
        assert!(
            !checkpoint.is_empty(),
            "the newer checkpoint body must be on disk"
        );
        let saved = verification_manager
            .load_session(&session_id)
            .expect("completed turn document must survive coalescing");
        assert_eq!(saved.metadata.title, "Completed turn");
        let recovery = verification_manager
            .load_session_checkpoint(&session_id)
            .expect("read in-flight checkpoint")
            .expect("in-flight checkpoint must survive");
        assert_eq!(recovery.metadata.title, "Next turn in progress");
        assert!(
            report.completed >= 2,
            "both the completed document and newer checkpoint must be counted, got: {report:?}"
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }
    #[test]
    fn offline_queue_editor_lease_survives_until_pending_write_finishes() {
        let directory = tempfile::tempdir().expect("queue fixture");
        let manager = SessionManager::new(directory.path().join("sessions")).expect("manager");
        let lease = manager
            .acquire_offline_queue_lease("session-A")
            .expect("first editor");
        let mut pending = PendingState::default();
        pending.absorb(PersistRequest::OfflineQueue {
            state: OfflineQueueState {
                draft: Some(QueuedSessionMessage {
                    display: "last edited draft".into(),
                    skill_instruction: None,
                    skill_provenance: None,
                }),
                ..OfflineQueueState::default()
            },
            lease: Arc::clone(&lease),
        });
        drop(lease); // The old window changed session before the actor ran.
        assert!(manager.acquire_offline_queue_lease("session-A").is_err());
        let report = flush_inner(&manager, &mut pending, &SessionSaveHealth::default());
        assert!(report.failures.is_empty(), "draft write failed: {report:?}");
        assert_eq!(report.completed, 1);
        let _next_editor = manager
            .acquire_offline_queue_lease("session-A")
            .expect("released after write");
        assert_eq!(
            manager
                .load_offline_queue_state("session-A")
                .unwrap()
                .unwrap()
                .draft
                .unwrap()
                .display,
            "last edited draft"
        );
    }
}

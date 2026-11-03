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
//! - **One dedicated tokio task** orders owned blocking batches. The UI only sends requests;
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
//!   consumer retains one barrier and one successor per session instead of one per send.
//!   Queued snapshots retain only the canonical journal; legacy `messages`
//!   are derived at the disk boundary instead of doubling every paused
//!   request.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use codewhale_engine::session_manager::{
    OfflineQueueLease, OfflineQueueState, SavedSession, SessionManager, SessionPublication,
    SessionPublicationKind as PublicationKind,
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
        reply: oneshot::Sender<Result<SessionPublication, String>>,
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

impl PersistRequest {
    fn receipt(&self) -> Option<codewhale_engine::session_manager::SessionWriteReceipt> {
        match self {
            Self::SaveCheckpoint { session }
            | Self::AcknowledgedCheckpoint { session, .. }
            | Self::SessionSnapshot(session)
            | Self::CompletedCommit { session } => session.write_receipt.clone(),
            _ => None,
        }
    }
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
/// time, so a paused consumer retains one ordered barrier and one latest successor per session
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
        for (_, slot) in std::mem::take(&mut guard.pending.publications) {
            for publication in slot.barrier.into_iter().chain(slot.latest) {
                publication.refuse("persistence actor shut down before publication");
            }
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
    /// newest compatible successor. Returns `false` when the actor is closed
    /// or the native predecessor cannot be retained in order.
    pub fn try_send(&self, mut request: PersistRequest) -> bool {
        let receipt = request.receipt();
        if self.tx.cmd_tx.is_closed() {
            if let Some(receipt) = receipt {
                receipt.refuse("persistence actor is closed");
            }
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
                if let Some(receipt) = receipt {
                    receipt.refuse("persistence actor is closed");
                }
                return false;
            }
            guard.pending.absorb(request)
        };
        match control {
            Control::Refused => false,
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
                match self.tx.cmd_tx.try_send(ActorCommand::WorkReady) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        let mut guard = self
                            .tx
                            .shared
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.notified = false;
                        if let Some(receipt) = receipt {
                            receipt.refuse("persistence actor is closed");
                        }
                        false
                    }
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
    let receipt = request.receipt();
    if try_persist(request) {
        return Ok(());
    }
    if let Some(error) = receipt.and_then(|receipt| receipt.observation().err()) {
        return Err(error.to_string());
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
        Err("session persistence actor is not initialized".to_string())
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
    match ACTOR_TX.get() {
        Some(handle) => handle.try_send(request),
        None => {
            if let Some(receipt) = request.receipt() {
                receipt.refuse("session persistence actor is not initialized");
            }
            false
        }
    }
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
    let manager = Arc::new(manager);

    let task = spawn_supervised(
        "persistence-actor",
        std::panic::Location::caller(),
        async move {
            let mut unreported = FlushReport::default();

            async fn flush_cycle(
                manager: &Arc<SessionManager>,
                mut pending: PendingState,
                unreported: &mut FlushReport,
                health: &SessionSaveHealth,
            ) {
                let session_ids: Vec<_> = pending
                    .publications
                    .iter()
                    .filter(|(_, slot)| {
                        slot.barrier
                            .iter()
                            .chain(slot.latest.iter())
                            .any(|publication| publication.kind != PublicationKind::Checkpoint)
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                let receipts: Vec<_> = pending
                    .publications
                    .values()
                    .flat_map(|slot| slot.barrier.iter().chain(slot.latest.iter()))
                    .filter_map(|publication| publication.session.write_receipt.clone())
                    .collect();
                let manager = manager.clone();
                let worker_health = health.clone();
                let cycle = codewhale_runtime::runtime_control::owner_work(move || {
                    Ok(flush_inner(&manager, &mut pending, &worker_health))
                })
                .await;
                match cycle {
                    Ok(cycle) => unreported.merge(cycle),
                    Err(error) => {
                        for receipt in receipts {
                            receipt.refuse(format!("owned persistence batch failed: {error}"));
                        }
                        tracing::warn!(target: "persistence", %error, "owned persistence batch failed");
                        for session_id in session_ids {
                            health.record::<()>(
                                &session_id,
                                &Err(std::io::Error::other(error.to_string())),
                            );
                        }
                        unreported.merge(FlushReport {
                            completed: 0,
                            failures: vec![(
                                "persistence-worker".to_string(),
                                std::io::ErrorKind::Other,
                            )],
                        });
                    }
                }
            }

            // Work is coalesced at send time into the shared pending state;
            // every command handler takes whatever has accumulated and
            // flushes it outside the sender lock.
            while let Some(command) = rx.recv().await {
                let pending = rx.take_pending();
                match command {
                    ActorCommand::WorkReady => {
                        if !pending.is_empty() {
                            flush_cycle(&manager, pending, &mut unreported, &rx.health).await;
                        }
                    }
                    ActorCommand::FlushAndReport { reply } => {
                        flush_cycle(&manager, pending, &mut unreported, &rx.health).await;
                        let _ = reply.send(std::mem::take(&mut unreported));
                    }
                    ActorCommand::Shutdown => {
                        flush_cycle(&manager, pending, &mut unreported, &rx.health).await;
                        return;
                    }
                }
            }
            // Every sender is gone — final flush and exit. Each absorb
            // guarantees a queued WorkReady, so this is normally empty.
            let pending = rx.take_pending();
            flush_cycle(&manager, pending, &mut unreported, &rx.health).await;
        },
    );

    (handle, task)
}

#[derive(Debug)]
struct PendingPublication {
    kind: PublicationKind,
    session: SavedSession,
    reply: Option<oneshot::Sender<Result<SessionPublication, String>>>,
}

impl PendingPublication {
    fn refuse(self, error: &str) {
        if let Some(receipt) = self.session.write_receipt.as_ref() {
            receipt.refuse(error);
        }
        if let Some(reply) = self.reply {
            let _ = reply.send(Err(error.into()));
        }
        tracing::warn!(session_id = %self.session.metadata.id, %error, "session publication refused before queueing");
    }
}

#[derive(Debug, Default)]
struct PendingSlot {
    barrier: Option<PendingPublication>,
    latest: Option<PendingPublication>,
}

#[derive(Debug, Default)]
struct PendingState {
    publications: BTreeMap<String, PendingSlot>,
    offline_queue: BTreeMap<String, PendingOfflineQueue>,
}

enum Control {
    Continue,
    Refused,
    Flush(oneshot::Sender<FlushReport>),
    Shutdown,
}

impl PendingState {
    fn is_empty(&self) -> bool {
        self.publications.is_empty() && self.offline_queue.is_empty()
    }

    fn absorb_publication(&mut self, publication: PendingPublication) -> Control {
        let id = publication.session.metadata.id.clone();
        if publication
            .session
            .write_receipt
            .as_ref()
            .and_then(|receipt| receipt.owner())
            .is_none()
        {
            publication.refuse("session publication lacks a captured native producer");
            return Control::Refused;
        }
        let slot = self.publications.entry(id).or_default();
        if slot.latest.is_none()
            && let Some(barrier) = slot.barrier.as_ref()
            && !publication
                .session
                .write_receipt
                .as_ref()
                .zip(barrier.session.write_receipt.as_ref())
                .is_some_and(|(current, prior)| current.is_successor_of(prior))
        {
            publication
                .refuse("session publication is not the captured successor of its pending barrier");
            return Control::Refused;
        }
        if let Some(previous) = slot.latest.take() {
            if previous.reply.is_some() {
                slot.latest = Some(previous);
                publication.refuse("an acknowledged checkpoint must publish before its successor");
                return Control::Refused;
            }
            let receipts = publication
                .session
                .write_receipt
                .as_ref()
                .zip(previous.session.write_receipt.as_ref());
            if !receipts
                .as_ref()
                .is_some_and(|(current, prior)| current.is_successor_of(prior))
            {
                slot.latest = Some(previous);
                publication
                    .refuse("session publication is not the captured successor of pending work");
                return Control::Refused;
            }
            if previous.kind == PublicationKind::Completion {
                if slot.barrier.is_some() {
                    slot.latest = Some(previous);
                    publication.refuse(
                        "completion publication is waiting on its checkpoint acknowledgement",
                    );
                    return Control::Refused;
                }
                slot.barrier = Some(previous);
            } else {
                let same_execution = receipts.as_ref().is_some_and(|(current, prior)| {
                    current
                        .owner()
                        .zip(prior.owner())
                        .is_some_and(|(current, prior)| current.same_execution(&prior))
                });
                let history_follows = same_execution
                    || match (
                        publication.session.journal.as_ref(),
                        previous.session.journal.as_ref(),
                    ) {
                        (Some(current), Some(prior)) => {
                            let current_index = current.index();
                            let retained_graph = prior.entries.iter().all(|entry| {
                                current_index
                                    .get(entry.id.as_str())
                                    .is_some_and(|current| **current == *entry)
                            });
                            let current_path = current.root_to_leaf();
                            let prior_path = prior.root_to_leaf();
                            retained_graph
                                && current_path.len() >= prior_path.len()
                                && prior_path
                                    .iter()
                                    .zip(current_path.iter())
                                    .all(|(prior, current)| prior == current)
                        }
                        (None, None) => publication
                            .session
                            .messages
                            .starts_with(&previous.session.messages),
                        _ => false,
                    };
                let explicit_control = publication.session.write_receipt.as_ref().and_then(|receipt| receipt.owner())
                    .is_some_and(|owner| matches!(owner, codewhale_engine::session_manager::SessionWriteOwner::Control(_))
                        || matches!(owner, codewhale_engine::session_manager::SessionWriteOwner::Engine(source) if source.compaction_id.is_some()));
                if !same_execution && !history_follows {
                    if explicit_control && slot.barrier.is_none() {
                        slot.barrier = Some(previous);
                        slot.latest = Some(publication);
                        return Control::Continue;
                    }
                    slot.latest = Some(previous);
                    publication
                        .refuse("pending publication history is incompatible with its successor");
                    return Control::Refused;
                }
                if !receipts
                    .is_some_and(|(current, prior)| current.coalesce_after(prior, previous.kind))
                {
                    slot.latest = Some(previous);
                    publication
                        .refuse("pending publication could not preserve its captured predecessor");
                    return Control::Refused;
                }
                tracing::debug!(session_id = %publication.session.metadata.id, previous = ?previous.kind, current = ?publication.kind, "coalesced native publication without acknowledging the predecessor");
            }
        }
        if publication.reply.is_some() && slot.barrier.is_none() {
            slot.barrier = Some(publication);
        } else {
            slot.latest = Some(publication);
        }
        Control::Continue
    }

    fn absorb(&mut self, req: PersistRequest) -> Control {
        match req {
            PersistRequest::AcknowledgedCheckpoint { session, reply } => {
                if let Some(receipt) = session.write_receipt.as_ref() {
                    receipt.require_checkpoint_acknowledgement();
                }
                self.absorb_publication(PendingPublication {
                    kind: PublicationKind::Checkpoint,
                    session,
                    reply: Some(reply),
                })
            }
            PersistRequest::SaveCheckpoint { session } => {
                self.absorb_publication(PendingPublication {
                    kind: PublicationKind::Checkpoint,
                    session,
                    reply: None,
                })
            }
            PersistRequest::SessionSnapshot(session) => {
                self.absorb_publication(PendingPublication {
                    kind: PublicationKind::Snapshot,
                    session,
                    reply: None,
                })
            }
            PersistRequest::CompletedCommit { session } => {
                self.absorb_publication(PendingPublication {
                    kind: PublicationKind::Completion,
                    session,
                    reply: None,
                })
            }
            PersistRequest::OfflineQueue { state, lease } => {
                self.offline_queue.insert(
                    lease.session_id().to_string(),
                    PendingOfflineQueue::Save {
                        state: Box::new(state),
                        lease,
                    },
                );
                Control::Continue
            }
            PersistRequest::ClearOfflineQueue { lease } => {
                self.offline_queue.insert(
                    lease.session_id().to_string(),
                    PendingOfflineQueue::Clear { lease },
                );
                Control::Continue
            }
            PersistRequest::FlushAndReport { reply } => Control::Flush(reply),
            PersistRequest::Shutdown => Control::Shutdown,
        }
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

    for (session_id, slot) in std::mem::take(&mut pending.publications) {
        for publication in slot.barrier.into_iter().chain(slot.latest) {
            let PendingPublication {
                kind,
                session,
                reply,
            } = publication;
            if reply.as_ref().is_some_and(oneshot::Sender::is_closed) {
                if let Some(receipt) = session.write_receipt.as_ref() {
                    receipt.refuse("checkpoint acknowledgement was withdrawn before publication");
                }
                continue;
            }
            match kind {
                PublicationKind::Checkpoint => {
                    let result = manager.publish_checkpoint_owned(session);
                    if let Some(reply) = reply {
                        let _ = reply.send(result.as_ref().cloned().map_err(ToString::to_string));
                    }
                    record(format!("checkpoint:{session_id}"), result.map(|_| ()));
                }
                PublicationKind::Snapshot => {
                    let result = manager.publish_session_bounded(session);
                    health.record(&session_id, &result);
                    record(format!("session:{session_id}"), result.map(|_| ()));
                }
                PublicationKind::Completion => {
                    let result = manager.publish_completed_session(session);
                    health.record(&session_id, &result);
                    match result {
                        Ok((_, clear)) => {
                            record(format!("completed-commit:{session_id}"), Ok(()));
                            record(format!("clear-checkpoint:{session_id}"), clear);
                        }
                        Err(error) => record(format!("completed-commit:{session_id}"), Err(error)),
                    }
                }
            }
        }
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
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
        session.write_receipt = Some(manager.expect_absent_session(&session_id).unwrap());
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .unwrap();
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
        first.write_receipt = Some(
            manager
                .expect_absent_session(&first.metadata.id)
                .expect("native fresh source"),
        );
        first
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        first.metadata.title = "Session A".to_string();
        let mut second = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        second.write_receipt = Some(
            manager
                .expect_absent_session(&second.metadata.id)
                .expect("native fresh source"),
        );
        second
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        for completion in [false, true] {
            let mut pending = PendingState::default();
            let mut checkpoint = session.clone();
            checkpoint.metadata.id = uuid::Uuid::new_v4().to_string();
            checkpoint.write_receipt = Some(
                manager
                    .expect_absent_session(&checkpoint.metadata.id)
                    .unwrap(),
            );
            checkpoint
                .capture_publication(
                    codewhale_engine::session_manager::SessionWriteOwner::control(),
                )
                .unwrap();
            let mut successor = checkpoint.clone();
            successor
                .capture_publication(
                    codewhale_engine::session_manager::SessionWriteOwner::control(),
                )
                .unwrap();
            let (reply, mut receiver) = oneshot::channel();
            pending.absorb(PersistRequest::AcknowledgedCheckpoint {
                session: checkpoint,
                reply,
            });
            pending.absorb(if completion {
                PersistRequest::CompletedCommit { session: successor }
            } else {
                PersistRequest::SaveCheckpoint { session: successor }
            });
            assert!(matches!(
                receiver.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            let report = flush_inner(&manager, &mut pending, &SessionSaveHealth::default());
            assert!(report.failures.is_empty(), "{report:?}");

            assert!(receiver.await.expect("real publication reply").is_ok());
        }
        let mut snapshot = session.clone();
        snapshot
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .unwrap();
        let (handle, task) = spawn_persistence_actor(manager);
        let (checkpoint_reply, checkpoint_result) = oneshot::channel();
        assert!(handle.try_send(PersistRequest::AcknowledgedCheckpoint {
            session: session.clone(),
            reply: checkpoint_reply,
        }));
        assert!(handle.try_send(PersistRequest::SessionSnapshot(snapshot)));
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
        let checkpoint_blocker = sessions_dir.join("checkpoints");
        std::fs::write(&checkpoint_blocker, b"not a directory").expect("block checkpoints dir");
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
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
        handle.try_send(PersistRequest::SessionSnapshot(session.clone()));
        flush(&handle).await.expect("flush");
        assert_eq!(
            handle
                .session_save_health()
                .failing
                .as_ref()
                .map(|(id, _)| id.as_str()),
            Some(session_id.as_str())
        );
        std::fs::remove_file(&checkpoint_blocker).expect("unblock checkpoint inspection");
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
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

    /// Pre-write a crash-recovery checkpoint through its real writer so
    /// completion-commit tests can assert on valid recovery evidence without
    /// needing a prior in-flight turn.
    fn seed_checkpoint_file(
        sessions_dir: &std::path::Path,
        session_id: &str,
    ) -> SessionPublication {
        let manager = SessionManager::new(sessions_dir.to_path_buf()).expect("checkpoint manager");
        let mut session = codewhale_engine::session_manager::create_saved_session_with_id_and_mode(
            session_id.to_string(),
            &[],
            "deepseek-v4-pro",
            sessions_dir.parent().expect("fixture workspace"),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(manager.expect_absent_session(session_id).unwrap());
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .unwrap();
        manager
            .publish_checkpoint_owned(session)
            .expect("seed checkpoint file")
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        let session_id = session.metadata.id.clone();
        let checkpoint_publication = seed_checkpoint_file(&sessions_dir, &session_id);
        let checkpoint_path = checkpoint_publication.path.clone();
        session.write_receipt = Some(checkpoint_publication.write_receipt());
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("checkpoint successor");
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        let session_id = session.metadata.id.clone();
        let checkpoint_publication = seed_checkpoint_file(&sessions_dir, &session_id);
        let checkpoint_path = checkpoint_publication.path.clone();
        session.write_receipt = Some(checkpoint_publication.write_receipt());
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("checkpoint successor");
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
        let mut committed = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        committed.write_receipt = Some(
            manager
                .expect_absent_session(&committed.metadata.id)
                .expect("native fresh source"),
        );
        committed
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        let mut inflight = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        inflight.write_receipt = Some(
            manager
                .expect_absent_session(&inflight.metadata.id)
                .expect("native fresh source"),
        );
        inflight
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        let committed_id = committed.metadata.id.clone();
        let inflight_id = inflight.metadata.id.clone();
        let checkpoint_publication = seed_checkpoint_file(&sessions_dir, &committed_id);
        let committed_checkpoint = checkpoint_publication.path.clone();
        committed.write_receipt = Some(checkpoint_publication.write_receipt());
        committed
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("checkpoint successor");
        // The concurrent session's checkpoint must carry a loadable body too.
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
        let mut session = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        session.write_receipt = Some(
            manager
                .expect_absent_session(&session.metadata.id)
                .expect("native fresh source"),
        );
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        let session_id = session.metadata.id.clone();
        let checkpoint_publication = seed_checkpoint_file(&sessions_dir, &session_id);
        let checkpoint_path = checkpoint_publication.path.clone();
        session.write_receipt = Some(checkpoint_publication.write_receipt());
        session
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("checkpoint successor");
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
        completed.write_receipt = Some(
            manager
                .expect_absent_session(&completed.metadata.id)
                .expect("native fresh source"),
        );
        completed
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        completed.metadata.title = "Completed turn".to_string();
        completed.metadata.total_tokens = 7;
        let mut inflight = codewhale_engine::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        inflight.write_receipt = Some(
            manager
                .expect_absent_session(&inflight.metadata.id)
                .expect("native fresh source"),
        );
        inflight
            .capture_publication(codewhale_engine::session_manager::SessionWriteOwner::control())
            .expect("native producer");
        // The new turn reuses the same session id: a fresh in-flight
        // checkpoint arrives while the previous completion is still queued.
        inflight.metadata.id = completed.metadata.id.clone();
        inflight.metadata.title = "Next turn in progress".to_string();
        inflight.metadata.total_tokens = 11;
        inflight.write_receipt = completed.write_receipt.clone();
        inflight
            .capture_publication(
                codewhale_engine::session_manager::SessionWriteOwner::Submission(
                    uuid::Uuid::new_v4().to_string(),
                ),
            )
            .expect("next native submission");
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
        assert_eq!(saved.metadata.total_tokens, 7);
        let recovery = verification_manager
            .load_session_checkpoint(&session_id)
            .expect("read in-flight checkpoint")
            .expect("in-flight checkpoint must survive");
        assert_eq!(recovery.metadata.title, "Completed turn");
        assert_eq!(recovery.metadata.total_tokens, 11);
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

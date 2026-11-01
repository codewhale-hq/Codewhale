//! Turn-phase heartbeat and stall self-report (#6184).
//!
//! A turn that stops producing output used to leave no trace: no log line,
//! nothing in `crashes/`, and a UI that could not tell a quiet model wait from
//! a wedged engine. The engine now publishes *where* a turn is (its phase), a
//! monotonic last-progress stamp, and the bound the current phase may stay
//! silent for. A watchdog task, independent of the turn future, turns an
//! overdue bounded phase into a log line, a stall record under `crashes/`, and
//! a status event naming the phase.
//!
//! Phases that are owned by their own bound elsewhere — a tool batch (per-tool
//! timeouts plus the UI tool-hang watchdog), a compaction pass, a human
//! approval — are declared *parked* (`bound = None`) and never reported here.
//!
//! The heartbeat is shared with the UI through `EngineHandle`, so the UI's own
//! watchdog reads the engine's liveness directly instead of inferring it from
//! the stream-chunk timeout.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codewhale_runtime::stall_report::{StallReport, report_stall};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::core::events::Event;

/// How often the watchdog samples the heartbeat.
pub const STALL_WATCHDOG_TICK: Duration = Duration::from_secs(5);
/// Bound for the engine's own between-request work (context assembly, hooks,
/// MCP refresh, post-stream bookkeeping). None of it waits on a provider, so a
/// few minutes of silence here is a wedge, not a slow model.
pub const PREPARING_PHASE_BOUND: Duration = Duration::from_secs(180);
/// Ceiling for a tool batch, including everything the engine does after a tool
/// returns and before it parks the batch. The phase used to carry no bound at
/// all ("per-tool timeouts own it"), which left exactly that post-tool window
/// owned by nobody: a turn wedged there produced no stall record, no status
/// line and no cancellation for 56 minutes, and the UI watchdog declined to
/// fire because the engine still looked like the owner. A generous ceiling is
/// enough — a tool that outlives its own timeout is already an error, and a
/// legitimate long build stays far below this.
pub const TOOLS_PHASE_BOUND: Duration = Duration::from_secs(1800);
/// Grace added on top of a wait's own timeout. The inner timeout should fire
/// first; the heartbeat only reports when that timeout itself failed to.
pub const STALL_BOUND_GRACE: Duration = Duration::from_secs(30);

/// Where the active turn currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    Idle,
    /// Engine-local work between provider requests.
    Preparing,
    /// Request sent; waiting for the stream to open and produce its first event.
    AwaitingModel,
    /// Stream open; waiting on the next event.
    Streaming,
    /// Planning or executing a tool batch (parked: per-tool bounds own it).
    Tools,
    /// Automatic compaction pass (parked: the pass owns its bound).
    Compacting,
}

impl TurnPhase {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing the next request",
            Self::AwaitingModel => "waiting for the model's first response",
            Self::Streaming => "streaming the model response",
            Self::Tools => "running tools",
            Self::Compacting => "compacting context",
        }
    }
}

impl fmt::Display for TurnPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
struct HeartbeatState {
    phase: TurnPhase,
    detail: Option<String>,
    bound: Option<Duration>,
    last_progress: Instant,
    turn_id: Option<String>,
    provider_request: Option<String>,
    /// Bumped on every phase change or progress touch; a stall is reported at
    /// most once per value.
    progress_seq: u64,
    reported_seq: Option<u64>,
    /// Latest report, cleared by the next progress.
    stall: Option<StallReport>,
}

/// Point-in-time view for the UI watchdog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatSnapshot {
    pub phase: TurnPhase,
    pub since_progress: Duration,
    pub bound: Option<Duration>,
    pub stall: Option<StallReport>,
}

impl HeartbeatSnapshot {
    /// The engine is inside a wait it bounds itself and has not reported as
    /// overdue. The UI must not second-guess it. Parked phases (tools,
    /// compaction) stay under the UI's own tool-hang and turn watchdogs.
    #[must_use]
    pub fn engine_owns_live_wait(&self) -> bool {
        self.phase != TurnPhase::Idle && self.bound.is_some() && self.stall.is_none()
    }
}

/// Shared turn-phase heartbeat. Cheap to update from the turn loop.
#[derive(Debug)]
pub struct TurnHeartbeat {
    state: Mutex<HeartbeatState>,
}

impl Default for TurnHeartbeat {
    fn default() -> Self {
        Self {
            state: Mutex::new(HeartbeatState {
                phase: TurnPhase::Idle,
                detail: None,
                bound: None,
                last_progress: Instant::now(),
                turn_id: None,
                provider_request: None,
                progress_seq: 0,
                reported_seq: None,
                stall: None,
            }),
        }
    }
}

impl TurnHeartbeat {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut HeartbeatState) -> R) -> R {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    /// A new turn starts in [`TurnPhase::Preparing`].
    pub fn begin_turn(&self, turn_id: &str) {
        self.with_state(|state| {
            state.turn_id = Some(turn_id.to_string());
            state.provider_request = None;
        });
        self.enter(TurnPhase::Preparing, None, Some(PREPARING_PHASE_BOUND));
    }

    /// Enter `phase`. `bound = None` declares a parked wait that this watchdog
    /// never reports.
    pub fn enter(&self, phase: TurnPhase, detail: Option<String>, bound: Option<Duration>) {
        self.with_state(|state| {
            state.phase = phase;
            state.detail = detail;
            state.bound = bound;
            state.last_progress = Instant::now();
            state.progress_seq = state.progress_seq.wrapping_add(1);
            state.stall = None;
        });
    }

    /// Record progress inside the current phase.
    pub fn touch(&self) {
        self.with_state(|state| {
            state.last_progress = Instant::now();
            state.progress_seq = state.progress_seq.wrapping_add(1);
            state.stall = None;
        });
    }

    /// Stream progress: the first event of a request moves the phase from
    /// awaiting-model to streaming with the inter-chunk bound; later events
    /// only touch.
    pub fn stream_progress(&self, streaming_bound: Duration) {
        let entering = self.with_state(|state| state.phase != TurnPhase::Streaming);
        if entering {
            let detail = self.with_state(|state| state.detail.clone());
            self.enter(TurnPhase::Streaming, detail, Some(streaming_bound));
        } else {
            self.touch();
        }
    }

    /// Remember the provider's id for the in-flight response.
    pub fn set_provider_request(&self, id: impl Into<String>) {
        let id = id.into();
        if id.is_empty() {
            return;
        }
        self.with_state(|state| state.provider_request = Some(id));
    }

    pub fn idle(&self) {
        self.enter(TurnPhase::Idle, None, None);
        self.with_state(|state| state.turn_id = None);
    }

    #[must_use]
    pub fn snapshot_at(&self, now: Instant) -> HeartbeatSnapshot {
        self.with_state(|state| HeartbeatSnapshot {
            phase: state.phase,
            since_progress: now.saturating_duration_since(state.last_progress),
            bound: state.bound,
            stall: state.stall.clone(),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> HeartbeatSnapshot {
        self.snapshot_at(Instant::now())
    }

    /// Return a report the first time the current bounded phase is overdue.
    pub fn detect_stall_at(&self, now: Instant) -> Option<StallReport> {
        self.with_state(|state| {
            if state.phase == TurnPhase::Idle || state.reported_seq == Some(state.progress_seq) {
                return None;
            }
            let bound = state.bound?;
            let since_progress = now.saturating_duration_since(state.last_progress);
            if since_progress <= bound {
                return None;
            }
            state.reported_seq = Some(state.progress_seq);
            let report = StallReport {
                source: "engine",
                phase: format!("while {}", state.phase.label()),
                detail: state.detail.clone(),
                turn_id: state.turn_id.clone(),
                provider_request: state.provider_request.clone(),
                since_progress,
                bound: Some(bound),
            };
            state.stall = Some(report.clone());
            Some(report)
        })
    }
}

/// Aborts the wrapped task when dropped (the watchdog must not outlive the
/// engine that owns its heartbeat).
pub struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Supervise `heartbeat` until the event channel closes: every overdue bounded
/// phase yields one log line, one stall record, and one status event.
pub fn spawn_turn_stall_watchdog(
    heartbeat: Arc<TurnHeartbeat>,
    tx_event: mpsc::Sender<Event>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_turn_stall_watchdog_every(heartbeat, tx_event, cancel, STALL_WATCHDOG_TICK)
}

fn spawn_turn_stall_watchdog_every(
    heartbeat: Arc<TurnHeartbeat>,
    tx_event: mpsc::Sender<Event>,
    cancel: CancellationToken,
    tick: Duration,
) -> tokio::task::JoinHandle<()> {
    #[cfg(any(
        test,
        feature = "test-support",
        all(feature = "test-thread-scoped-state", debug_assertions)
    ))]
    let test_dir = codewhale_runtime::stall_report::stall_record_dir();
    tokio::spawn(async move {
        #[cfg(any(
            test,
            feature = "test-support",
            all(feature = "test-thread-scoped-state", debug_assertions)
        ))]
        codewhale_runtime::stall_report::set_test_stall_record_dir(test_dir);
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if tx_event.is_closed() {
                break;
            }
            if let Some(report) = heartbeat.detect_stall_at(Instant::now()) {
                let record = report_stall(&report);
                let mut line = report.status_line();
                if let Some(path) = record {
                    line.push_str(&format!(" Stall record: {}", path.display()));
                }
                // Never block the watchdog on a full mailbox: a wedged
                // consumer is exactly the case it exists to survive.
                let _ = tx_event.try_send(Event::status(line));
                // Reporting alone left a wedged turn running forever, which is
                // the failure this watchdog exists to prevent: the status line
                // scrolls and the turn still never ends. Cancelling turns the
                // wedge into a finished turn with a stated reason.
                cancel.cancel();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::stall_report::set_test_stall_record_dir;

    fn later(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    #[test]
    fn stall_bounded_phase_reports_once_per_episode() {
        let heartbeat = TurnHeartbeat::new();
        heartbeat.begin_turn("turn_1");
        heartbeat.enter(
            TurnPhase::AwaitingModel,
            Some("deepseek/deepseek-v4-pro".into()),
            Some(Duration::from_secs(60)),
        );
        assert!(heartbeat.detect_stall_at(Instant::now()).is_none());
        let report = heartbeat
            .detect_stall_at(later(61))
            .expect("overdue bounded phase must report");
        assert_eq!(report.turn_id.as_deref(), Some("turn_1"));
        assert!(report.phase.contains("first response"), "{}", report.phase);
        assert!(
            heartbeat.detect_stall_at(later(120)).is_none(),
            "once per episode"
        );
        assert!(!heartbeat.snapshot().engine_owns_live_wait());
        heartbeat.touch();
        assert!(
            heartbeat.snapshot().engine_owns_live_wait(),
            "progress clears the stall"
        );
    }

    #[test]
    fn stall_parked_phase_is_never_reported() {
        let heartbeat = TurnHeartbeat::new();
        heartbeat.begin_turn("turn_1");
        heartbeat.enter(TurnPhase::Tools, Some("exec_shell".into()), None);
        assert!(heartbeat.detect_stall_at(later(24 * 60 * 60)).is_none());
        assert!(
            !heartbeat.snapshot().engine_owns_live_wait(),
            "parked phases stay under the UI's own watchdogs"
        );
        heartbeat.idle();
        assert!(heartbeat.detect_stall_at(later(24 * 60 * 60)).is_none());
    }

    #[test]
    fn stall_first_stream_event_switches_to_inter_chunk_bound() {
        let heartbeat = TurnHeartbeat::new();
        heartbeat.begin_turn("turn_1");
        heartbeat.enter(
            TurnPhase::AwaitingModel,
            None,
            Some(Duration::from_secs(10)),
        );
        heartbeat.stream_progress(Duration::from_secs(100));
        let snapshot = heartbeat.snapshot();
        assert_eq!(snapshot.phase, TurnPhase::Streaming);
        assert_eq!(snapshot.bound, Some(Duration::from_secs(100)));
        assert!(heartbeat.detect_stall_at(later(50)).is_none());
        assert!(heartbeat.detect_stall_at(later(101)).is_some());
    }

    /// Fault injection: an inter-chunk wait that never ends produces a log
    /// line, a `crashes/` stall record, and a status event within the bound
    /// plus one watchdog tick.
    #[tokio::test]
    async fn stall_watchdog_writes_record_and_status_within_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        set_test_stall_record_dir(Some(dir.path().to_path_buf()));
        let heartbeat = TurnHeartbeat::new();
        heartbeat.begin_turn("turn_wedged");
        let bound = Duration::from_millis(200);
        let tick = Duration::from_millis(20);
        heartbeat.enter(TurnPhase::Streaming, Some("mock/model".into()), Some(bound));
        heartbeat.set_provider_request("resp_123");
        let (tx, mut rx) = mpsc::channel(4);
        let started = std::time::Instant::now();
        let cancel = CancellationToken::new();
        let watchdog =
            spawn_turn_stall_watchdog_every(Arc::clone(&heartbeat), tx, cancel.clone(), tick);

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("stall status")
            .expect("event");
        let elapsed = started.elapsed();
        assert!(elapsed >= bound, "not before the bound: {elapsed:?}");
        let Event::Status { message } = event else {
            panic!("expected status event");
        };
        assert!(
            message.contains("Turn stalled while streaming"),
            "{message}"
        );
        assert!(message.contains("Esc to cancel and retry"), "{message}");
        assert!(
            cancel.is_cancelled(),
            "a reported stall must end the turn, not only describe it"
        );
        assert!(message.contains("Stall record:"), "{message}");

        let records: Vec<_> = std::fs::read_dir(dir.path())
            .expect("record dir")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("record"))
            .collect();
        assert_eq!(records.len(), 1, "exactly one record per stall episode");
        assert!(records[0].contains("Kind: turn-stall"));
        assert!(records[0].contains("Turn: turn_wedged"));
        assert!(records[0].contains("Provider request: resp_123"));
        watchdog.abort();
        set_test_stall_record_dir(None);
    }
}

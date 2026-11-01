//! Terminal input ingestion and fairness controls for the TUI event loop.

use std::cell::Cell;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};

const TERMINAL_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TERMINAL_INPUT_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const TERMINAL_INPUT_IDLE_AFTER: Duration = Duration::from_secs(5);
const TERMINAL_INPUT_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);
pub(super) const TERMINAL_INPUT_STALL_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const TERMINAL_INPUT_RECOVERY_COOLDOWN: Duration = Duration::from_secs(10);
const TERMINAL_INPUT_CHILD_PAUSE_TIMEOUT: Duration = Duration::from_millis(500);
const TERMINAL_INPUT_CHILD_PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Upper bound on engine events processed before yielding to terminal input.
pub(super) const MAX_ENGINE_EVENTS_PER_DRAIN: usize = 16;
/// Wall-clock budget for one engine drain batch (#1830 / #2317 input fairness).
pub(super) const ENGINE_DRAIN_TIME_BUDGET: Duration = Duration::from_millis(8);

pub(super) enum TerminalInputMessage {
    Event(ObservedTerminalEvent),
    Heartbeat,
    Error(io::Error),
}

/// A terminal event paired with the instant the dedicated input thread read it.
///
/// The event loop can lag behind this thread under load. Keeping receipt time
/// prevents that backlog from collapsing a deliberate pause between a raw
/// paste and Enter into a paste-speed sequence.
pub(crate) struct ObservedTerminalEvent {
    pub(crate) event: Event,
    pub(crate) observed_at: Instant,
}

impl ObservedTerminalEvent {
    pub(crate) fn new(event: Event, observed_at: Instant) -> Self {
        Self { event, observed_at }
    }
}

/// Process-wide handle on the one terminal input pump's pause flags.
///
/// There is one stdin per process and exactly one [`TerminalInputPump`]
/// reading it, so this is a singleton by construction rather than by
/// convention. It exists so that *handing the terminal to a child* can be one
/// operation instead of a rule every call site has to remember (#6165):
/// suspending raw mode and the alternate screen does not stop the pump
/// thread, which keeps calling `event::read()` on the same tty and splits the
/// user's keystrokes between the child and the composer.
///
/// Known limitation: the gate only stops the pump reading. It cannot drain
/// input the pump already buffered, and it cannot refuse the handoff when a
/// cancellation key is pending — both need the receiver and the event loop's
/// pending queue, so they stay with [`super::prepare_terminal_input_handoff`]
/// at the call sites that have them.
static CHILD_TERMINAL_GATE: Mutex<Option<ChildTerminalGate>> = Mutex::new(None);

#[derive(Clone)]
struct ChildTerminalGate {
    paused: Arc<AtomicBool>,
    paused_ack: Arc<AtomicBool>,
}

/// Publish this pump as the process's terminal input owner.
pub(super) fn publish_child_terminal_gate(paused: &Arc<AtomicBool>, paused_ack: &Arc<AtomicBool>) {
    if let Ok(mut gate) = CHILD_TERMINAL_GATE.lock() {
        *gate = Some(ChildTerminalGate {
            paused: Arc::clone(paused),
            paused_ack: Arc::clone(paused_ack),
        });
    }
}

/// Retract `paused`'s pump, but only if it is still the published one — a
/// detached wedged thread must not unpublish the replacement that took over.
fn retract_child_terminal_gate(paused: &Arc<AtomicBool>) {
    if let Ok(mut gate) = CHILD_TERMINAL_GATE.lock()
        && gate
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.paused, paused))
    {
        *gate = None;
    }
}

/// The terminal input pump, paused for as long as this guard is alive.
///
/// Held by [`crate::tui::external_editor::with_suspended_tui`] across the
/// whole child handoff, so the pump resumes on every path out — including a
/// child that failed to spawn or a panic unwinding through it.
pub(crate) struct ChildTerminalInputPause {
    gate: Option<ChildTerminalGate>,
}

/// Stop the process's terminal input pump before a child takes the tty.
///
/// Fails closed: if the pump does not acknowledge the pause, the caller must
/// not run the child, because that is exactly the keystroke-splitting state
/// this guards against. A process with no pump published (tests, non-TUI
/// callers) has nothing to pause and succeeds with an inert guard, matching
/// [`TerminalInputPump::pause_for_child_terminal`]'s `handle.is_none()` case.
pub(crate) fn pause_terminal_input_for_child() -> io::Result<ChildTerminalInputPause> {
    let Some(gate) = CHILD_TERMINAL_GATE
        .lock()
        .ok()
        .and_then(|gate| gate.clone())
    else {
        return Ok(ChildTerminalInputPause { gate: None });
    };
    gate.paused.store(true, Ordering::Release);
    let deadline = Instant::now() + TERMINAL_INPUT_CHILD_PAUSE_TIMEOUT;
    while !gate.paused_ack.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            gate.paused_ack.store(false, Ordering::Release);
            gate.paused.store(false, Ordering::Release);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal input pump did not pause before child terminal handoff",
            ));
        }
        // Blocking-call convention (#6149): a bounded retry, capped by
        // `TERMINAL_INPUT_CHILD_PAUSE_TIMEOUT`, in a synchronous API whose
        // caller is about to block this very thread on a foreground editor
        // for as long as the user keeps it open. `tokio::time` is not
        // reachable from here and would not change what the thread does.
        thread::sleep(TERMINAL_INPUT_CHILD_PAUSE_POLL_INTERVAL);
    }
    Ok(ChildTerminalInputPause { gate: Some(gate) })
}

impl Drop for ChildTerminalInputPause {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.paused_ack.store(false, Ordering::Release);
            gate.paused.store(false, Ordering::Release);
        }
    }
}

pub(crate) struct TerminalInputPump {
    pub(super) rx: std::sync::mpsc::Receiver<TerminalInputMessage>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) paused: Arc<AtomicBool>,
    pub(super) paused_ack: Arc<AtomicBool>,
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) last_alive_at: Cell<Instant>,
}

pub(super) struct TerminalInputPumpParts {
    pub(super) rx: std::sync::mpsc::Receiver<TerminalInputMessage>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) paused: Arc<AtomicBool>,
    pub(super) paused_ack: Arc<AtomicBool>,
    pub(super) handle: JoinHandle<()>,
}

impl TerminalInputPump {
    pub(super) fn spawn() -> io::Result<Self> {
        let parts = Self::spawn_parts()?;
        publish_child_terminal_gate(&parts.paused, &parts.paused_ack);
        Ok(Self {
            rx: parts.rx,
            stop: parts.stop,
            paused: parts.paused,
            paused_ack: parts.paused_ack,
            handle: Some(parts.handle),
            last_alive_at: Cell::new(Instant::now()),
        })
    }

    fn spawn_parts() -> io::Result<TerminalInputPumpParts> {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let paused_ack = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_paused = Arc::clone(&paused);
        let thread_paused_ack = Arc::clone(&paused_ack);
        let handle = thread::Builder::new()
            .name("codewhale-terminal-input".to_string())
            .spawn(move || {
                let mut last_heartbeat = Instant::now();
                let mut last_event = last_heartbeat;
                while !thread_stop.load(Ordering::Acquire) {
                    if thread_paused.load(Ordering::Acquire) {
                        thread_paused_ack.store(true, Ordering::Release);
                        thread::sleep(TERMINAL_INPUT_CHILD_PAUSE_POLL_INTERVAL);
                        continue;
                    }
                    thread_paused_ack.store(false, Ordering::Release);
                    let poll_interval = if last_event.elapsed() < TERMINAL_INPUT_IDLE_AFTER {
                        TERMINAL_INPUT_POLL_INTERVAL
                    } else {
                        TERMINAL_INPUT_IDLE_POLL_INTERVAL
                    };
                    match event::poll(poll_interval) {
                        Ok(true) if thread_stop.load(Ordering::Acquire) => break,
                        Ok(true) => match event::read() {
                            Ok(event) => {
                                last_heartbeat = Instant::now();
                                last_event = last_heartbeat;
                                let observed = ObservedTerminalEvent::new(event, last_heartbeat);
                                if tx.send(TerminalInputMessage::Event(observed)).is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                let _ = tx.send(TerminalInputMessage::Error(err));
                                break;
                            }
                        },
                        Ok(false) => {
                            let now = Instant::now();
                            if now.duration_since(last_heartbeat)
                                >= TERMINAL_INPUT_HEARTBEAT_INTERVAL
                            {
                                last_heartbeat = now;
                                if tx.send(TerminalInputMessage::Heartbeat).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(TerminalInputMessage::Error(err));
                            break;
                        }
                    }
                }
            })?;
        Ok(TerminalInputPumpParts {
            rx,
            stop,
            paused,
            paused_ack,
            handle,
        })
    }

    pub(super) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> io::Result<Option<ObservedTerminalEvent>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(TerminalInputMessage::Event(event)) => {
                    self.mark_alive();
                    return Ok(Some(event));
                }
                Ok(TerminalInputMessage::Heartbeat) => {
                    self.mark_alive();
                    if remaining.is_zero() {
                        return Ok(None);
                    }
                }
                Ok(TerminalInputMessage::Error(err)) => {
                    self.mark_alive();
                    return Err(err);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "terminal input pump disconnected",
                    ));
                }
            }
        }
    }

    pub(super) fn try_recv(&self) -> io::Result<Option<ObservedTerminalEvent>> {
        loop {
            match self.rx.try_recv() {
                Ok(TerminalInputMessage::Event(event)) => {
                    self.mark_alive();
                    return Ok(Some(event));
                }
                Ok(TerminalInputMessage::Heartbeat) => {
                    self.mark_alive();
                }
                Ok(TerminalInputMessage::Error(err)) => {
                    self.mark_alive();
                    return Err(err);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(None),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(None),
            }
        }
    }

    pub(super) fn mark_alive(&self) {
        self.last_alive_at.set(Instant::now());
    }

    pub(super) fn stalled_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_alive_at.get())
    }

    /// Async: callers are on the event-loop task, so the ack wait uses
    /// `tokio::time::sleep` — a `thread::sleep` here would park a Tokio
    /// worker for up to `TERMINAL_INPUT_CHILD_PAUSE_TIMEOUT`.
    pub(super) async fn pause_for_child_terminal(&self) -> io::Result<()> {
        self.paused.store(true, Ordering::Release);
        if self.handle.is_none() {
            self.paused_ack.store(true, Ordering::Release);
            self.mark_alive();
            return Ok(());
        }

        let deadline = Instant::now() + TERMINAL_INPUT_CHILD_PAUSE_TIMEOUT;
        while !self.paused_ack.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                self.paused_ack.store(false, Ordering::Release);
                self.paused.store(false, Ordering::Release);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminal input pump did not pause before child terminal handoff",
                ));
            }
            tokio::time::sleep(TERMINAL_INPUT_CHILD_PAUSE_POLL_INTERVAL).await;
        }
        self.mark_alive();
        Ok(())
    }

    pub(super) fn resume_after_child_terminal(&self) {
        self.paused_ack.store(false, Ordering::Release);
        self.paused.store(false, Ordering::Release);
        self.mark_alive();
    }

    /// Replace a wedged pump thread with a freshly spawned one.
    ///
    /// The old thread may be blocked forever inside crossterm's blocking
    /// `event::read` (a stalled Windows console poll, or a Unix tty that
    /// stopped delivering bytes), so it can never be joined. Instead it is
    /// detached: `stop` is flagged and the `JoinHandle` dropped, so if the
    /// thread ever wakes it exits on its own (its send fails once `rx` is
    /// replaced, and the stop flag covers the poll loop).
    ///
    /// Known limitation (U03-10): stop is flagged before the replacement is
    /// spawned, but a thread already inside `event::read` cannot be stopped.
    /// If it ever wakes, the one event it read has no receiver and is lost.
    pub(super) fn restart_detached(&mut self) -> io::Result<()> {
        self.detach_current_thread();
        let parts = Self::spawn_parts()?;
        self.install_parts(parts);
        Ok(())
    }

    /// Flag the current pump thread to stop and drop its handle without
    /// joining (the thread may be wedged in a blocking terminal read).
    pub(super) fn detach_current_thread(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.handle.take();
        retract_child_terminal_gate(&self.paused);
    }

    /// Adopt freshly spawned pump parts and reset the liveness clock.
    pub(super) fn install_parts(&mut self, parts: TerminalInputPumpParts) {
        publish_child_terminal_gate(&parts.paused, &parts.paused_ack);
        self.rx = parts.rx;
        self.stop = parts.stop;
        self.paused = parts.paused;
        self.paused_ack = parts.paused_ack;
        self.handle = Some(parts.handle);
        self.last_alive_at.set(Instant::now());
    }
}

impl Drop for TerminalInputPump {
    fn drop(&mut self) {
        // `event::read` can remain blocked forever after a tty disconnect on
        // every supported desktop platform. Joining here would turn an input
        // failure into an application shutdown hang. Flag the cooperative
        // stop and detach; if the read ever wakes, the loop observes `stop`
        // (or its send fails because `rx` was dropped) and exits on its own.
        self.stop.store(true, Ordering::Release);
        let _ = self.handle.take();
        retract_child_terminal_gate(&self.paused);
    }
}

pub(super) fn engine_drain_budget_exhausted(
    events_drained: usize,
    started: Instant,
    now: Instant,
) -> bool {
    events_drained >= MAX_ENGINE_EVENTS_PER_DRAIN
        || now.saturating_duration_since(started) >= ENGINE_DRAIN_TIME_BUDGET
}

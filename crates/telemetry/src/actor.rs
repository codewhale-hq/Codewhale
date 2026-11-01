//! The background writer.
//!
//! One dedicated OS thread behind a bounded `mpsc`. `record()` is a
//! non-blocking `try_send` and a hard no-op when the process is not armed;
//! everything the thread does is wrapped in `catch_unwind`, so a panic inside
//! telemetry costs telemetry and nothing else.
//!
//! A plain thread rather than a `tokio` task, deliberately: `init` is called
//! from six subcommand dispatch points, several of which have no runtime yet,
//! and a telemetry subsystem that only works when someone remembered to be
//! inside an executor is a subsystem that silently collects nothing on half its
//! surfaces.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use crate::buffer;
use crate::client::{self, SendOutcome};
use crate::decision::{self, TelemetryDecision};
use crate::envelope;
use crate::event::{Batch, Event, SCHEMA_VERSION, Surface};

/// Events per batch.
pub const BATCH_MAX_EVENTS: usize = 200;
/// Byte ceiling per batch body.
pub const BATCH_MAX_BYTES: usize = 64 * 1024;

pub(crate) enum Message {
    Event(Box<Event>),
    PersistLocal(SyncSender<FlushOutcome>),
    Shutdown(SyncSender<FlushOutcome>),
}

/// What a flush attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Nothing was buffered.
    Empty,
    /// Events remain in the local buffer for a later network flush.
    Buffered,
    /// A batch was written to the dry-run sink.
    DryRun,
    /// A batch was accepted by the endpoint.
    Sent,
    /// A batch was assembled and dropped — offline, refused, or contended.
    Dropped,
    /// Telemetry was off by the time the flush ran; nothing was sent.
    Suppressed,
    /// The actor did not answer inside the caller's deadline.
    TimedOut,
}

/// Facts the writer thread needs, fixed at arming time.
#[derive(Debug, Clone)]
pub(crate) struct Context {
    pub root: PathBuf,
    pub endpoint: Option<String>,
    pub surface: Surface,
    pub config_path: Option<PathBuf>,
    pub app_version: String,
    pub git_sha: Option<String>,
    pub tty: bool,
}

/// A handle to the writer thread.
#[derive(Debug)]
pub(crate) struct Handle {
    tx: SyncSender<Message>,
    pending: Arc<AtomicUsize>,
    closing: AtomicBool,
}

impl Handle {
    /// Start the writer thread.
    pub(crate) fn spawn(context: Context) -> Self {
        let (tx, rx) = sync_channel::<Message>(buffer::MAX_EVENTS + 1);
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending.clone();
        // A detached thread: nothing joins it, and the process exiting while it
        // is mid-write is exactly the case the torn-line tolerance covers.
        let _ = std::thread::Builder::new()
            .name("codewhale-telemetry".to_string())
            .spawn(move || run(&context, &rx, &worker_pending));
        Self {
            tx,
            pending,
            closing: AtomicBool::new(false),
        }
    }

    /// Queue an event without blocking; drop it when full, closing or closed.
    pub(crate) fn record(&self, event: Event) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let mut pending = self.pending.load(Ordering::Acquire);
        loop {
            if pending >= buffer::MAX_EVENTS {
                return;
            }
            match self.pending.compare_exchange_weak(
                pending,
                pending + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => pending = current,
            }
        }
        if self.closing.load(Ordering::Acquire)
            || self.tx.try_send(Message::Event(Box::new(event))).is_err()
        {
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Ask for a final flush and stop the thread.
    pub(crate) fn shutdown(&self, deadline: Duration) -> FlushOutcome {
        self.round_trip(deadline, Message::Shutdown)
    }

    /// Persist queued events locally and stop without making a network request.
    ///
    /// Unlike [`Handle::shutdown`], this waits without a deadline: the local
    /// path performs only bounded disk work, and racing it against a clock is
    /// what lost dry-run receipts on slow hosts (#6269). The writer always
    /// acknowledges — even on panic — so the only way out without an outcome
    /// is a writer that is already gone, which fails open as `TimedOut`.
    pub(crate) fn persist_local(&self) -> FlushOutcome {
        if self.closing.swap(true, Ordering::AcqRel) {
            return FlushOutcome::TimedOut;
        }
        let (ack_tx, ack_rx) = sync_channel::<FlushOutcome>(1);
        if self.tx.try_send(Message::PersistLocal(ack_tx)).is_err() {
            return FlushOutcome::TimedOut;
        }
        ack_rx.recv().unwrap_or(FlushOutcome::TimedOut)
    }

    fn round_trip(
        &self,
        deadline: Duration,
        build: impl FnOnce(SyncSender<FlushOutcome>) -> Message,
    ) -> FlushOutcome {
        let started = Instant::now();
        if self.closing.swap(true, Ordering::AcqRel) {
            return FlushOutcome::TimedOut;
        }
        let (ack_tx, ack_rx) = sync_channel::<FlushOutcome>(1);
        if self.tx.try_send(build(ack_tx)).is_err() {
            return FlushOutcome::TimedOut;
        }
        match ack_rx.recv_timeout(deadline.saturating_sub(started.elapsed())) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                FlushOutcome::TimedOut
            }
        }
    }
}

fn run(context: &Context, rx: &Receiver<Message>, pending: &AtomicUsize) {
    while let Ok(message) = rx.recv() {
        // Telemetry never takes the process with it. The hook has already been
        // installed by the time this thread exists, so a panic here is caught,
        // dropped, and the loop continues.
        let result = catch_unwind(AssertUnwindSafe(|| match message {
            Message::Event(event) => {
                pending.fetch_sub(1, Ordering::AcqRel);
                append(context, &event);
                None
            }
            Message::PersistLocal(ack) => {
                // The caller joins this without a deadline (#6269), so the
                // acknowledgement must be unconditional: a panic here would
                // otherwise hang a short CLI command at exit. The outer
                // `catch_unwind` still guards the other arms.
                let outcome = match catch_unwind(AssertUnwindSafe(|| persist_local(context))) {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        tracing::debug!("telemetry local persist recovered from a panic");
                        FlushOutcome::Dropped
                    }
                };
                let _ = ack.send(outcome);
                Some(())
            }
            Message::Shutdown(ack) => {
                let _ = ack.send(flush(context));
                Some(())
            }
        }));
        match result {
            Ok(Some(())) => return,
            Ok(None) => {}
            Err(_) => {
                tracing::debug!("telemetry writer recovered from a panic");
            }
        }
    }
}

fn append(context: &Context, event: &Event) {
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    let path = buffer::buffer_path(&context.root);
    let _ = buffer::append(&context.root, &path, &line);
}

/// Seal every event queued before this message without reaching the network.
///
/// The channel is FIFO, so all prior event appends have settled before this
/// runs. Re-deciding here preserves the same mid-session opt-out boundary as a
/// full flush. An explicitly empty endpoint is the local dry-run sink and can
/// therefore be finalized immediately; a configured endpoint leaves the
/// events in `buffer.jsonl` for the next interactive flush.
fn persist_local(context: &Context) -> FlushOutcome {
    match decision::re_decide(context.config_path.as_deref(), context.surface) {
        TelemetryDecision::Enabled(_) => {}
        TelemetryDecision::OptedOut | TelemetryDecision::ForcedOff => {
            return FlushOutcome::Suppressed;
        }
    }
    if buffer::tombstone_present(&context.root) {
        return FlushOutcome::Suppressed;
    }
    if context.endpoint.is_none() {
        return flush(context);
    }
    if buffer::read_lines(&buffer::buffer_path(&context.root)).is_empty() {
        FlushOutcome::Empty
    } else {
        FlushOutcome::Buffered
    }
}

/// Drain, re-check consent, and deliver.
///
/// The re-check is the point: telemetry is resolved once at init, but the
/// documented mid-session opt-out is an external file write this process would
/// otherwise never observe. If the answer is now `OptedOut`, `decide` has
/// already wiped and left the tombstone, and the drained events go nowhere.
fn flush(context: &Context) -> FlushOutcome {
    match decision::re_decide(context.config_path.as_deref(), context.surface) {
        TelemetryDecision::Enabled(_) => {}
        TelemetryDecision::OptedOut | TelemetryDecision::ForcedOff => {
            return FlushOutcome::Suppressed;
        }
    }
    if buffer::tombstone_present(&context.root) {
        return FlushOutcome::Suppressed;
    }

    let lines = buffer::drain(&context.root);
    if lines.is_empty() {
        return FlushOutcome::Empty;
    }
    let events = parse_events(&lines);
    if events.is_empty() {
        return FlushOutcome::Empty;
    }

    let Ok(install) = envelope::read_or_create_install_id(&context.root) else {
        return FlushOutcome::Dropped;
    };

    let mut state = envelope::read_state(&context.root);
    state.schema_version = SCHEMA_VERSION;
    state.last_flush = Some(envelope::now_rfc3339());
    // Written on attempt, not on success, so a permanently offline machine
    // attempts at most once per interval rather than on every launch.
    let _ = envelope::write_state(&context.root, &state);

    let batch = Batch {
        schema_version: SCHEMA_VERSION,
        notice_version: crate::event::NOTICE_VERSION,
        sent_at: envelope::now_rfc3339(),
        install_id: install.install_id,
        app_version: context.app_version.clone(),
        git_sha: context.git_sha.clone(),
        surface: context.surface,
        os: envelope::current_os(),
        arch: envelope::current_arch(),
        libc: envelope::current_libc(),
        tty: context.tty,
        events,
    };

    match client::send(&context.root, context.endpoint.as_deref(), &batch) {
        SendOutcome::DryRun => FlushOutcome::DryRun,
        SendOutcome::Accepted => FlushOutcome::Sent,
        SendOutcome::Dropped => FlushOutcome::Dropped,
    }
}

/// Parse drained lines into events, capped at [`BATCH_MAX_EVENTS`] and
/// [`BATCH_MAX_BYTES`], skipping anything that does not parse **or does not
/// satisfy its declared string bounds**.
///
/// The bound re-check is the point. Everything upstream of here builds events
/// from closed enums, `u32`s, and two reducers — but this function is a
/// deserializer, and its input is a file on disk that any process running as
/// the user can append to. `Event::is_bounded` is what stops
/// `{"event":"panic","site":"<anything at all>"}` from becoming a first-party
/// POST under the user's install id.
pub(crate) fn parse_events(lines: &[String]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut bytes = 0usize;
    for line in lines {
        if events.len() >= BATCH_MAX_EVENTS || bytes + line.len() > BATCH_MAX_BYTES {
            break;
        }
        if let Ok(event) = serde_json::from_str::<Event>(line) {
            if !event.is_bounded() {
                tracing::debug!("telemetry dropped an out-of-bounds buffered event");
                continue;
            }
            bytes += line.len();
            events.push(event);
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_persistence_joins_the_writer_without_a_deadline() {
        // The caller blocks on a mock writer that answers on its own
        // schedule: no sleep, no timing assertion, no race with the clock.
        let (tx, rx) = sync_channel(buffer::MAX_EVENTS + 1);
        let handle = Handle {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
            closing: AtomicBool::new(false),
        };
        handle.record(Event::SessionStart {
            source: crate::SessionSource::Unknown,
        });
        let waiter = std::thread::spawn(move || handle.persist_local());
        assert!(matches!(rx.try_recv(), Ok(Message::Event(_))));
        let Message::PersistLocal(ack) = rx.recv().expect("local persistence request") else {
            panic!("short CLI persistence must not request a network shutdown flush");
        };
        ack.send(FlushOutcome::Buffered)
            .expect("the caller is still joined");
        assert_eq!(waiter.join().expect("waiter"), FlushOutcome::Buffered);
        assert!(
            rx.try_recv().is_err(),
            "one persist request enqueues exactly one message"
        );
    }

    #[test]
    fn local_persistence_fails_open_when_the_writer_is_gone() {
        // The writer thread exits after a persist or shutdown request; a
        // second caller must fail open, not hang on a dead receiver. Send
        // fails deterministically here, so no timing assertion is needed.
        let (tx, rx) = sync_channel::<Message>(buffer::MAX_EVENTS + 1);
        drop(rx);
        let handle = Handle {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
            closing: AtomicBool::new(false),
        };
        assert_eq!(handle.persist_local(), FlushOutcome::TimedOut);
    }
}

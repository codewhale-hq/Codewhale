//! Lifecycle event outbox: a local JSONL log of session/turn/subagent
//! lifecycle events plus an optional webhook fan-out.
//!
//! This is the machine-readable sibling of the TUI shell-hook system. Hooks
//! fire shell commands per event and are TUI-only; the outbox appends one
//! JSON line per event to a config-gated file and needs no per-event
//! configuration. It is additive and opt-in: with no path configured,
//! [`LifecycleOutbox::emit`] is a no-op.
//!
//! # Line schema
//!
//! Every line is a `codewhale_app_server_protocol::runtime::RuntimeEventEnvelope`:
//!
//! ```json
//! {"schema_version": 1, "seq": 3, "event": "turn_start", "kind": "turn.started",
//!  "thread_id": "…", "turn_id": "…", "item_id": null, "timestamp": "…",
//!  "created_at": "…", "payload": {…}}
//! ```
//!
//! - `seq` is monotonic per outbox file. Each batched append acquires an
//!   exclusive advisory file lock, recovers the `seq` of the file's last
//!   complete line (bounded tail scan, so an outbox that grows unbounded is
//!   never re-read in full), assigns `last + 1..`, and fsyncs before unlocking.
//! - `event` is the snake-case lifecycle name (`turn_start`, `turn_end`, …);
//!   `kind` is the dotted kind (`turn.started`, `turn.failed`, …).
//! - Payloads are constructed by the emit sites from bounded, pre-redacted
//!   fields only — never raw tool arguments, environment, or full transcript
//!   text. [`bounded_text`] enforces the same ceilings as the desktop
//!   notification payloads: headline ≤ 80, detail ≤ 120, preview ≤ 200
//!   characters.
//!
//! # Delivery model
//!
//! [`LifecycleOutbox::emit`] never blocks the caller: it enqueues the event
//! on an internal channel and a single writer task appends lines in order.
//! If no tokio runtime is available the event is dropped with a warning.
//! Webhook POSTs (`{"at": …, "event": …}`) are attempted after the local
//! append and fsync, carrying a stable `Idempotency-Key` header derived from
//! the outbox identity and `event.seq` so receivers can deduplicate retries.
//! Failures are logged and never retried into the agent loop; [`LifecycleOutbox::flush`]
//! waits for queued events to finish their delivery attempt and reports any
//! durable append or webhook failure.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use codewhale_app_server_protocol::runtime::{
    RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION, RuntimeEventEnvelope,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;

use crate::WebhookHookSink;

/// Text-length ceilings for outbox payload fields. Mirrors the desktop
/// notification payload limits so the outbox never carries more than the
/// lock-screen-capable surface already does.
pub const OUTBOX_HEADLINE_MAX_CHARS: usize = 80;
pub const OUTBOX_DETAIL_MAX_CHARS: usize = 120;
pub const OUTBOX_PREVIEW_MAX_CHARS: usize = 200;

/// Suffix appended when [`bounded_text`] truncates a field.
pub const OUTBOX_TRUNCATION_MARKER: &str = "…";

/// How far back from EOF the seq-recovery scan reads. Outbox lines are
/// bounded (payload ceilings above plus envelope overhead), so a line can
/// never approach this window and the last complete line is always inside it.
const SEQ_RECOVERY_TAIL_BYTES: u64 = 64 * 1024;

/// Queue capacity from emit sites to the writer (#6212). The outbox is
/// observability, not control flow: when a consumer is wedged longer than
/// this backlog, further events are dropped with a warning instead of
/// retaining unbounded snapshots in memory. Flush commands are never dropped
/// silently — a full queue fails the flush fast rather than timing it out.
const OUTBOX_QUEUE_CAPACITY: usize = 1024;

/// Events gathered per writer drain before one batched append. Batching only
/// affects how many queued events share a single file open/write/flush; each
/// line still lands as its own complete JSONL record.
const OUTBOX_MAX_BATCH: usize = 64;

/// Concurrent webhook posts the writer allows in flight. A stalled webhook
/// delays only its own event (plus the flush barrier, which the caller's
/// timeout bounds) — never the audit-log appends of later events.
const WEBHOOK_MAX_INFLIGHT: usize = 8;

/// One lifecycle event destined for the outbox.
///
/// Construct one per emit site. `payload` must only contain bounded,
/// pre-redacted fields; apply [`bounded_text`] to anything free-form (error
/// messages, previews) before inserting it.
#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    /// Snake-case event name, e.g. `"turn_start"`.
    pub event: String,
    /// Dotted event kind, e.g. `"turn.started"` or `"turn.failed"`.
    pub kind: String,
    /// Owning session/thread id. Empty when the producer has none.
    pub thread_id: String,
    /// Current turn id, when known.
    pub turn_id: Option<String>,
    /// Current item id, when known.
    pub item_id: Option<String>,
    /// Bounded, redacted event payload.
    pub payload: Value,
}

/// The lifecycle outbox handle.
///
/// Cheap to clone (an `Arc`). When constructed without a path the outbox is
/// disabled and every `emit` is a no-op.
#[derive(Clone)]
pub struct LifecycleOutbox {
    inner: Option<Arc<OutboxInner>>,
}

impl Default for LifecycleOutbox {
    fn default() -> Self {
        Self::disabled()
    }
}

impl LifecycleOutbox {
    /// Create an outbox writing to `path` when set and non-empty.
    ///
    /// `webhook_url` optionally adds a webhook fan-out (POST `{"at", "event"}`,
    /// best-effort); `webhook_token` is its optional bearer token. Webhook
    /// delivery is configured independently of the file: it only ever runs
    /// when `webhook_url` is set, and it never replaces the local append.
    pub fn new(
        path: Option<PathBuf>,
        webhook_url: Option<String>,
        webhook_token: Option<String>,
    ) -> Self {
        let path = match path {
            Some(path) if !path.as_os_str().is_empty() => crate::expand_home(path),
            _ => return Self::disabled(),
        };
        let webhook = webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| WebhookHookSink::new_with_token(url.to_string(), webhook_token));
        let (sender, receiver) = tokio::sync::mpsc::channel(OUTBOX_QUEUE_CAPACITY);
        Self {
            inner: Some(Arc::new(OutboxInner {
                path,
                webhook,
                sender,
                receiver: Mutex::new(Some(receiver)),
                writer_spawned: AtomicBool::new(false),
                spawn_lock: Mutex::new(()),
            })),
        }
    }

    /// A disabled outbox that drops every event.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// True when a path was configured and events will be written.
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Emit one lifecycle event.
    ///
    /// Never blocks: the event is queued for the outbox's writer task (spawned
    /// lazily on the current tokio runtime on first use). Events queued with
    /// no runtime available — or after the writer task is gone — are dropped
    /// with a warning. Delivery failures inside the writer are logged and
    /// dropped as well; the outbox is observability, not control flow.
    pub fn emit(&self, event: LifecycleEvent) {
        let Some(inner) = self.inner.clone() else {
            return;
        };
        if let Err(error) = inner.enqueue(OutboxCommand::Event(event)) {
            tracing::warn!(target: "lifecycle_outbox", %error, "lifecycle event dropped");
        }
    }

    /// Wait for events queued before this call to finish their delivery attempt.
    ///
    /// Local writes are flushed and synced before the writer acknowledges this
    /// barrier, and any append or webhook delivery failure since the previous
    /// barrier is returned to the caller. A stalled writer/webhook or a stopped
    /// runtime returns an error within `timeout` instead of holding process
    /// teardown indefinitely. This does not close other cloned handles.
    pub async fn flush(&self, timeout: Duration) -> Result<()> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(());
        };
        let (reply, completed) = oneshot::channel();
        inner.enqueue(OutboxCommand::Flush(reply))?;
        let outcome = tokio::time::timeout(timeout, completed)
            .await
            .context("lifecycle outbox flush timed out; queued events may be incomplete")?
            .context("lifecycle outbox writer stopped before flush completed")?;
        outcome.map_err(anyhow::Error::msg)
    }
}

enum OutboxCommand {
    Event(LifecycleEvent),
    Flush(oneshot::Sender<std::result::Result<(), String>>),
}

struct OutboxInner {
    path: PathBuf,
    webhook: Option<WebhookHookSink>,
    sender: Sender<OutboxCommand>,
    /// The writer task's receive half. Taken exactly once by the writer task.
    receiver: Mutex<Option<Receiver<OutboxCommand>>>,
    writer_spawned: AtomicBool,
    /// Serializes the lazy writer-task spawn so two racing first emits cannot
    /// start two writers.
    spawn_lock: Mutex<()>,
}

impl OutboxInner {
    /// Queue an event and make sure the writer task exists to drain it.
    ///
    /// Ordering: `send` happens before the spawn so events queued before the
    /// writer starts are drained first, preserving enqueue order. The queue
    /// is bounded: a wedged consumer drops further events with a warning
    /// (observability, not control flow) but fails a flush fast instead of
    /// dropping its reply channel.
    fn enqueue(self: &Arc<Self>, command: OutboxCommand) -> Result<()> {
        let is_flush = matches!(command, OutboxCommand::Flush(_));
        match self.sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) if !is_flush => {
                tracing::warn!(
                    target: "lifecycle_outbox",
                    queue_capacity = OUTBOX_QUEUE_CAPACITY,
                    "lifecycle event dropped: outbox queue is full"
                );
            }
            Err(TrySendError::Full(_)) => {
                anyhow::bail!("lifecycle outbox queue is full; flush rejected");
            }
            Err(TrySendError::Closed(_)) => {
                anyhow::bail!("lifecycle outbox writer task is gone");
            }
        }
        self.ensure_writer_spawned();
        Ok(())
    }

    fn ensure_writer_spawned(self: &Arc<Self>) {
        if self.writer_spawned.load(Ordering::Acquire) {
            return;
        }
        let _guard = self
            .spawn_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.writer_spawned.load(Ordering::Acquire) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                target: "lifecycle_outbox",
                "no tokio runtime available; lifecycle events are queued but will not be written"
            );
            return;
        };
        let receiver = self
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(receiver) = receiver else {
            return;
        };
        let mut state = WriterState::new(self.path.clone(), self.webhook.clone(), receiver);
        self.writer_spawned.store(true, Ordering::Release);
        handle.spawn(async move {
            state.run().await;
        });
    }
}

/// The outbox writer: owns the file state and the event queue drain loop.
struct WriterState {
    path: PathBuf,
    sink_id: String,
    webhook: Option<WebhookHookSink>,
    receiver: Receiver<OutboxCommand>,
}

fn outbox_sink_id(path: &Path) -> String {
    format!(
        "outbox-{:016x}",
        crate::fnv1a_64(path.to_string_lossy().as_bytes())
    )
}

fn record_webhook_join(
    joined: std::result::Result<std::result::Result<(), String>, tokio::task::JoinError>,
    pending_error: &mut Option<String>,
) {
    match joined {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            pending_error.get_or_insert(error);
        }
        Err(error) => {
            pending_error.get_or_insert_with(|| format!("lifecycle webhook task failed: {error}"));
        }
    }
}

impl WriterState {
    fn new(
        path: PathBuf,
        webhook: Option<WebhookHookSink>,
        receiver: Receiver<OutboxCommand>,
    ) -> Self {
        let sink_id = outbox_sink_id(&path);
        Self {
            path,
            sink_id,
            webhook,
            receiver,
        }
    }

    /// Drain the queue until every sender is dropped, then exit.
    ///
    /// Events are gathered into batches ([`OUTBOX_MAX_BATCH`]) so a burst
    /// shares one file open/lock/write/sync. Webhook posts fan out through a
    /// bounded [`JoinSet`](tokio::task::JoinSet) and never delay the appends
    /// of later batches — a stalled webhook only holds back its own event
    /// plus the flush barrier, which the caller's timeout bounds (#6212).
    async fn run(&mut self) {
        let mut batch: Vec<OutboxCommand> = Vec::with_capacity(OUTBOX_MAX_BATCH);
        let mut webhooks: tokio::task::JoinSet<std::result::Result<(), String>> =
            tokio::task::JoinSet::new();
        let mut pending_error: Option<String> = None;
        while self.receiver.recv_many(&mut batch, OUTBOX_MAX_BATCH).await > 0 {
            let mut events = Vec::new();
            let mut replies = Vec::new();
            for command in batch.drain(..) {
                match command {
                    OutboxCommand::Event(event) => events.push(event),
                    OutboxCommand::Flush(reply) => replies.push(reply),
                }
            }
            if let Err(error) = self
                .deliver_batch(&events, &mut webhooks, &mut pending_error)
                .await
            {
                tracing::warn!(
                    target: "lifecycle_outbox",
                    %error,
                    path = %self.path.display(),
                    "lifecycle outbox write failed"
                );
                pending_error.get_or_insert_with(|| format!("{error:#}"));
            }
            if !replies.is_empty() {
                while let Some(joined) = webhooks.join_next().await {
                    record_webhook_join(joined, &mut pending_error);
                }
                let outcome = match pending_error.take() {
                    Some(error) => Err(error),
                    None => Ok(()),
                };
                for reply in replies {
                    let _ = reply.send(outcome.clone());
                }
            }
        }
    }

    /// Lock the outbox file, recover the latest seq on disk, append and sync
    /// the whole batch, then fan webhook posts out concurrently.
    async fn deliver_batch(
        &mut self,
        events: &[LifecycleEvent],
        webhooks: &mut tokio::task::JoinSet<std::result::Result<(), String>>,
        pending_error: &mut Option<String>,
    ) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let file = crate::open_private_append(&self.path)
            .await?
            .into_std()
            .await;
        let path = self.path.clone();
        let batch_events = events.to_vec();
        let collect_webhooks = self.webhook.is_some();

        let webhook_posts = tokio::task::spawn_blocking(move || -> Result<Vec<(u64, Value)>> {
            let mut file = file;
            file.lock()
                .with_context(|| format!("failed to lock outbox {}", path.display()))?;
            let result = (|| -> Result<Vec<(u64, Value)>> {
                let mut next_seq = recover_last_seq_from_file(&mut file, &path)?;
                let mut lines = Vec::with_capacity(batch_events.len());
                let mut posts = Vec::new();
                for event in batch_events {
                    let seq = next_seq;
                    next_seq = next_seq.saturating_add(1);
                    let envelope = RuntimeEventEnvelope {
                        schema_version: RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,
                        seq,
                        event: event.event,
                        kind: event.kind,
                        thread_id: event.thread_id,
                        turn_id: event.turn_id,
                        item_id: event.item_id,
                        timestamp: Utc::now().to_rfc3339(),
                        created_at: Some(Utc::now().to_rfc3339()),
                        payload: event.payload,
                        extra: Default::default(),
                    };
                    if collect_webhooks {
                        posts.push((
                            seq,
                            json!({
                                "at": envelope.timestamp,
                                "event": envelope,
                            }),
                        ));
                    }
                    lines.push(
                        serde_json::to_string(&envelope)
                            .context("failed to encode outbox event")?,
                    );
                }
                append_lines(&mut file, &lines)?;
                Ok(posts)
            })();
            let _ = file.unlock();
            result
        })
        .await
        .context("lifecycle outbox writer task panicked")??;

        if let Some(webhook) = &self.webhook {
            for (seq, payload) in webhook_posts {
                while webhooks.len() >= WEBHOOK_MAX_INFLIGHT {
                    if let Some(joined) = webhooks.join_next().await {
                        record_webhook_join(joined, pending_error);
                    }
                }
                let sink = webhook.clone();
                let idempotency_key = format!("{}:{seq}", self.sink_id);
                webhooks.spawn(async move {
                    if let Err(error) = sink
                        .post_payload_with_idempotency_key(payload, &idempotency_key)
                        .await
                    {
                        tracing::warn!(
                            target: "lifecycle_outbox",
                            %error,
                            "lifecycle webhook delivery failed (dropped)"
                        );
                        return Err(format!("{error:#}"));
                    }
                    Ok(())
                });
            }
        }

        Ok(())
    }
}

fn append_lines(file: &mut std::fs::File, lines: &[String]) -> Result<()> {
    file.seek(std::io::SeekFrom::End(0))
        .context("failed to seek outbox event")?;
    let mut records = Vec::with_capacity(lines.len());
    for line in lines {
        let mut record = Vec::with_capacity(line.len() + 1);
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        records.push(record);
    }
    for record in &records {
        file.write_all(record)
            .context("failed to write outbox event")?;
    }
    file.flush().context("failed to flush outbox event")?;
    file.sync_all().context("failed to sync outbox event")
}

/// Recover the seq to continue from: the `seq` of the outbox file's last
/// complete line, plus 1 — or 1 for a missing/empty file.
fn recover_last_seq_from_file(file: &mut std::fs::File, path: &Path) -> Result<u64> {
    let len = file
        .metadata()
        .with_context(|| format!("failed to stat outbox {}", path.display()))?
        .len();
    if len == 0 {
        return Ok(1);
    }
    let start = len.saturating_sub(SEQ_RECOVERY_TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut tail = vec![0u8; (len - start) as usize];
    file.read_exact(&mut tail)?;

    let line = match tail.iter().rposition(|byte| *byte == b'\n') {
        Some(last_nl) => {
            let body = &tail[..last_nl];
            match body.iter().rposition(|byte| *byte == b'\n') {
                Some(idx) => &body[idx + 1..],
                None => body,
            }
        }
        None => return Ok(1),
    };
    let line = std::str::from_utf8(line).context("outbox tail is not UTF-8")?;
    if line.trim().is_empty() {
        return Ok(1);
    }
    let envelope: RuntimeEventEnvelope =
        serde_json::from_str(line).context("failed to parse last outbox line")?;
    Ok(envelope.seq.saturating_add(1))
}

#[cfg(test)]
async fn recover_last_seq(path: &Path) -> Result<u64> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file.into_std().await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open outbox {}", path.display()));
        }
    };
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = file;
        recover_last_seq_from_file(&mut file, &path)
    })
    .await
    .context("outbox seq recovery task panicked")?
}

/// Bound free-form text to at most `max_chars` characters, stripping control
/// bytes and ANSI escape sequences and collapsing whitespace runs first.
///
/// The limit counts Unicode scalar values, not bytes, so multi-byte text gets
/// the same ceiling as ASCII. The result is safe to embed in an outbox
/// payload. Callers remain responsible for only ever passing non-secret
/// fields (error messages, previews, model/provider labels — never raw tool
/// arguments, environment, or full transcript text), the same discipline the
/// desktop notification payloads enforce.
pub fn bounded_text(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut truncated = false;
    let mut out = String::new();
    let mut char_count = 0usize;
    for ch in cleaned.chars() {
        if char_count + 1 > max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
        char_count += 1;
    }
    if truncated {
        // Make room for the marker while staying under the character ceiling.
        let marker_chars = OUTBOX_TRUNCATION_MARKER.chars().count();
        while char_count + marker_chars > max_chars {
            out.pop();
            char_count -= 1;
        }
        out.push_str(OUTBOX_TRUNCATION_MARKER);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_outbox_path(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        (dir, path)
    }

    fn event(name: &str, kind: &str) -> LifecycleEvent {
        LifecycleEvent {
            event: name.to_string(),
            kind: kind.to_string(),
            thread_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            item_id: None,
            payload: json!({"status": "completed"}),
        }
    }

    fn test_writer(path: PathBuf) -> WriterState {
        WriterState::new(
            path,
            None,
            tokio::sync::mpsc::channel(OUTBOX_QUEUE_CAPACITY).1,
        )
    }

    async fn deliver_all(state: &mut WriterState, events: Vec<LifecycleEvent>) {
        let mut webhooks = tokio::task::JoinSet::new();
        let mut pending_error = None;
        state
            .deliver_batch(&events, &mut webhooks, &mut pending_error)
            .await
            .expect("deliver");
        while webhooks.join_next().await.is_some() {}
    }

    async fn read_lines(path: &Path) -> Vec<Value> {
        let text = tokio::fs::read_to_string(path).await.expect("read outbox");
        text.lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
            .collect()
    }

    #[test]
    fn flush_persists_queued_turn_boundaries_before_runtime_teardown() {
        let (_dir, path) = temp_outbox_path("shutdown.jsonl");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let outbox = LifecycleOutbox::new(Some(path.clone()), None, None);
            outbox.emit(event("turn_start", "turn.started"));
            let clone = outbox.clone();
            clone.emit(event("turn_end", "turn.completed"));
            outbox.flush(Duration::from_secs(2)).await.expect("flush");
        });
        drop(runtime);

        let contents = std::fs::read_to_string(path).expect("outbox persisted before exit");
        let lines: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("complete JSONL record"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "turn_start");
        assert_eq!(lines[0]["seq"], 1);
        assert_eq!(lines[1]["event"], "turn_end");
        assert_eq!(lines[1]["seq"], 2);
    }

    #[tokio::test]
    async fn flush_disabled_or_unused_outbox_creates_no_file() {
        let (_dir, path) = temp_outbox_path("unused.jsonl");
        LifecycleOutbox::disabled()
            .flush(Duration::ZERO)
            .await
            .expect("disabled flush");
        LifecycleOutbox::new(Some(path.clone()), None, None)
            .flush(Duration::from_secs(2))
            .await
            .expect("unused flush");
        assert!(!path.exists());
    }

    /// A stalled webhook must not head-of-line block the audit log: the
    /// appends of later events land while the slow post is still in flight
    /// (#6212). The pre-batching writer awaited each webhook inline, so the
    /// second event's line could not appear until the first post resolved.
    #[tokio::test]
    async fn stalled_webhook_does_not_block_later_appends() {
        let (_dir, path) = temp_outbox_path("webhook-head-of-line.jsonl");
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let outbox = LifecycleOutbox::new(Some(path.clone()), Some(server.uri()), None);

        outbox.emit(event("turn_start", "turn.started"));
        // Give the writer time to start the stalled webhook post for the
        // first event before queueing the second.
        tokio::time::sleep(Duration::from_millis(100)).await;
        outbox.emit(event("turn_end", "turn.completed"));

        for _ in 0..300 {
            let appended = tokio::fs::read_to_string(&path)
                .await
                .is_ok_and(|text| text.lines().count() >= 2);
            if appended {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let lines = read_lines(&path).await;
        assert_eq!(
            lines.len(),
            2,
            "the second event must be appended while the first webhook is still stalled"
        );
        assert_eq!(lines[1]["event"], "turn_end");
    }

    #[tokio::test]
    async fn flush_is_bounded_when_webhook_delivery_stalls() {
        let (_dir, path) = temp_outbox_path("stalled.jsonl");
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let outbox = LifecycleOutbox::new(Some(path), Some(server.uri()), None);
        outbox.emit(event("turn_start", "turn.started"));
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            outbox.flush(Duration::from_millis(20)),
        )
        .await
        .expect("the flush deadline must bound a blocked writer")
        .expect_err("stalled delivery must report incomplete drain");
        assert!(error.to_string().contains("flush timed out"));
    }

    #[test]
    fn flush_reports_a_writer_lost_with_its_runtime() {
        let (_dir, path) = temp_outbox_path("stopped.jsonl");
        let outbox = LifecycleOutbox::new(Some(path), None, None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("first runtime");
        runtime.block_on(async {
            outbox.emit(event("turn_start", "turn.started"));
            outbox
                .flush(Duration::from_secs(2))
                .await
                .expect("first flush");
        });
        drop(runtime);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("second runtime");
        let error = runtime
            .block_on(outbox.flush(Duration::from_secs(2)))
            .expect_err("a closed writer cannot acknowledge delivery");
        assert!(error.to_string().contains("writer task is gone"));
    }

    #[tokio::test]
    async fn appends_one_jsonl_line_per_event_with_envelope_schema() {
        let (_dir, path) = temp_outbox_path("schema.jsonl");
        let mut state = test_writer(path.clone());
        deliver_all(&mut state, vec![event("turn_start", "turn.started")]).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["schema_version"], 1);
        assert_eq!(line["seq"], 1);
        assert_eq!(line["event"], "turn_start");
        assert_eq!(line["kind"], "turn.started");
        assert_eq!(line["thread_id"], "session-1");
        assert_eq!(line["turn_id"], "turn-1");
        assert_eq!(line["item_id"], Value::Null);
        assert!(line["timestamp"].as_str().is_some());
        assert!(line["payload"]["status"].as_str() == Some("completed"));
    }

    /// Every emit site now carries `payload.workspace` (and subagent events
    /// additionally `payload.subagent`) for consumer-side routing. The writer
    /// must preserve those fields verbatim through the envelope round trip
    /// for every event type.
    #[tokio::test]
    async fn payload_workspace_and_subagent_fields_survive_the_round_trip() {
        let (_dir, path) = temp_outbox_path("routing-fields.jsonl");
        let mut state = test_writer(path.clone());
        let workspace = "/home/cw/wt-lane";
        let subagent = "explore-1";
        let subagent_payload = json!({ "workspace": workspace, "subagent": subagent });
        deliver_all(
            &mut state,
            vec![
                LifecycleEvent {
                    event: "session_start".to_string(),
                    kind: "session.started".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: None,
                    item_id: None,
                    payload: json!({ "workspace": workspace }),
                },
                LifecycleEvent {
                    event: "turn_start".to_string(),
                    kind: "turn.started".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: Some("turn-1".to_string()),
                    item_id: None,
                    payload: json!({ "workspace": workspace }),
                },
                LifecycleEvent {
                    event: "turn_end".to_string(),
                    kind: "turn.completed".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: Some("turn-1".to_string()),
                    item_id: None,
                    payload: json!({ "workspace": workspace }),
                },
                LifecycleEvent {
                    event: "turn_stalled".to_string(),
                    kind: "turn.stalled".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: Some("turn-1".to_string()),
                    item_id: None,
                    payload: json!({ "workspace": workspace }),
                },
                LifecycleEvent {
                    event: "subagent_spawn".to_string(),
                    kind: "subagent.spawned".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: Some("turn-1".to_string()),
                    item_id: None,
                    payload: subagent_payload.clone(),
                },
                LifecycleEvent {
                    event: "subagent_complete".to_string(),
                    kind: "subagent.completed".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: Some("turn-1".to_string()),
                    item_id: None,
                    payload: subagent_payload.clone(),
                },
                LifecycleEvent {
                    event: "session_end".to_string(),
                    kind: "session.ended".to_string(),
                    thread_id: "session-1".to_string(),
                    turn_id: None,
                    item_id: None,
                    payload: json!({ "workspace": workspace }),
                },
            ],
        )
        .await;

        let lines = read_lines(&path).await;
        let events: Vec<&str> = lines
            .iter()
            .map(|line| line["event"].as_str().expect("event"))
            .collect();
        assert_eq!(
            events,
            vec![
                "session_start",
                "turn_start",
                "turn_end",
                "turn_stalled",
                "subagent_spawn",
                "subagent_complete",
                "session_end",
            ],
            "the routing-field contract must cover every lifecycle event type"
        );
        for line in &lines {
            assert_eq!(
                line["payload"]["workspace"],
                json!(workspace),
                "workspace must survive the round trip for event {}",
                line["event"]
            );
        }
        for event in ["subagent_spawn", "subagent_complete"] {
            let line = lines
                .iter()
                .find(|line| line["event"] == event)
                .expect(event);
            assert_eq!(
                line["payload"]["subagent"],
                json!(subagent),
                "subagent must survive the round trip for event {event}"
            );
        }
    }

    #[tokio::test]
    async fn seq_is_monotonic_and_recovers_across_reopen() {
        let (_dir, path) = temp_outbox_path("seq.jsonl");
        let mut state = test_writer(path.clone());
        deliver_all(
            &mut state,
            vec![
                event("session_start", "session.started"),
                event("turn_start", "turn.started"),
                event("turn_end", "turn.completed"),
            ],
        )
        .await;

        // A fresh writer (new process, same file) continues the sequence.
        let mut reopened = test_writer(path.clone());
        deliver_all(&mut reopened, vec![event("turn_start", "turn.started")]).await;

        let lines = read_lines(&path).await;
        let seqs: Vec<u64> = lines
            .iter()
            .map(|line| line["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn missing_and_empty_files_start_at_seq_1() {
        let (_dir, path) = temp_outbox_path("empty.jsonl");
        assert_eq!(recover_last_seq(&path).await.expect("missing file"), 1);

        tokio::fs::write(&path, "").await.expect("empty file");
        assert_eq!(recover_last_seq(&path).await.expect("empty file"), 1);
    }

    #[tokio::test]
    async fn partial_trailing_line_is_ignored_during_recovery() {
        let (_dir, path) = temp_outbox_path("partial.jsonl");
        tokio::fs::write(
            &path,
            format!(
                "{}\n{}\n{{\"schema_version\":1,\"seq\":3,\"event\":\"turn_",
                r#"{"schema_version":1,"seq":1,"event":"session_start","kind":"session.started","thread_id":"s","turn_id":null,"item_id":null,"timestamp":"t","payload":{}}"#,
                r#"{"schema_version":1,"seq":2,"event":"turn_start","kind":"turn.started","thread_id":"s","turn_id":null,"item_id":null,"timestamp":"t","payload":{}}"#,
            ),
        )
        .await
        .expect("write partial outbox");
        // The torn trailing line is not a complete record; recovery continues
        // from the last complete line's seq (2) → next seq 3.
        assert_eq!(recover_last_seq(&path).await.expect("recover"), 3);
    }

    #[tokio::test]
    async fn emit_queues_and_writes_in_order_without_blocking() {
        let (_dir, path) = temp_outbox_path("emit.jsonl");
        let outbox = LifecycleOutbox::new(Some(path.clone()), None, None);
        assert!(outbox.is_enabled());

        outbox.emit(event("session_start", "session.started"));
        outbox.emit(event("turn_start", "turn.started"));
        outbox.emit(event("turn_end", "turn.completed"));

        // The writer task drains asynchronously; wait for the lines to land.
        for _ in 0..100 {
            if tokio::fs::metadata(&path)
                .await
                .is_ok_and(|meta| meta.len() > 0)
                && read_lines(&path).await.len() >= 3
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 3, "expected all queued events to be written");
        let events: Vec<&str> = lines
            .iter()
            .map(|line| line["event"].as_str().expect("event"))
            .collect();
        assert_eq!(events, vec!["session_start", "turn_start", "turn_end"]);
        let seqs: Vec<u64> = lines
            .iter()
            .map(|line| line["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![1, 2, 3], "seq must be assigned in emit order");
    }

    #[test]
    fn disabled_outbox_drops_events_and_reports_disabled() {
        let outbox = LifecycleOutbox::new(None, None, None);
        assert!(!outbox.is_enabled());
        outbox.emit(event("turn_start", "turn.started")); // must not panic

        let empty_path = LifecycleOutbox::new(Some(PathBuf::new()), None, None);
        assert!(!empty_path.is_enabled());

        let default = LifecycleOutbox::default();
        assert!(!default.is_enabled());
    }

    #[test]
    fn webhook_only_configures_without_a_file_path() {
        // `webhook_url` without `path` is stored losslessly in config; the
        // outbox handle itself only activates on a path.
        let outbox = LifecycleOutbox::new(
            None,
            Some("https://example.com/hook".to_string()),
            Some("token".to_string()),
        );
        assert!(!outbox.is_enabled());
    }

    #[test]
    fn bounded_text_truncates_to_limit_with_marker() {
        assert_eq!(bounded_text("short", 80), "short");
        let long = "x".repeat(200);
        let bounded = bounded_text(&long, OUTBOX_DETAIL_MAX_CHARS);
        assert_eq!(bounded.chars().count(), OUTBOX_DETAIL_MAX_CHARS);
        assert!(bounded.ends_with(OUTBOX_TRUNCATION_MARKER));
        assert!(bounded.starts_with('x'));
    }

    #[test]
    fn bounded_text_strips_controls_and_collapses_whitespace() {
        assert_eq!(
            bounded_text("line\x1b[31m one\n\n  two\t", 80),
            "line[31m one two"
        );
        assert_eq!(bounded_text("", 80), "");
        assert_eq!(bounded_text("   \n\t  ", 80), "");
    }

    #[test]
    fn bounded_text_respects_utf8_boundaries() {
        // 30 multi-byte emoji (4 bytes each) = 120 bytes but only 30 chars.
        let emoji = "🦈".repeat(30);
        let bounded = bounded_text(&emoji, OUTBOX_DETAIL_MAX_CHARS);
        assert!(bounded.chars().count() <= OUTBOX_DETAIL_MAX_CHARS);
        assert!(bounded.starts_with('🦈'));
    }

    /// The webhook transport must POST `{"at", "event"}` JSON and, when a
    /// token is configured, send it as `Authorization: Bearer <token>`.
    #[tokio::test]
    async fn webhook_posts_at_event_payload_with_bearer_token() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/hook"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer secret-token",
            ))
            .and(wiremock::matchers::body_partial_json(json!({
                "event": {"kind": "turn.started"}
            })))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let webhook = WebhookHookSink::new_with_token(
            format!("{}/hook", server.uri()),
            Some("secret-token".to_string()),
        );
        webhook
            .post_payload(json!(
                {"at": "2026-08-19T00:00:00Z", "event": {"kind": "turn.started"}}
            ))
            .await
            .expect("webhook delivery");

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1, "exactly one webhook POST");
    }

    /// A webhook that always fails must surface its error to the caller
    /// (which logs and drops it) — never panic, never retry forever.
    #[tokio::test]
    async fn webhook_failure_is_an_error_not_a_panic() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let webhook = WebhookHookSink::new_with_token(format!("{}/hook", server.uri()), None);
        let result = webhook.post_payload(json!({})).await;
        assert!(result.is_err(), "expected the failure to be reported");
    }

    #[tokio::test]
    async fn interleaved_writers_assign_unique_monotonic_seqs() {
        let (_dir, path) = temp_outbox_path("two-writers.jsonl");
        let mut writer_a = test_writer(path.clone());
        let mut writer_b = test_writer(path.clone());

        deliver_all(&mut writer_a, vec![event("turn_start", "turn.started")]).await;
        deliver_all(&mut writer_b, vec![event("turn_start", "turn.started")]).await;
        deliver_all(
            &mut writer_a,
            vec![
                event("turn_end", "turn.completed"),
                event("session_end", "session.ended"),
            ],
        )
        .await;
        deliver_all(&mut writer_b, vec![event("turn_end", "turn.completed")]).await;

        let lines = read_lines(&path).await;
        let seqs: Vec<u64> = lines
            .iter()
            .map(|line| line["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn flush_returns_error_after_failed_durable_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outbox = LifecycleOutbox::new(Some(dir.path().to_path_buf()), None, None);

        outbox.emit(event("turn_start", "turn.started"));
        let err = outbox
            .flush(Duration::from_secs(2))
            .await
            .expect_err("flush must fail when durable append fails");
        assert!(
            err.to_string().contains("failed to open sink file"),
            "unexpected flush error: {err}"
        );

        outbox.emit(event("turn_end", "turn.completed"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        outbox
            .flush(Duration::from_secs(2))
            .await
            .expect_err("flush in a subsequent batch must still surface prior append failure");
    }

    #[test]
    fn append_lines_syncs_after_final_write_all() {
        let source = include_str!("lifecycle_outbox.rs");
        let fn_start = source
            .find("fn append_lines(")
            .expect("append_lines function must exist");
        let body = &source[fn_start..];
        let fn_end = body
            .find("\n}\n")
            .expect("append_lines closing brace must exist");
        let fn_body = &body[..fn_end];
        let write_pos = fn_body
            .rfind("write_all(")
            .expect("append_lines must call write_all");
        let sync_pos = fn_body
            .find(".sync_all(")
            .or_else(|| fn_body.find(".sync_data("))
            .expect("append_lines must fsync via sync_all or sync_data");
        assert!(sync_pos > write_pos, "fsync must happen after write_all");
    }

    #[tokio::test]
    async fn outbox_webhook_retry_sends_stable_sink_seq_idempotency_key() {
        use std::sync::atomic::AtomicUsize;

        let (_dir, path) = temp_outbox_path("webhook-idempotency.jsonl");
        let server = wiremock::MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |_req: &wiremock::Request| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    wiremock::ResponseTemplate::new(500)
                } else {
                    wiremock::ResponseTemplate::new(200)
                }
            })
            .mount(&server)
            .await;

        let outbox = LifecycleOutbox::new(Some(path.clone()), Some(server.uri()), None);
        outbox.emit(event("turn_start", "turn.started"));
        outbox.flush(Duration::from_secs(2)).await.expect("flush 1");
        outbox.emit(event("turn_end", "turn.completed"));
        outbox.flush(Duration::from_secs(2)).await.expect("flush 2");

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 3);
        let keys: Vec<&str> = requests
            .iter()
            .map(|req| {
                req.headers
                    .get("idempotency-key")
                    .and_then(|v| v.to_str().ok())
                    .expect("Idempotency-Key header")
            })
            .collect();
        assert_eq!(keys[0], keys[1], "retry must reuse the same key");
        assert!(keys[0].ends_with(":1"), "first event key: {}", keys[0]);
        assert!(keys[2].ends_with(":2"), "second event key: {}", keys[2]);
        assert_ne!(keys[0], keys[2]);
    }
}

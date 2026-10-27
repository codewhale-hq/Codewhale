//! Durable fleet inbox and run ledger.
//!
//! Stores fleet state as append-only JSONL so the manager can survive
//! restarts and reconstruct queue/worker state by replaying records.
//! Artifacts are referenced by bounded metadata; large payloads live on disk
//! and are never embedded in the ledger.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::collections::HashMap;
#[cfg(test)]
use std::fs::OpenOptions;
use std::sync::{Mutex, OnceLock};

use codewhale_util::files::{WorkspaceFile, same_file};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use codewhale_protocol::fleet::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

const FLEET_DIR: &str = ".codewhale";
const FLEET_LEDGER_FILE: &str = "fleet.jsonl";
const FLEET_LEDGER_LOCK_FILE: &str = "fleet.lock";

/// Path of the ledger file under `workspace`, mirroring [`FleetLedger::open`].
pub(crate) fn fleet_ledger_path(workspace: &Path) -> PathBuf {
    workspace.join(FLEET_DIR).join(FLEET_LEDGER_FILE)
}

/// Process-wide append wakes, one [`Notify`] per ledger file (#6211 R7b).
/// Ledger managers open per operation, so instances cannot share a channel
/// handle; the registry joins differently-spelled opens of one file by
/// canonicalized path. A wake means "re-poll with your cursor" — the cursor
/// stays the source of truth, so a missed or spurious wake only costs one
/// extra poll, never lost or duplicated events.
static FLEET_LEDGER_WAKES: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<Notify>>>> =
    OnceLock::new();

fn fleet_ledger_wake_key(ledger_path: &Path) -> PathBuf {
    if let Ok(canonical) = ledger_path.canonicalize() {
        return canonical;
    }
    // The ledger file may not exist yet at subscribe time; canonicalize the
    // parent so both spellings still meet. A fully missing tree falls back
    // to the raw path on both sides.
    match ledger_path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(ledger_path.file_name())
    {
        Some((parent, name)) => parent.join(name),
        None => ledger_path.to_path_buf(),
    }
}

/// Subscribe to append wakes for the ledger at `ledger_path`. Callers must
/// still poll on a fallback interval: the registry only sees in-process
/// appends, and a wake can race the subscriber's last read.
pub(crate) fn subscribe_fleet_ledger_appends(ledger_path: &Path) -> std::sync::Arc<Notify> {
    let key = fleet_ledger_wake_key(ledger_path);
    let mut wakes = FLEET_LEDGER_WAKES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("fleet ledger wake registry poisoned");
    wakes
        .entry(key)
        .or_insert_with(|| std::sync::Arc::new(Notify::new()))
        .clone()
}

fn notify_fleet_ledger_append(ledger_path: &Path) {
    let key = fleet_ledger_wake_key(ledger_path);
    let notify = FLEET_LEDGER_WAKES
        .get()
        .and_then(|wakes| wakes.lock().ok())
        .and_then(|wakes| wakes.get(&key).cloned());
    if let Some(notify) = notify {
        notify.notify_waiters();
    }
}

fn inline_secret_assignment_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(?:"|')?\b(api[_-]?key|apikey|secret|token|password|passwd|authorization|auth[_-]?token|access[_-]?key|client[_-]?secret|private[_-]?key)\b(?:"|')?\s*([:=])\s*(?:"[^"]*"|'[^']*'|[^\s,;}\]]+)"#,
        )
        .expect("Fleet inline secret redaction pattern must compile")
    })
}

fn bearer_secret_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}")
            .expect("Fleet bearer secret redaction pattern must compile")
    })
}

/// A single append-only record in the fleet ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum FleetLedgerRecord {
    /// Replay generation rotated whenever compaction replaces durable history.
    /// Legacy ledgers omit this record and use the stable `legacy` generation
    /// until their first compaction.
    ReplayEpoch {
        epoch: String,
    },
    RunCreated {
        // Boxed: FleetRun is by far the largest payload; boxing keeps the enum
        // small (clippy::large_enum_variant). Serde treats Box<T> as T.
        run: Box<FleetRun>,
    },
    RunStatusChanged {
        run_id: FleetRunId,
        status: FleetRunStatus,
        timestamp: String,
    },
    TaskEnqueued {
        entry: FleetInboxEntry,
    },
    TaskLeased {
        run_id: FleetRunId,
        task_id: String,
        worker_id: String,
        leased_at: String,
        lease_expires_at: Option<String>,
    },
    TaskCompletedOrFailed {
        run_id: FleetRunId,
        task_id: String,
        worker_id: String,
        timestamp: String,
        #[serde(default = "default_terminal_task_status")]
        status: FleetTaskLedgerStatus,
    },
    /// Exact owner lifecycle high-water mark retained across compaction. Raw
    /// lifecycle records remain the normal source; this checkpoint prevents a
    /// compacted multi-lease history from reusing lower graph idempotency keys.
    TaskLifecycleCheckpoint {
        run_id: FleetRunId,
        task_id: String,
        lifecycle_seq: u64,
    },
    /// One crash-atomic terminal transition and its attempt-fenced receipt.
    /// Keeping these values in one JSONL record prevents a process crash from
    /// publishing a terminal task without the receipt that explains it.
    TaskAttemptFinalized {
        event: FleetWorkerEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_status: Option<FleetTaskLedgerStatus>,
        receipt: Box<FleetReceipt>,
    },
    EventAppended {
        event: FleetWorkerEvent,
    },
    /// Durable per-task event high-water mark retained by compaction even when
    /// the highest raw event was intentionally excluded from projections (for
    /// example, late progress after cancellation).
    EventSequenceCheckpoint {
        run_id: FleetRunId,
        worker_id: String,
        task_id: String,
        seq: u64,
    },
    Heartbeat {
        worker_id: String,
        timestamp: String,
        #[serde(default)]
        cpu_percent: Option<f32>,
        #[serde(default)]
        memory_mb: Option<u64>,
    },
    ReceiptRecorded {
        // Boxed for the same reason as RunCreated: FleetReceipt is the largest
        // variant payload. Serde treats Box<T> as T.
        receipt: Box<FleetReceipt>,
    },
    AlertSent {
        run_id: FleetRunId,
        task_id: String,
        channel: String,
        timestamp: String,
        /// Present on attempt-fenced restart-exhaustion alerts. Older records
        /// omit these fields and remain replayable as audit-only entries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
}

/// Reconstructed fleet state after replaying the ledger.
#[derive(Debug, Clone, Default)]
pub struct FleetLedgerState {
    pub runs: BTreeMap<String, FleetRun>,
    pub run_status_overrides: BTreeMap<String, FleetRunStatus>,
    /// Tasks keyed by run_id:task_id.
    pub tasks: BTreeMap<String, FleetTaskState>,
    /// Worker status by worker_id.
    pub workers: BTreeMap<String, FleetWorkerStatus>,
    /// Latest heartbeat by worker_id.
    pub heartbeats: BTreeMap<String, FleetHeartbeatState>,
    /// Latest event seq per worker_id:task_id.
    pub latest_seq: BTreeMap<String, u64>,
    /// Structured owner for each sequence key, used to write compaction
    /// checkpoints without parsing delimiter-bearing ids back out of a string.
    pub(crate) sequence_owners: BTreeMap<String, FleetEventSequenceOwner>,
    /// Latest event envelope per worker_id:run_id:task_id.
    pub latest_events: BTreeMap<String, FleetWorkerEvent>,
    /// Artifact events keyed by worker_id:run_id:task_id:path.
    pub artifact_events: BTreeMap<String, FleetWorkerEvent>,
    /// Restart events keyed by worker_id:run_id:task_id.
    pub restarted_events: BTreeMap<String, FleetWorkerEvent>,
    /// Escalation events keyed by worker_id:run_id:task_id.
    pub escalated_events: BTreeMap<String, FleetWorkerEvent>,
    /// Completed receipts by run_id:task_id.
    pub receipts: BTreeMap<String, FleetReceipt>,
    /// Durable alert deliveries keyed by run/task/attempt/channel.
    pub(crate) alerts: BTreeMap<(String, String, Option<u32>, String), FleetLedgerAlert>,
    /// Accumulated worker usage per run_id (R6, #5567), folded from
    /// `UsageReport` events during replay.
    pub run_usage: BTreeMap<String, FleetRunUsage>,
}

/// Run-level usage accumulator (R6, #5567).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FleetRunUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl FleetRunUsage {
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Run-level budget alert identity (R6, #5567): one alert per run, not per
/// task, so the dedupe key uses a reserved task slot.
pub(crate) const BUDGET_ALERT_TASK: &str = "__run__";
pub(crate) const BUDGET_ALERT_CHANNEL: &str = "budget_exceeded";

impl FleetLedgerState {
    /// Accumulated worker usage for one run; zero when nothing reported.
    #[must_use]
    pub fn run_usage(&self, run_id: &FleetRunId) -> FleetRunUsage {
        self.run_usage.get(&run_id.0).copied().unwrap_or_default()
    }

    /// `(total, ceiling)` when the run declares a usage ceiling and the
    /// accumulated total has reached it; `None` for unbounded runs.
    #[must_use]
    pub fn run_ceiling_breached(&self, run_id: &FleetRunId) -> Option<(u64, u64)> {
        let ceiling = self
            .runs
            .get(&run_id.0)?
            .usage_ceiling
            .as_ref()?
            .max_total_tokens;
        let total = self.run_usage(run_id).total_tokens();
        (total >= ceiling).then_some((total, ceiling))
    }
}

#[derive(Debug, Clone)]
pub struct FleetTaskState {
    pub entry: FleetInboxEntry,
    pub status: FleetTaskLedgerStatus,
    /// Monotonic owner sequence reconstructed from durable lifecycle records.
    pub lifecycle_seq: u64,
    pub leased_to: Option<String>,
    pub leased_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetTaskLedgerStatus {
    Enqueued,
    Leased,
    Completed,
    Failed,
    Cancelled,
}

fn default_terminal_task_status() -> FleetTaskLedgerStatus {
    FleetTaskLedgerStatus::Completed
}

#[derive(Debug, Clone)]
pub struct FleetHeartbeatState {
    pub timestamp: String,
    pub cpu_percent: Option<f32>,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct FleetLedgerAlert {
    run_id: FleetRunId,
    task_id: String,
    channel: String,
    timestamp: String,
    worker_id: Option<String>,
    attempt: Option<u32>,
    seq: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct FleetEventSequenceOwner {
    run_id: FleetRunId,
    worker_id: String,
    task_id: String,
}

/// Precise failure boundary for managed-client replay.
#[derive(Debug, thiserror::Error)]
pub enum FleetEventReplayError {
    #[error("fleet run {run_id} does not exist")]
    UnknownRun { run_id: String },
    #[error("fleet event cursor is no longer available for run {run_id}")]
    CursorUnavailable { run_id: String },
    #[error("failed to read fleet event history: {message}")]
    Storage { message: String },
}

/// Append-only JSONL ledger for fleet runs.
#[derive(Debug)]
pub struct FleetLedger {
    ledger_path: PathBuf,
    ledger_file: WorkspaceFile,
    lock_file: WorkspaceFile,
    original_lock: std::fs::File,
    /// Stable advisory-lock inode shared by every manager for this workspace.
    ///
    /// The ledger itself is replaced during compaction, so locking
    /// `fleet.jsonl` would leave appenders holding a lock on the old inode.
    lock_path: PathBuf,
    #[cfg(test)]
    fail_start_append_after_callback: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_restart_append_after_callback: std::sync::atomic::AtomicBool,
}

impl FleetLedger {
    /// Open (or create) the ledger under `workspace/.codewhale/fleet.jsonl`.
    pub fn open(workspace: &Path) -> Result<Self> {
        let relative = Path::new(FLEET_DIR).join(FLEET_LEDGER_FILE);
        let ledger_path = workspace.join(&relative);
        let ledger_file = WorkspaceFile::open(workspace, &relative, true)?;
        ledger_file
            .open_update(true, true)
            .with_context(|| format!("creating fleet ledger {}", ledger_path.display()))?;
        let lock_path = workspace.join(FLEET_DIR).join(FLEET_LEDGER_LOCK_FILE);
        let lock_file = ledger_file.sibling(FLEET_LEDGER_LOCK_FILE)?;
        let original_lock = lock_file
            .open_update(true, false)
            .with_context(|| format!("creating fleet ledger lock {}", lock_path.display()))?;
        Ok(Self {
            ledger_file,
            lock_file,
            original_lock,
            ledger_path,
            lock_path,
            #[cfg(test)]
            fail_start_append_after_callback: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_restart_append_after_callback: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn path(&self) -> &Path {
        &self.ledger_path
    }

    #[cfg(test)]
    pub(crate) fn fail_next_start_append_after_callback(&self) {
        self.fail_start_append_after_callback
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_restart_append_after_callback(&self) {
        self.fail_restart_append_after_callback
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn open_lock_file(&self) -> Result<std::fs::File> {
        let file = self
            .lock_file
            .open_update(false, false)
            .with_context(|| format!("opening fleet ledger lock {}", self.lock_path.display()))?;
        if !same_file(&file, &self.original_lock)? {
            bail!("Fleet ledger lock was replaced; reopen the workspace before continuing");
        }
        Ok(file)
    }

    fn with_read_lock<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let lock_file = self.open_lock_file()?;
        let lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .read()
            .with_context(|| format!("read-locking fleet ledger {}", self.ledger_path.display()))?;
        action()
    }

    fn with_write_lock<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let lock_file = self.open_lock_file()?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write().with_context(|| {
            format!("write-locking fleet ledger {}", self.ledger_path.display())
        })?;
        action()
    }

    /// Append a single record without rewriting existing ledger contents.
    fn append_record_unlocked(&self, record: &FleetLedgerRecord) -> Result<()> {
        self.append_records_unlocked(std::slice::from_ref(record))
    }

    /// Append a transaction's records with one write while the caller holds
    /// the workspace ledger lock. Cancellation uses this to publish its
    /// interrupted + cancelled pair without another manager allocating a
    /// sequence number between them.
    fn append_records_unlocked(&self, records: &[FleetLedgerRecord]) -> Result<()> {
        let mut lines = String::new();
        for record in records {
            lines.push_str(
                &serde_json::to_string(record).context("serializing fleet ledger record")?,
            );
            lines.push('\n');
        }
        let mut file = self
            .ledger_file
            .open_update(true, true)
            .with_context(|| format!("opening fleet ledger {}", self.ledger_path.display()))?;
        // A process can die after writing only part of its final JSON record.
        // O_APPEND would otherwise concatenate the next valid record directly
        // onto that unterminated tail, causing replay to discard both. Preserve
        // the forensic fragment but quarantine it as its own malformed line.
        let len = file
            .metadata()
            .with_context(|| {
                format!(
                    "reading fleet ledger metadata {}",
                    self.ledger_path.display()
                )
            })?
            .len();
        if len > 0 {
            file.seek(SeekFrom::End(-1)).with_context(|| {
                format!("seeking fleet ledger tail {}", self.ledger_path.display())
            })?;
            let mut tail = [0_u8; 1];
            file.read_exact(&mut tail).with_context(|| {
                format!("reading fleet ledger tail {}", self.ledger_path.display())
            })?;
            if tail[0] != b'\n' {
                file.write_all(b"\n").with_context(|| {
                    format!(
                        "quarantining fleet ledger tail {}",
                        self.ledger_path.display()
                    )
                })?;
            }
        }
        file.write_all(lines.as_bytes())
            .with_context(|| format!("appending fleet ledger {}", self.ledger_path.display()))?;
        file.flush()
            .with_context(|| format!("flushing fleet ledger {}", self.ledger_path.display()))?;
        file.sync_data()
            .with_context(|| format!("syncing fleet ledger {}", self.ledger_path.display()))?;
        // Wake SSE subscribers after the bytes are durable (#6211 R7b). Every
        // record kind funnels through here, so a wake only promises "something
        // changed" — subscribers re-poll with their cursor.
        notify_fleet_ledger_append(&self.ledger_path);
        Ok(())
    }

    /// Append a single record while excluding compaction and other writers.
    fn append_record(&self, record: &FleetLedgerRecord) -> Result<()> {
        self.with_write_lock(|| self.append_record_unlocked(record))
    }

    pub fn create_run(&self, run: &FleetRun) -> Result<()> {
        self.append_record(&FleetLedgerRecord::RunCreated {
            run: Box::new(sanitize_run_for_ledger(run)),
        })
    }

    pub fn update_run_status(
        &self,
        run_id: &FleetRunId,
        status: FleetRunStatus,
        timestamp: &str,
    ) -> Result<()> {
        self.append_record(&FleetLedgerRecord::RunStatusChanged {
            run_id: run_id.clone(),
            status,
            timestamp: timestamp.to_string(),
        })
    }

    pub fn enqueue(&self, entry: FleetInboxEntry) -> Result<()> {
        self.append_record(&FleetLedgerRecord::TaskEnqueued { entry })
    }

    /// Mark a task as leased to a worker.
    ///
    /// Production transitions must use one of the compare-and-set helpers
    /// below. This raw append remains available only to focused replay tests.
    #[cfg(test)]
    pub fn lease_task(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        leased_at: &str,
        lease_expires_at: Option<&str>,
    ) -> Result<()> {
        self.append_record(&FleetLedgerRecord::TaskLeased {
            run_id: run_id.clone(),
            task_id: task_id.to_string(),
            worker_id: worker_id.to_string(),
            leased_at: leased_at.to_string(),
            lease_expires_at: lease_expires_at.map(String::from),
        })
    }

    /// Atomically lease one queued task to an idle logical worker.
    ///
    /// The queue snapshot, task/worker eligibility checks, run-capacity check,
    /// and durable lease append share one cross-process lock. A competing
    /// manager therefore observes either the queued task or the winning lease,
    /// never the same stale snapshot followed by a second lease.
    pub fn lease_task_if_enqueued(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        leased_at: &str,
        lease_expires_at: Option<&str>,
        max_active_for_run: Option<usize>,
    ) -> Result<bool> {
        self.lease_task_if_enqueued_with_events(
            run_id,
            task_id,
            worker_id,
            leased_at,
            lease_expires_at,
            max_active_for_run,
            Vec::new(),
            false,
            || Ok(()),
        )
    }

    /// Atomically claim a queued task and publish its initial lifecycle.
    ///
    /// The callback runs before the durable transaction while the same ledger
    /// lock is held. Fleet uses it to synchronously persist the exact launch
    /// projection; a callback failure leaves the task Enqueued. Callers must
    /// compensate the projection if the subsequent ledger append fails.
    #[allow(clippy::too_many_arguments)]
    pub fn start_task_if_enqueued(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        leased_at: &str,
        lease_expires_at: Option<&str>,
        max_active_for_run: Option<usize>,
        initial_events: Vec<FleetWorkerEventPayload>,
        on_started: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        self.lease_task_if_enqueued_with_events(
            run_id,
            task_id,
            worker_id,
            leased_at,
            lease_expires_at,
            max_active_for_run,
            initial_events,
            true,
            on_started,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn lease_task_if_enqueued_with_events(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        leased_at: &str,
        lease_expires_at: Option<&str>,
        max_active_for_run: Option<usize>,
        initial_events: Vec<FleetWorkerEventPayload>,
        record_heartbeat: bool,
        on_started: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        self.with_write_lock(move || {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(false);
            };
            if task.status != FleetTaskLedgerStatus::Enqueued {
                return Ok(false);
            }
            if state.tasks.values().any(|candidate| {
                candidate.status == FleetTaskLedgerStatus::Leased
                    && candidate.leased_to.as_deref() == Some(worker_id)
            }) {
                return Ok(false);
            }
            if max_active_for_run.is_some_and(|max_active| {
                state
                    .tasks
                    .values()
                    .filter(|candidate| candidate.entry.run_id == *run_id)
                    .filter(|candidate| candidate.status == FleetTaskLedgerStatus::Leased)
                    .count()
                    >= max_active
            }) {
                return Ok(false);
            }

            // Budget ceiling (R6, #5567): a breached run admits nothing new.
            // Already-leased tasks finish; the once-only alert is recorded by
            // the usage-event path.
            if state.run_ceiling_breached(run_id).is_some() {
                return Ok(false);
            }

            let mut records =
                Vec::with_capacity(1 + initial_events.len() + usize::from(record_heartbeat));
            records.push(FleetLedgerRecord::TaskLeased {
                run_id: run_id.clone(),
                task_id: task_id.to_string(),
                worker_id: worker_id.to_string(),
                leased_at: leased_at.to_string(),
                lease_expires_at: lease_expires_at.map(String::from),
            });
            let event_key = event_key(worker_id, &run_id.0, task_id);
            let mut next_seq = state.latest_seq.get(&event_key).copied().unwrap_or(0) + 1;
            for payload in initial_events {
                records.push(FleetLedgerRecord::EventAppended {
                    event: FleetWorkerEvent {
                        seq: next_seq,
                        run_id: run_id.clone(),
                        worker_id: worker_id.to_string(),
                        task_id: task_id.to_string(),
                        timestamp: leased_at.to_string(),
                        payload,
                        extra: BTreeMap::new(),
                    },
                });
                next_seq = next_seq.saturating_add(1);
            }
            if record_heartbeat {
                records.push(FleetLedgerRecord::Heartbeat {
                    worker_id: worker_id.to_string(),
                    timestamp: leased_at.to_string(),
                    cpu_percent: None,
                    memory_mb: None,
                });
            }
            // Persist the exact launch/coordination projection before making
            // the lease visible. A failed callback leaves the task Enqueued
            // with attempt 0 rather than a Running lease that cannot launch.
            on_started()?;
            #[cfg(test)]
            if self
                .fail_start_append_after_callback
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                bail!("forced Fleet ledger append failure after coordination callback");
            }
            self.append_records_unlocked(&records)?;
            Ok(true)
        })
    }

    #[cfg(test)]
    pub fn mark_task_terminal_status(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: Option<&str>,
        timestamp: &str,
        status: FleetTaskLedgerStatus,
    ) -> Result<()> {
        self.append_record(&FleetLedgerRecord::TaskCompletedOrFailed {
            run_id: run_id.clone(),
            task_id: task_id.to_string(),
            worker_id: worker_id.unwrap_or_default().to_string(),
            timestamp: timestamp.to_string(),
            status,
        })
    }

    /// Change a task's terminal projection only if the exact attempt and event
    /// observed by the verifier are still current. A concurrent restart changes
    /// both the attempt counter and lifecycle sequence, so late verification
    /// can never fail the replacement attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn mark_task_terminal_status_if_unchanged(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        expected_status: FleetTaskLedgerStatus,
        expected_attempts: u32,
        expected_latest_seq: u64,
        timestamp: &str,
        status: FleetTaskLedgerStatus,
    ) -> Result<bool> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(false);
            };
            let latest_seq = state
                .latest_seq
                .get(&event_key(worker_id, &run_id.0, task_id))
                .copied()
                .unwrap_or(0);
            if task.status != expected_status
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
                || latest_seq != expected_latest_seq
            {
                return Ok(false);
            }
            self.append_record_unlocked(&FleetLedgerRecord::TaskCompletedOrFailed {
                run_id: run_id.clone(),
                task_id: task_id.to_string(),
                worker_id: worker_id.to_string(),
                timestamp: timestamp.to_string(),
                status,
            })?;
            Ok(true)
        })
    }

    pub fn append_event(&self, event: FleetWorkerEvent) -> Result<()> {
        let usage_receipt = matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. });
        let run_id = event.run_id.clone();
        let timestamp = event.timestamp.clone();
        self.append_record(&FleetLedgerRecord::EventAppended { event })?;
        if usage_receipt {
            self.record_budget_exceeded_alert_once(&run_id, &timestamp)?;
        }
        Ok(())
    }

    /// Allocate and append the next event sequence under one ledger lock.
    pub fn append_event_next_seq(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<FleetWorkerEvent> {
        let event = self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            self.append_record_unlocked(&FleetLedgerRecord::EventAppended {
                event: event.clone(),
            })?;
            Ok(event)
        })?;
        if matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. }) {
            self.record_budget_exceeded_alert_once(run_id, timestamp)?;
        }
        Ok(event)
    }

    /// Append progress only while this exact worker still owns the live lease.
    /// Host startup and stream draining use this guard so output produced after
    /// an out-of-process cancellation cannot become durable task progress.
    pub fn append_event_if_leased(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        expected_attempts: u32,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<Option<FleetWorkerEvent>> {
        if matches!(
            &payload,
            FleetWorkerEventPayload::Completed { .. }
                | FleetWorkerEventPayload::Failed { .. }
                | FleetWorkerEventPayload::Cancelled { .. }
        ) {
            bail!("conditional progress append does not accept terminal worker events");
        }
        let appended = self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(None);
            };
            if task.status != FleetTaskLedgerStatus::Leased
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
            {
                return Ok(None);
            }
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            self.append_record_unlocked(&FleetLedgerRecord::EventAppended {
                event: event.clone(),
            })?;
            Ok(Some(event))
        });
        if let Ok(Some(event)) = &appended
            && matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. })
        {
            self.record_budget_exceeded_alert_once(run_id, timestamp)?;
        }
        appended
    }

    /// Append a non-terminal scheduler event only if the complete lease
    /// version observed during policy evaluation is still current.
    #[allow(clippy::too_many_arguments)]
    pub fn append_event_if_lease_unchanged(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        expected_attempts: u32,
        expected_latest_seq: u64,
        expected_heartbeat_at: Option<&str>,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<Option<FleetWorkerEvent>> {
        if matches!(
            &payload,
            FleetWorkerEventPayload::Completed { .. }
                | FleetWorkerEventPayload::Failed { .. }
                | FleetWorkerEventPayload::Cancelled { .. }
        ) {
            bail!("conditional progress append does not accept terminal worker events");
        }
        let appended = self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(None);
            };
            let latest_seq = state
                .latest_seq
                .get(&event_key(worker_id, &run_id.0, task_id))
                .copied()
                .unwrap_or(0);
            let heartbeat_at = state
                .heartbeats
                .get(worker_id)
                .map(|heartbeat| heartbeat.timestamp.as_str());
            if task.status != FleetTaskLedgerStatus::Leased
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
                || latest_seq != expected_latest_seq
                || heartbeat_at != expected_heartbeat_at
            {
                return Ok(None);
            }
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            self.append_record_unlocked(&FleetLedgerRecord::EventAppended {
                event: event.clone(),
            })?;
            Ok(Some(event))
        });
        if let Ok(Some(event)) = &appended
            && matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. })
        {
            self.record_budget_exceeded_alert_once(run_id, timestamp)?;
        }
        appended
    }

    /// Append a terminal worker event only while the expected task lease is
    /// still live. This is the completion side of the same compare-and-set used
    /// by cancellation: whichever terminal transition acquires the ledger lock
    /// first wins, and the loser cannot overwrite the task or mint a receipt.
    pub fn append_terminal_event_if_leased(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        expected_attempts: u32,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<Option<FleetWorkerEvent>> {
        if !matches!(
            &payload,
            FleetWorkerEventPayload::Completed { .. }
                | FleetWorkerEventPayload::Failed { .. }
                | FleetWorkerEventPayload::Cancelled { .. }
        ) {
            bail!("conditional terminal append requires a terminal worker event");
        }
        let appended = self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(None);
            };
            if task.status != FleetTaskLedgerStatus::Leased
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
            {
                return Ok(None);
            }
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            self.append_record_unlocked(&FleetLedgerRecord::EventAppended {
                event: event.clone(),
            })?;
            Ok(Some(event))
        });
        if let Ok(Some(event)) = &appended
            && matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. })
        {
            self.record_budget_exceeded_alert_once(run_id, timestamp)?;
        }
        appended
    }

    /// Finalize one exact process attempt and its receipt as one JSONL record.
    /// A restart increments `attempts`, so a late process or verifier cannot
    /// terminalize or publish evidence for its replacement.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize_task_attempt_if_leased(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        expected_attempts: u32,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
        final_status: Option<FleetTaskLedgerStatus>,
        mut receipt: FleetReceipt,
    ) -> Result<Option<FleetWorkerEvent>> {
        if !matches!(
            &payload,
            FleetWorkerEventPayload::Completed { .. }
                | FleetWorkerEventPayload::Failed { .. }
                | FleetWorkerEventPayload::Cancelled { .. }
        ) {
            bail!("attempt finalization requires a terminal worker event");
        }
        if final_status.is_some_and(|status| {
            !matches!(
                status,
                FleetTaskLedgerStatus::Completed
                    | FleetTaskLedgerStatus::Failed
                    | FleetTaskLedgerStatus::Cancelled
            )
        }) {
            bail!("attempt finalization status must be terminal");
        }
        if receipt.run_id != *run_id || receipt.task_id != task_id || receipt.worker_id != worker_id
        {
            bail!("attempt receipt identity does not match its terminal event");
        }
        if receipt
            .attempt
            .is_some_and(|attempt| attempt != expected_attempts)
        {
            bail!("attempt receipt generation does not match its lease");
        }
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(None);
            };
            if task.status != FleetTaskLedgerStatus::Leased
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
            {
                return Ok(None);
            }
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            receipt.attempt = Some(expected_attempts);
            receipt.terminal_seq = Some(event.seq);
            self.append_record_unlocked(&FleetLedgerRecord::TaskAttemptFinalized {
                event: event.clone(),
                final_status,
                receipt: Box::new(receipt),
            })?;
            Ok(Some(event))
        })
    }

    /// Append a scheduler-owned terminal event only if the stale lease version
    /// it evaluated is still exact. In particular, a fresh heartbeat arriving
    /// after stale detection invalidates exhaustion/failure of that worker.
    #[allow(clippy::too_many_arguments)]
    pub fn append_terminal_event_if_lease_unchanged(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        expected_attempts: u32,
        expected_latest_seq: u64,
        expected_heartbeat_at: Option<&str>,
        timestamp: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<Option<FleetWorkerEvent>> {
        if !matches!(
            &payload,
            FleetWorkerEventPayload::Completed { .. }
                | FleetWorkerEventPayload::Failed { .. }
                | FleetWorkerEventPayload::Cancelled { .. }
        ) {
            bail!("conditional terminal append requires a terminal worker event");
        }
        let appended = self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(None);
            };
            let latest_seq = state
                .latest_seq
                .get(&event_key(worker_id, &run_id.0, task_id))
                .copied()
                .unwrap_or(0);
            let heartbeat_at = state
                .heartbeats
                .get(worker_id)
                .map(|heartbeat| heartbeat.timestamp.as_str());
            if task.status != FleetTaskLedgerStatus::Leased
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
                || latest_seq != expected_latest_seq
                || heartbeat_at != expected_heartbeat_at
            {
                return Ok(None);
            }
            let event = next_worker_event(&state, run_id, worker_id, task_id, timestamp, payload);
            self.append_record_unlocked(&FleetLedgerRecord::EventAppended {
                event: event.clone(),
            })?;
            Ok(Some(event))
        });
        if let Ok(Some(event)) = &appended
            && matches!(&event.payload, FleetWorkerEventPayload::UsageReport { .. })
        {
            self.record_budget_exceeded_alert_once(run_id, timestamp)?;
        }
        appended
    }

    /// Atomically restart the exact task attempt observed by a manager.
    ///
    /// Status, worker ownership, attempt count, latest event sequence, and
    /// heartbeat are the transition version. Any intervening cancellation,
    /// completion, progress event, heartbeat, or competing restart makes this
    /// compare-and-set lose without appending a second lease.
    #[allow(clippy::too_many_arguments)]
    pub fn restart_task_if_unchanged(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        expected_status: FleetTaskLedgerStatus,
        expected_attempts: u32,
        expected_latest_seq: u64,
        expected_heartbeat_at: Option<&str>,
        leased_at: &str,
        lease_expires_at: Option<&str>,
        restart_count: u32,
    ) -> Result<bool> {
        self.restart_task_if_unchanged_with_callback(
            run_id,
            task_id,
            worker_id,
            expected_status,
            expected_attempts,
            expected_latest_seq,
            expected_heartbeat_at,
            leased_at,
            lease_expires_at,
            restart_count,
            || Ok(()),
        )
    }

    /// Restart the observed attempt while persisting its replacement launch
    /// identity before the new lease becomes visible. This mirrors
    /// `start_task_if_enqueued`: a failed callback leaves the old attempt
    /// untouched, so no leased generation can exist without a matching
    /// durable launch manifest.
    #[allow(clippy::too_many_arguments)]
    pub fn restart_task_if_unchanged_with_callback(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        expected_status: FleetTaskLedgerStatus,
        expected_attempts: u32,
        expected_latest_seq: u64,
        expected_heartbeat_at: Option<&str>,
        leased_at: &str,
        lease_expires_at: Option<&str>,
        restart_count: u32,
        on_restarted: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        self.with_write_lock(move || {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(false);
            };
            let lifecycle_key = event_key(worker_id, &run_id.0, task_id);
            let latest_seq = state.latest_seq.get(&lifecycle_key).copied().unwrap_or(0);
            let heartbeat_at = state
                .heartbeats
                .get(worker_id)
                .map(|heartbeat| heartbeat.timestamp.as_str());
            if task.status != expected_status
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
                || latest_seq != expected_latest_seq
                || heartbeat_at != expected_heartbeat_at
            {
                return Ok(false);
            }
            if state.tasks.iter().any(|(candidate_key, candidate)| {
                candidate_key != &key
                    && candidate.status == FleetTaskLedgerStatus::Leased
                    && candidate.leased_to.as_deref() == Some(worker_id)
            }) {
                return Ok(false);
            }

            let restarted = FleetWorkerEvent {
                seq: latest_seq.saturating_add(1),
                run_id: run_id.clone(),
                worker_id: worker_id.to_string(),
                task_id: task_id.to_string(),
                timestamp: leased_at.to_string(),
                payload: FleetWorkerEventPayload::Restarted { restart_count },
                extra: BTreeMap::new(),
            };
            let running = FleetWorkerEvent {
                seq: restarted.seq.saturating_add(1),
                run_id: run_id.clone(),
                worker_id: worker_id.to_string(),
                task_id: task_id.to_string(),
                timestamp: leased_at.to_string(),
                payload: FleetWorkerEventPayload::Running,
                extra: BTreeMap::new(),
            };
            on_restarted()?;
            #[cfg(test)]
            if self
                .fail_restart_append_after_callback
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                bail!("forced Fleet restart ledger append failure after coordination callback");
            }
            self.append_records_unlocked(&[
                FleetLedgerRecord::TaskLeased {
                    run_id: run_id.clone(),
                    task_id: task_id.to_string(),
                    worker_id: worker_id.to_string(),
                    leased_at: leased_at.to_string(),
                    lease_expires_at: lease_expires_at.map(String::from),
                },
                FleetLedgerRecord::EventAppended { event: restarted },
                FleetLedgerRecord::EventAppended { event: running },
                FleetLedgerRecord::Heartbeat {
                    worker_id: worker_id.to_string(),
                    timestamp: leased_at.to_string(),
                    cpu_percent: None,
                    memory_mb: None,
                },
            ])?;
            Ok(true)
        })
    }

    /// Atomically cancel an active task, optionally requiring one exact lease.
    ///
    /// `expected_worker_id = Some` is a compare-and-set for an operator command
    /// targeting a specific live worker. `None` expresses run-wide cancellation
    /// and accepts either a queued task or whichever worker leased it before
    /// this transaction acquired the lock.
    pub fn cancel_task_if_active(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        expected_worker_id: Option<&str>,
        timestamp: &str,
        signal: Option<&str>,
        cancelled_by: Option<&str>,
    ) -> Result<bool> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&key) else {
                return Ok(false);
            };
            if !matches!(
                task.status,
                FleetTaskLedgerStatus::Enqueued | FleetTaskLedgerStatus::Leased
            ) {
                return Ok(false);
            }
            if expected_worker_id.is_some_and(|expected| {
                task.status != FleetTaskLedgerStatus::Leased
                    || task.leased_to.as_deref() != Some(expected)
            }) {
                return Ok(false);
            }

            if let Some(worker_id) = task.leased_to.as_deref() {
                let event_key = event_key(worker_id, &run_id.0, task_id);
                let first_seq = state.latest_seq.get(&event_key).copied().unwrap_or(0) + 1;
                let mut records = Vec::with_capacity(2);
                let mut next_seq = first_seq;
                if let Some(signal) = signal {
                    records.push(FleetLedgerRecord::EventAppended {
                        event: FleetWorkerEvent {
                            seq: next_seq,
                            run_id: run_id.clone(),
                            worker_id: worker_id.to_string(),
                            task_id: task_id.to_string(),
                            timestamp: timestamp.to_string(),
                            payload: FleetWorkerEventPayload::Interrupted {
                                signal: Some(signal.to_string()),
                            },
                            extra: BTreeMap::new(),
                        },
                    });
                    next_seq += 1;
                }
                records.push(FleetLedgerRecord::EventAppended {
                    event: FleetWorkerEvent {
                        seq: next_seq,
                        run_id: run_id.clone(),
                        worker_id: worker_id.to_string(),
                        task_id: task_id.to_string(),
                        timestamp: timestamp.to_string(),
                        payload: FleetWorkerEventPayload::Cancelled {
                            cancelled_by: cancelled_by.map(str::to_string),
                        },
                        extra: BTreeMap::new(),
                    },
                });
                self.append_records_unlocked(&records)?;
            } else {
                self.append_record_unlocked(&FleetLedgerRecord::TaskCompletedOrFailed {
                    run_id: run_id.clone(),
                    task_id: task_id.to_string(),
                    worker_id: String::new(),
                    timestamp: timestamp.to_string(),
                    status: FleetTaskLedgerStatus::Cancelled,
                })?;
            }
            Ok(true)
        })
    }

    pub fn heartbeat(
        &self,
        worker_id: &str,
        timestamp: &str,
        cpu_percent: Option<f32>,
        memory_mb: Option<u64>,
    ) -> Result<()> {
        self.append_record(&FleetLedgerRecord::Heartbeat {
            worker_id: worker_id.to_string(),
            timestamp: timestamp.to_string(),
            cpu_percent,
            memory_mb,
        })
    }

    pub fn record_receipt(&self, receipt: FleetReceipt) -> Result<()> {
        self.append_record(&FleetLedgerRecord::ReceiptRecorded {
            receipt: Box::new(receipt),
        })
    }

    /// Record the run's budget-exceeded alert exactly once and pause the
    /// run (R6, #5567). Returns true only for the recording transition; a
    /// non-breached or already-alerted run is a no-op.
    pub fn record_budget_exceeded_alert_once(
        &self,
        run_id: &FleetRunId,
        timestamp: &str,
    ) -> Result<bool> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let Some((total, ceiling)) = state.run_ceiling_breached(run_id) else {
                return Ok(false);
            };
            let key = alert_key(run_id, BUDGET_ALERT_TASK, None, BUDGET_ALERT_CHANNEL);
            if state.alerts.contains_key(&key) {
                return Ok(false);
            }
            tracing::warn!(
                target: "fleet",
                run = %run_id.0,
                total,
                ceiling,
                "fleet run crossed its usage ceiling; pausing run and refusing new admissions"
            );
            self.append_record_unlocked(&FleetLedgerRecord::AlertSent {
                run_id: run_id.clone(),
                task_id: BUDGET_ALERT_TASK.to_string(),
                channel: BUDGET_ALERT_CHANNEL.to_string(),
                timestamp: timestamp.to_string(),
                worker_id: None,
                attempt: None,
                seq: None,
            })?;
            self.append_record_unlocked(&FleetLedgerRecord::RunStatusChanged {
                run_id: run_id.clone(),
                status: FleetRunStatus::Paused,
                timestamp: timestamp.to_string(),
            })?;
            Ok(true)
        })
    }

    pub fn record_alert(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        channel: &str,
        timestamp: &str,
    ) -> Result<()> {
        self.append_record(&FleetLedgerRecord::AlertSent {
            run_id: run_id.clone(),
            task_id: task_id.to_string(),
            channel: channel.to_string(),
            timestamp: timestamp.to_string(),
            worker_id: None,
            attempt: None,
            seq: None,
        })
    }

    /// Record one restart-exhaustion alert for one exact failed attempt.
    /// The audit marker and inspectable escalation are represented by one JSONL
    /// record, so competing schedulers and crash recovery cannot duplicate it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_failed_attempt_alert_once(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        expected_attempts: u32,
        channel_label: &str,
        channel_key: &str,
        timestamp: &str,
    ) -> Result<bool> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            let task_key = task_key(&run_id.0, task_id);
            let Some(task) = state.tasks.get(&task_key) else {
                return Ok(false);
            };
            if task.status != FleetTaskLedgerStatus::Failed
                || task.leased_to.as_deref() != Some(worker_id)
                || task.entry.attempts != expected_attempts
            {
                return Ok(false);
            }
            let exact_alert_key = alert_key(run_id, task_id, Some(expected_attempts), channel_key);
            // Pre-attempt ledgers stored only the display label. Replay infers
            // their attempt, so suppress that same delivery without treating a
            // legacy attempt as a permanent block on future retries.
            let legacy_alert_key =
                alert_key(run_id, task_id, Some(expected_attempts), channel_label);
            if state.alerts.contains_key(&exact_alert_key)
                || state.alerts.contains_key(&legacy_alert_key)
            {
                return Ok(false);
            }
            let lifecycle_key = event_key(worker_id, &run_id.0, task_id);
            let seq = state
                .latest_seq
                .get(&lifecycle_key)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            self.append_record_unlocked(&FleetLedgerRecord::AlertSent {
                run_id: run_id.clone(),
                task_id: task_id.to_string(),
                channel: channel_key.to_string(),
                timestamp: timestamp.to_string(),
                worker_id: Some(worker_id.to_string()),
                attempt: Some(expected_attempts),
                seq: Some(seq),
            })?;
            Ok(true)
        })
    }

    /// Replay the ledger and reconstruct current state. Malformed or partial
    /// lines are skipped so an interrupted write cannot corrupt earlier state.
    pub fn rebuild_state(&self) -> Result<FleetLedgerState> {
        self.with_read_lock(|| self.rebuild_state_unlocked())
    }

    /// Read one bounded page from the durable Fleet transition history.
    ///
    /// A cursor is the opaque digest of a ledger transition, not a global
    /// worker sequence. That distinction matters because worker `seq` values
    /// restart for each `(worker, task)` lifecycle. Recent cursors survive a
    /// process restart and normal appends. Ledger compaction may intentionally
    /// discard old history; in that case callers receive `CursorUnavailable`
    /// and must reload the current run projection instead of silently skipping
    /// an unknown gap.
    pub fn replay_events(
        &self,
        run_id: &FleetRunId,
        after: Option<&str>,
        limit: usize,
    ) -> std::result::Result<FleetEventReplay, FleetEventReplayError> {
        let (run_exists, all_events, history_compacted) = self
            .with_read_lock(|| self.scan_runtime_events_unlocked(run_id))
            .map_err(|error| FleetEventReplayError::Storage {
                message: error.to_string(),
            })?;
        if !run_exists {
            return Err(FleetEventReplayError::UnknownRun {
                run_id: run_id.0.clone(),
            });
        }

        let limit = limit.clamp(1, 1_000);
        let (events, has_more, history_truncated) = if let Some(after) = after {
            let Some(position) = all_events.iter().position(|event| event.cursor == after) else {
                return Err(FleetEventReplayError::CursorUnavailable {
                    run_id: run_id.0.clone(),
                });
            };
            let remaining = &all_events[position.saturating_add(1)..];
            (
                remaining.iter().take(limit).cloned().collect::<Vec<_>>(),
                remaining.len() > limit,
                false,
            )
        } else {
            let start = all_events.len().saturating_sub(limit);
            (
                all_events[start..].to_vec(),
                false,
                start > 0 || history_compacted,
            )
        };
        let next_cursor = events.last().map(|event| event.cursor.clone());
        Ok(FleetEventReplay {
            run_id: run_id.clone(),
            events,
            has_more,
            history_truncated,
            next_cursor,
        })
    }

    fn scan_runtime_events_unlocked(
        &self,
        run_filter: &FleetRunId,
    ) -> Result<(bool, Vec<FleetRuntimeEvent>, bool)> {
        let mut state = FleetLedgerState::default();
        let mut events = Vec::new();
        let mut replay_epoch = "legacy".to_string();
        let mut history_compacted = false;
        let file = match self.ledger_file.open_file() {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((false, events, history_compacted));
            }
            Err(error) => return Err(error).context("Opening Fleet ledger for replay"),
        };
        let reader = std::io::BufReader::new(file);
        for (line_no, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    tracing::warn!(
                        "fleet ledger line {} unreadable during event replay: {}",
                        line_no + 1,
                        error
                    );
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let record = match serde_json::from_str::<FleetLedgerRecord>(&line) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(
                        "fleet ledger line {} parse error during event replay (skipping): {}",
                        line_no + 1,
                        error
                    );
                    continue;
                }
            };
            if let FleetLedgerRecord::ReplayEpoch { epoch } = &record {
                replay_epoch.clone_from(epoch);
                history_compacted = true;
                apply_record(&mut state, record);
                continue;
            }
            for event in runtime_events_from_record(&record, &state, line_no, &replay_epoch) {
                if event.run_id == *run_filter {
                    events.push(event);
                }
            }
            apply_record(&mut state, record);
        }
        Ok((
            state.runs.contains_key(&run_filter.0),
            events,
            history_compacted,
        ))
    }

    fn rebuild_state_unlocked(&self) -> Result<FleetLedgerState> {
        let mut state = FleetLedgerState::default();
        let file = match self.ledger_file.open_file() {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(state),
            Err(error) => return Err(error).context("Opening Fleet ledger for state rebuild"),
        };
        let reader = std::io::BufReader::new(file);
        for (line_no, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(err) => {
                    tracing::warn!("fleet ledger line {} unreadable: {}", line_no + 1, err);
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let record: FleetLedgerRecord = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(err) => {
                    tracing::warn!(
                        "fleet ledger line {} parse error (skipping): {}",
                        line_no + 1,
                        err
                    );
                    continue;
                }
            };
            apply_record(&mut state, record);
        }
        Ok(state)
    }

    /// Claim the next available inbox task for `worker_id`. Returns the
    /// enqueued entry and appends a lease record.
    pub fn claim_next(
        &self,
        worker_id: &str,
        _worker_capabilities: &[String],
        timestamp: &str,
    ) -> Result<Option<FleetInboxEntry>> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            if state.tasks.values().any(|candidate| {
                candidate.status == FleetTaskLedgerStatus::Leased
                    && candidate.leased_to.as_deref() == Some(worker_id)
            }) {
                return Ok(None);
            }
            // Find oldest enqueued task whose task spec (if known) matches worker
            // capabilities. For now, tasks without specs match everything.
            let candidate = state
                .tasks
                .values()
                .filter(|t| matches!(t.status, FleetTaskLedgerStatus::Enqueued))
                .map(|t| &t.entry)
                .min_by_key(|e| (e.priority, e.enqueued_at.clone()))
                .cloned();
            let Some(entry) = candidate else {
                return Ok(None);
            };
            self.append_record_unlocked(&FleetLedgerRecord::TaskLeased {
                run_id: entry.run_id.clone(),
                task_id: entry.task_id.clone(),
                worker_id: worker_id.to_string(),
                leased_at: timestamp.to_string(),
                lease_expires_at: None,
            })?;
            Ok(Some(entry))
        })
    }

    /// Compact the ledger by rewriting only the records needed to reconstruct
    /// current state. This truncates history but preserves run/task/event
    /// metadata and receipts.
    pub fn compact(&self) -> Result<()> {
        self.compact_with_snapshot_hook(|| {})
    }

    fn compact_with_snapshot_hook(&self, after_snapshot: impl FnOnce()) -> Result<()> {
        self.with_write_lock(|| {
            let state = self.rebuild_state_unlocked()?;
            after_snapshot();
            let mut lines = vec![serde_json::to_string(&FleetLedgerRecord::ReplayEpoch {
                epoch: uuid::Uuid::new_v4().simple().to_string(),
            })?];
            let mut terminal_lines = Vec::new();
            let mut lifecycle_lines = Vec::new();
            for run in state.runs.values() {
                lines.push(serde_json::to_string(&FleetLedgerRecord::RunCreated {
                    run: Box::new(run.clone()),
                })?);
                if let Some(status) = state.run_status_overrides.get(&run.id.0) {
                    lines.push(serde_json::to_string(
                        &FleetLedgerRecord::RunStatusChanged {
                            run_id: run.id.clone(),
                            status: status.clone(),
                            timestamp: run.updated_at.clone().unwrap_or_default(),
                        },
                    )?);
                }
            }
            for task in state.tasks.values() {
                let mut enqueued_entry = task.entry.clone();
                if task.leased_to.is_some() {
                    // Replaying the retained TaskLeased record increments the
                    // attempt counter, so checkpoint the value immediately
                    // before that transition instead of inventing an attempt
                    // on every compaction.
                    enqueued_entry.attempts = enqueued_entry.attempts.saturating_sub(1);
                    enqueued_entry.lease_deadline = None;
                }
                lines.push(serde_json::to_string(&FleetLedgerRecord::TaskEnqueued {
                    entry: enqueued_entry,
                })?);
                if let Some(worker) = &task.leased_to {
                    lines.push(serde_json::to_string(&FleetLedgerRecord::TaskLeased {
                        run_id: task.entry.run_id.clone(),
                        task_id: task.entry.task_id.clone(),
                        worker_id: worker.clone(),
                        leased_at: task.leased_at.clone().unwrap_or_default(),
                        lease_expires_at: task.entry.lease_deadline.clone(),
                    })?);
                }
                if matches!(
                    task.status,
                    FleetTaskLedgerStatus::Completed
                        | FleetTaskLedgerStatus::Failed
                        | FleetTaskLedgerStatus::Cancelled
                ) {
                    terminal_lines.push(serde_json::to_string(
                        &FleetLedgerRecord::TaskCompletedOrFailed {
                            run_id: task.entry.run_id.clone(),
                            task_id: task.entry.task_id.clone(),
                            worker_id: task.leased_to.clone().unwrap_or_default(),
                            timestamp: task.completed_at.clone().unwrap_or_default(),
                            status: task.status,
                        },
                    )?);
                }
                lifecycle_lines.push(serde_json::to_string(
                    &FleetLedgerRecord::TaskLifecycleCheckpoint {
                        run_id: task.entry.run_id.clone(),
                        task_id: task.entry.task_id.clone(),
                        lifecycle_seq: task.lifecycle_seq,
                    },
                )?);
            }
            for alert in state.alerts.values() {
                lines.push(serde_json::to_string(&FleetLedgerRecord::AlertSent {
                    run_id: alert.run_id.clone(),
                    task_id: alert.task_id.clone(),
                    channel: alert.channel.clone(),
                    timestamp: alert.timestamp.clone(),
                    worker_id: alert.worker_id.clone(),
                    attempt: alert.attempt,
                    seq: alert.seq,
                })?);
            }
            let mut compacted_events = BTreeMap::new();
            for event in state.latest_events.values() {
                compacted_events.insert(compact_event_key(event), event.clone());
            }
            for event in state.artifact_events.values() {
                compacted_events.insert(compact_event_key(event), event.clone());
            }
            for event in state.restarted_events.values() {
                compacted_events.insert(compact_event_key(event), event.clone());
            }
            for event in state.escalated_events.values() {
                compacted_events.insert(compact_event_key(event), event.clone());
            }
            let mut compacted_events = compacted_events.into_values().collect::<Vec<_>>();
            compacted_events.sort_by(|left, right| {
                left.worker_id
                    .cmp(&right.worker_id)
                    .then_with(|| left.run_id.0.cmp(&right.run_id.0))
                    .then_with(|| left.task_id.cmp(&right.task_id))
                    .then_with(|| left.seq.cmp(&right.seq))
            });
            for event in compacted_events {
                lines.push(serde_json::to_string(&FleetLedgerRecord::EventAppended {
                    event,
                })?);
            }
            for (key, seq) in &state.latest_seq {
                let Some(owner) = state.sequence_owners.get(key) else {
                    continue;
                };
                lines.push(serde_json::to_string(
                    &FleetLedgerRecord::EventSequenceCheckpoint {
                        run_id: owner.run_id.clone(),
                        worker_id: owner.worker_id.clone(),
                        task_id: owner.task_id.clone(),
                        seq: *seq,
                    },
                )?);
            }
            for (worker_id, heartbeat) in &state.heartbeats {
                lines.push(serde_json::to_string(&FleetLedgerRecord::Heartbeat {
                    worker_id: worker_id.clone(),
                    timestamp: heartbeat.timestamp.clone(),
                    cpu_percent: heartbeat.cpu_percent,
                    memory_mb: heartbeat.memory_mb,
                })?);
            }
            // Terminal status is the final task-state projection. In
            // particular, a verifier may override a successful process exit to
            // Failed. Emit these records after retained worker events so replay
            // cannot let an earlier Completed event erase that override.
            lines.extend(terminal_lines);
            // Lifecycle checkpoints follow every reconstructed task-state
            // transition so replay ends at the exact pre-compaction sequence.
            lines.extend(lifecycle_lines);
            for receipt in state.receipts.values() {
                lines.push(serde_json::to_string(
                    &FleetLedgerRecord::ReceiptRecorded {
                        receipt: Box::new(receipt.clone()),
                    },
                )?);
            }
            let mut contents = lines.join("\n");
            if !contents.is_empty() {
                contents.push('\n');
            }
            self.ledger_file
                .replace(contents.as_bytes())
                .with_context(|| {
                    format!(
                        "atomically compacting Fleet ledger {}",
                        self.ledger_path.display()
                    )
                })?;
            Ok(())
        })
    }
}

fn runtime_events_from_record(
    record: &FleetLedgerRecord,
    state: &FleetLedgerState,
    ledger_ordinal: usize,
    replay_epoch: &str,
) -> Vec<FleetRuntimeEvent> {
    match record {
        FleetLedgerRecord::ReplayEpoch { .. } => Vec::new(),
        FleetLedgerRecord::RunCreated { run } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(&run.id, "run_created", ledger_ordinal, replay_epoch),
            event: "fleet.run.created".to_string(),
            run_id: run.id.clone(),
            worker_id: None,
            task_id: None,
            timestamp: Some(run.created_at.clone()),
            worker_seq: None,
            payload: json!({
                "target": run.target,
                "workflow": run.workflow,
                "roles": run.roles,
                "task_count": run.task_specs.len(),
                "worker_count": run.worker_specs.len(),
            }),
        }],
        FleetLedgerRecord::RunStatusChanged {
            run_id,
            status,
            timestamp,
        } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(run_id, "run_status", ledger_ordinal, replay_epoch),
            event: "fleet.run.status_changed".to_string(),
            run_id: run_id.clone(),
            worker_id: None,
            task_id: None,
            timestamp: Some(timestamp.clone()),
            worker_seq: None,
            payload: json!({ "status": status }),
        }],
        FleetLedgerRecord::TaskEnqueued { entry } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(
                &entry.run_id,
                "task_enqueued",
                ledger_ordinal,
                replay_epoch,
            ),
            event: "fleet.task.enqueued".to_string(),
            run_id: entry.run_id.clone(),
            worker_id: None,
            task_id: Some(entry.task_id.clone()),
            timestamp: Some(entry.enqueued_at.clone()),
            worker_seq: None,
            payload: json!({
                "priority": entry.priority,
                "attempts": entry.attempts,
            }),
        }],
        FleetLedgerRecord::TaskLeased {
            run_id,
            task_id,
            worker_id,
            leased_at,
            lease_expires_at,
        } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(run_id, "task_leased", ledger_ordinal, replay_epoch),
            event: "fleet.task.leased".to_string(),
            run_id: run_id.clone(),
            worker_id: Some(worker_id.clone()),
            task_id: Some(task_id.clone()),
            timestamp: Some(leased_at.clone()),
            worker_seq: None,
            payload: json!({ "lease_expires_at": lease_expires_at }),
        }],
        FleetLedgerRecord::TaskCompletedOrFailed {
            run_id,
            task_id,
            worker_id,
            timestamp,
            status,
        } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(run_id, "task_terminal", ledger_ordinal, replay_epoch),
            event: "fleet.task.terminal".to_string(),
            run_id: run_id.clone(),
            worker_id: (!worker_id.is_empty()).then(|| worker_id.clone()),
            task_id: Some(task_id.clone()),
            timestamp: Some(timestamp.clone()),
            worker_seq: None,
            payload: json!({ "status": status }),
        }],
        FleetLedgerRecord::TaskAttemptFinalized { event, receipt, .. } => vec![
            runtime_worker_event("worker", ledger_ordinal, replay_epoch, event),
            runtime_receipt_event("receipt", ledger_ordinal, replay_epoch, receipt),
        ],
        FleetLedgerRecord::EventAppended { event } => {
            vec![runtime_worker_event(
                "worker",
                ledger_ordinal,
                replay_epoch,
                event,
            )]
        }
        FleetLedgerRecord::Heartbeat {
            worker_id,
            timestamp,
            cpu_percent,
            memory_mb,
        } => active_task_for_replay_worker(state, worker_id)
            .map(|task| FleetRuntimeEvent {
                cursor: fleet_runtime_cursor(
                    &task.entry.run_id,
                    "heartbeat",
                    ledger_ordinal,
                    replay_epoch,
                ),
                event: "fleet.worker.heartbeat".to_string(),
                run_id: task.entry.run_id.clone(),
                worker_id: Some(worker_id.clone()),
                task_id: Some(task.entry.task_id.clone()),
                timestamp: Some(timestamp.clone()),
                worker_seq: None,
                payload: json!({
                    "cpu_percent": cpu_percent,
                    "memory_mb": memory_mb,
                }),
            })
            .into_iter()
            .collect(),
        FleetLedgerRecord::ReceiptRecorded { receipt } => {
            vec![runtime_receipt_event(
                "receipt",
                ledger_ordinal,
                replay_epoch,
                receipt,
            )]
        }
        FleetLedgerRecord::AlertSent {
            run_id,
            task_id,
            timestamp,
            worker_id,
            attempt,
            ..
        } => vec![FleetRuntimeEvent {
            cursor: fleet_runtime_cursor(run_id, "alert", ledger_ordinal, replay_epoch),
            event: "fleet.alert.sent".to_string(),
            run_id: run_id.clone(),
            worker_id: worker_id.clone(),
            task_id: Some(task_id.clone()),
            timestamp: Some(timestamp.clone()),
            worker_seq: None,
            payload: json!({ "attempt": attempt }),
        }],
        FleetLedgerRecord::TaskLifecycleCheckpoint { .. }
        | FleetLedgerRecord::EventSequenceCheckpoint { .. } => Vec::new(),
    }
}

fn active_task_for_replay_worker<'a>(
    state: &'a FleetLedgerState,
    worker_id: &str,
) -> Option<&'a FleetTaskState> {
    state.tasks.values().find(|task| {
        task.status == FleetTaskLedgerStatus::Leased && task.leased_to.as_deref() == Some(worker_id)
    })
}

fn runtime_worker_event(
    cursor_kind: &str,
    ledger_ordinal: usize,
    replay_epoch: &str,
    event: &FleetWorkerEvent,
) -> FleetRuntimeEvent {
    let (event_name, payload) = privacy_bounded_worker_payload(&event.payload);
    FleetRuntimeEvent {
        cursor: fleet_runtime_cursor(&event.run_id, cursor_kind, ledger_ordinal, replay_epoch),
        event: event_name,
        run_id: event.run_id.clone(),
        worker_id: Some(event.worker_id.clone()),
        task_id: Some(event.task_id.clone()),
        timestamp: Some(event.timestamp.clone()),
        worker_seq: Some(event.seq),
        payload,
    }
}

fn runtime_receipt_event(
    cursor_kind: &str,
    ledger_ordinal: usize,
    replay_epoch: &str,
    receipt: &FleetReceipt,
) -> FleetRuntimeEvent {
    let score = receipt.score.as_ref().map(|score| {
        json!({
            "value": score.value,
            "max": score.max,
        })
    });
    FleetRuntimeEvent {
        cursor: fleet_runtime_cursor(&receipt.run_id, cursor_kind, ledger_ordinal, replay_epoch),
        event: "fleet.task.receipt_recorded".to_string(),
        run_id: receipt.run_id.clone(),
        worker_id: Some(receipt.worker_id.clone()),
        task_id: Some(receipt.task_id.clone()),
        timestamp: Some(receipt.completed_at.clone()),
        worker_seq: receipt.terminal_seq,
        payload: json!({
            "attempt": receipt.attempt,
            "result": receipt.result,
            "failure_kind": receipt.failure_kind,
            "score": score,
            "artifact_kinds": receipt.artifacts.iter().map(|artifact| &artifact.kind).collect::<Vec<_>>(),
        }),
    }
}

fn privacy_bounded_worker_payload(payload: &FleetWorkerEventPayload) -> (String, Value) {
    let state = match payload {
        FleetWorkerEventPayload::Queued => "queued",
        FleetWorkerEventPayload::Leased { .. } => "leased",
        FleetWorkerEventPayload::Starting => "starting",
        FleetWorkerEventPayload::Running => "running",
        FleetWorkerEventPayload::ModelWait { .. } => "model_wait",
        FleetWorkerEventPayload::RunningTool { .. } => "running_tool",
        FleetWorkerEventPayload::WorkflowEvent { .. } => "workflow_event",
        FleetWorkerEventPayload::Heartbeat { .. } => "heartbeat",
        FleetWorkerEventPayload::UsageReport { .. } => "usage_report",
        FleetWorkerEventPayload::Artifact(_) => "artifact",
        FleetWorkerEventPayload::Completed { .. } => "completed",
        FleetWorkerEventPayload::Failed { .. } => "failed",
        FleetWorkerEventPayload::Cancelled { .. } => "cancelled",
        FleetWorkerEventPayload::Interrupted { .. } => "interrupted",
        FleetWorkerEventPayload::Stale { .. } => "stale",
        FleetWorkerEventPayload::Restarted { .. } => "restarted",
        FleetWorkerEventPayload::Escalated { .. } => "escalated",
    };
    let value = match payload {
        FleetWorkerEventPayload::Leased { lease_expires_at } => {
            json!({ "state": state, "lease_expires_at": lease_expires_at })
        }
        FleetWorkerEventPayload::ModelWait { model } => {
            json!({ "state": state, "model": model })
        }
        FleetWorkerEventPayload::RunningTool { tool, .. } => {
            json!({ "state": state, "tool": tool })
        }
        FleetWorkerEventPayload::WorkflowEvent {
            workflow_run_id,
            event,
        } => json!({
            "state": state,
            "workflow_run_id": workflow_run_id,
            "event_type": event.get("type").and_then(Value::as_str),
        }),
        FleetWorkerEventPayload::Heartbeat {
            cpu_percent,
            memory_mb,
        } => json!({
            "state": state,
            "cpu_percent": cpu_percent,
            "memory_mb": memory_mb,
        }),
        FleetWorkerEventPayload::UsageReport {
            input_tokens,
            output_tokens,
        } => json!({
            "state": state,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }),
        FleetWorkerEventPayload::Artifact(artifact) => json!({
            "state": state,
            "kind": artifact.kind,
            "mime_type": artifact.mime_type,
            "size_bytes": artifact.size_bytes,
        }),
        FleetWorkerEventPayload::Completed { exit_code, .. } => {
            json!({ "state": state, "exit_code": exit_code })
        }
        FleetWorkerEventPayload::Failed {
            reason,
            recoverable,
        } => json!({
            "state": state,
            "reason": redact_fleet_event_text(reason),
            "recoverable": recoverable,
        }),
        FleetWorkerEventPayload::Stale { last_heartbeat_at } => {
            json!({ "state": state, "last_heartbeat_at": last_heartbeat_at })
        }
        FleetWorkerEventPayload::Restarted { restart_count } => {
            json!({ "state": state, "restart_count": restart_count })
        }
        FleetWorkerEventPayload::Escalated { channel, .. } => {
            json!({ "state": state, "channel": channel })
        }
        FleetWorkerEventPayload::Queued
        | FleetWorkerEventPayload::Starting
        | FleetWorkerEventPayload::Running
        | FleetWorkerEventPayload::Cancelled { .. }
        | FleetWorkerEventPayload::Interrupted { .. } => json!({ "state": state }),
    };
    (format!("fleet.worker.{state}"), value)
}

fn redact_fleet_event_text(value: &str) -> String {
    // The shared redactor deliberately treats one whole line as an assignment.
    // Failure diagnostics often prefix an inline assignment (`provider failed:
    // api_key=...`), so make a second token-level pass before exposing the
    // bounded preview. Whitespace is normalized because this is a status
    // summary, not the forensic worker log.
    let redacted = bearer_secret_pattern()
        .replace_all(value, "Bearer [redacted]")
        .into_owned();
    let redacted = inline_secret_assignment_pattern()
        .replace_all(&redacted, "$1$2[redacted]")
        .into_owned();
    let redacted = codewhale_config::persistence::redact_secrets(&redacted);
    let redacted = redacted
        .split_whitespace()
        .map(codewhale_config::persistence::redact_secrets)
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = redacted.chars();
    let preview = chars.by_ref().take(1_000).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn fleet_runtime_cursor(
    run_id: &FleetRunId,
    kind: &str,
    ledger_ordinal: usize,
    replay_epoch: &str,
) -> String {
    // Cursor material is deliberately metadata-only: hashing a raw ledger
    // record would let a caller correlate or guess secret-bearing diagnostic
    // text. The generation rotates on compaction, while the physical JSONL
    // ordinal remains stable across ordinary appends and process restarts.
    let material = format!(
        "fleet-event-v1\0{replay_epoch}\0{}\0{ledger_ordinal}\0{kind}",
        run_id.0
    );
    format!(
        "fev1_{}_{}",
        codewhale_runtime::hashing::sha256_hex(material.as_bytes()),
        kind
    )
}

fn task_key(run_id: &str, task_id: &str) -> String {
    format!("{run_id}:{task_id}")
}

fn event_key(worker_id: &str, run_id: &str, task_id: &str) -> String {
    format!("{worker_id}:{run_id}:{task_id}")
}

fn alert_key(
    run_id: &FleetRunId,
    task_id: &str,
    attempt: Option<u32>,
    channel: &str,
) -> (String, String, Option<u32>, String) {
    (
        run_id.0.clone(),
        task_id.to_string(),
        attempt,
        channel.to_string(),
    )
}

fn alert_channel_label(channel_key: &str) -> &str {
    channel_key
        .rsplit_once('#')
        .filter(|(_, ordinal)| ordinal.parse::<usize>().is_ok())
        .map_or(channel_key, |(label, _)| label)
}

fn next_worker_event(
    state: &FleetLedgerState,
    run_id: &FleetRunId,
    worker_id: &str,
    task_id: &str,
    timestamp: &str,
    payload: FleetWorkerEventPayload,
) -> FleetWorkerEvent {
    let key = event_key(worker_id, &run_id.0, task_id);
    FleetWorkerEvent {
        seq: state.latest_seq.get(&key).copied().unwrap_or(0) + 1,
        run_id: run_id.clone(),
        worker_id: worker_id.to_string(),
        task_id: task_id.to_string(),
        timestamp: timestamp.to_string(),
        payload,
        extra: BTreeMap::new(),
    }
}

fn compact_event_key(event: &FleetWorkerEvent) -> String {
    format!(
        "{}:{}:{}:{}",
        event.worker_id, event.run_id.0, event.task_id, event.seq
    )
}

fn mark_task_terminal(
    state: &mut FleetLedgerState,
    run_id: &FleetRunId,
    task_id: &str,
    worker_id: &str,
    timestamp: &str,
    status: FleetTaskLedgerStatus,
) {
    let key = task_key(&run_id.0, task_id);
    if let Some(task) = state.tasks.get_mut(&key) {
        if task.status != status {
            task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
        }
        task.status = status;
        if !worker_id.is_empty() {
            task.leased_to = Some(worker_id.to_string());
        }
        task.completed_at = Some(timestamp.to_string());
    }
}

fn artifact_event_key(event: &FleetWorkerEvent, artifact: &FleetArtifactRef) -> String {
    format!(
        "{}:{}:{}:{}",
        event.worker_id,
        event.run_id.0,
        event.task_id,
        artifact.path.display()
    )
}

fn apply_record(state: &mut FleetLedgerState, record: FleetLedgerRecord) {
    match record {
        FleetLedgerRecord::ReplayEpoch { .. } => {}
        FleetLedgerRecord::RunCreated { run } => {
            state.runs.insert(run.id.0.clone(), *run);
        }
        FleetLedgerRecord::RunStatusChanged {
            run_id,
            status,
            timestamp: _,
        } => {
            state.run_status_overrides.insert(run_id.0, status);
        }
        FleetLedgerRecord::TaskEnqueued { entry } => {
            let key = task_key(&entry.run_id.0, &entry.task_id);
            state.tasks.entry(key).or_insert_with(|| FleetTaskState {
                entry,
                status: FleetTaskLedgerStatus::Enqueued,
                lifecycle_seq: 1,
                leased_to: None,
                leased_at: None,
                completed_at: None,
            });
        }
        FleetLedgerRecord::TaskLeased {
            run_id,
            task_id,
            worker_id,
            leased_at,
            lease_expires_at,
        } => {
            let key = task_key(&run_id.0, &task_id);
            if let Some(task) = state.tasks.get_mut(&key) {
                if task.status != FleetTaskLedgerStatus::Leased
                    || task.leased_to.as_deref() != Some(worker_id.as_str())
                    || task.leased_at.as_deref() != Some(leased_at.as_str())
                {
                    task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
                }
                task.status = FleetTaskLedgerStatus::Leased;
                task.leased_to = Some(worker_id);
                task.leased_at = Some(leased_at);
                task.entry.lease_deadline = lease_expires_at;
                task.entry.attempts = task.entry.attempts.saturating_add(1);
            }
        }
        FleetLedgerRecord::TaskCompletedOrFailed {
            run_id,
            task_id,
            worker_id,
            timestamp,
            status,
        } => {
            mark_task_terminal(state, &run_id, &task_id, &worker_id, &timestamp, status);
        }
        FleetLedgerRecord::TaskLifecycleCheckpoint {
            run_id,
            task_id,
            lifecycle_seq,
        } => {
            if let Some(task) = state.tasks.get_mut(&task_key(&run_id.0, &task_id)) {
                task.lifecycle_seq = task.lifecycle_seq.max(lifecycle_seq.max(1));
            }
        }
        FleetLedgerRecord::TaskAttemptFinalized {
            event,
            final_status,
            receipt,
        } => {
            let run_id = event.run_id.clone();
            let task_id = event.task_id.clone();
            let worker_id = event.worker_id.clone();
            let timestamp = event.timestamp.clone();
            apply_record(state, FleetLedgerRecord::EventAppended { event });
            if let Some(status) = final_status {
                mark_task_terminal(state, &run_id, &task_id, &worker_id, &timestamp, status);
            }
            let key = task_key(&receipt.run_id.0, &receipt.task_id);
            state.receipts.insert(key, *receipt);
        }
        FleetLedgerRecord::EventAppended { event } => {
            let latest_event_key = event_key(&event.worker_id, &event.run_id.0, &event.task_id);
            state.sequence_owners.insert(
                latest_event_key.clone(),
                FleetEventSequenceOwner {
                    run_id: event.run_id.clone(),
                    worker_id: event.worker_id.clone(),
                    task_id: event.task_id.clone(),
                },
            );
            let task_is_terminal = state
                .tasks
                .get(&task_key(&event.run_id.0, &event.task_id))
                .is_some_and(|task| {
                    matches!(
                        task.status,
                        FleetTaskLedgerStatus::Completed
                            | FleetTaskLedgerStatus::Failed
                            | FleetTaskLedgerStatus::Cancelled
                    )
                });
            let regresses_terminal_state = task_is_terminal
                && matches!(
                    &event.payload,
                    FleetWorkerEventPayload::Leased { .. }
                        | FleetWorkerEventPayload::Artifact(_)
                        | FleetWorkerEventPayload::ModelWait { .. }
                        | FleetWorkerEventPayload::RunningTool { .. }
                        | FleetWorkerEventPayload::WorkflowEvent { .. }
                        | FleetWorkerEventPayload::Heartbeat { .. }
                        | FleetWorkerEventPayload::UsageReport { .. }
                        | FleetWorkerEventPayload::Starting
                        | FleetWorkerEventPayload::Running
                        | FleetWorkerEventPayload::Stale { .. }
                        | FleetWorkerEventPayload::Restarted { .. }
                        | FleetWorkerEventPayload::Interrupted { .. }
                );
            if state
                .latest_seq
                .get(&latest_event_key)
                .copied()
                .is_none_or(|seq| event.seq > seq)
            {
                state.latest_seq.insert(latest_event_key.clone(), event.seq);
                if !regresses_terminal_state {
                    state.latest_events.insert(latest_event_key, event.clone());
                }
            }
            if let FleetWorkerEventPayload::Artifact(artifact) = &event.payload {
                state
                    .artifact_events
                    .insert(artifact_event_key(&event, artifact), event.clone());
            }
            if matches!(&event.payload, FleetWorkerEventPayload::Restarted { .. }) {
                state.restarted_events.insert(
                    event_key(&event.worker_id, &event.run_id.0, &event.task_id),
                    event.clone(),
                );
            }
            if matches!(&event.payload, FleetWorkerEventPayload::Escalated { .. }) {
                state.escalated_events.insert(
                    event_key(&event.worker_id, &event.run_id.0, &event.task_id),
                    event.clone(),
                );
            }
            // Usage always counts, even when a late receipt lands after the
            // task went terminal — the provider billed it either way.
            if let FleetWorkerEventPayload::UsageReport {
                input_tokens,
                output_tokens,
            } = &event.payload
            {
                let usage = state.run_usage.entry(event.run_id.0.clone()).or_default();
                usage.input_tokens = usage.input_tokens.saturating_add(*input_tokens);
                usage.output_tokens = usage.output_tokens.saturating_add(*output_tokens);
            }
            // Derive worker status from lifecycle events. A late stream event
            // must never resurrect a terminal task after an out-of-process
            // cancellation raced the foreground executor's final drain.
            if regresses_terminal_state {
                return;
            }
            match &event.payload {
                FleetWorkerEventPayload::Leased { .. }
                | FleetWorkerEventPayload::Restarted { .. }
                | FleetWorkerEventPayload::ModelWait { .. }
                | FleetWorkerEventPayload::RunningTool { .. }
                | FleetWorkerEventPayload::WorkflowEvent { .. }
                | FleetWorkerEventPayload::Heartbeat { .. }
                | FleetWorkerEventPayload::UsageReport { .. }
                | FleetWorkerEventPayload::Starting
                | FleetWorkerEventPayload::Running => {
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Busy);
                }
                FleetWorkerEventPayload::Interrupted { .. } => {
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Draining);
                }
                FleetWorkerEventPayload::Stale { .. } => {
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Unhealthy);
                }
                FleetWorkerEventPayload::Completed { .. } => {
                    mark_task_terminal(
                        state,
                        &event.run_id,
                        &event.task_id,
                        &event.worker_id,
                        &event.timestamp,
                        FleetTaskLedgerStatus::Completed,
                    );
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Online);
                }
                FleetWorkerEventPayload::Failed { .. } => {
                    mark_task_terminal(
                        state,
                        &event.run_id,
                        &event.task_id,
                        &event.worker_id,
                        &event.timestamp,
                        FleetTaskLedgerStatus::Failed,
                    );
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Online);
                }
                FleetWorkerEventPayload::Cancelled { .. } => {
                    mark_task_terminal(
                        state,
                        &event.run_id,
                        &event.task_id,
                        &event.worker_id,
                        &event.timestamp,
                        FleetTaskLedgerStatus::Cancelled,
                    );
                    state
                        .workers
                        .insert(event.worker_id.clone(), FleetWorkerStatus::Online);
                }
                _ => {}
            }
        }
        FleetLedgerRecord::EventSequenceCheckpoint {
            run_id,
            worker_id,
            task_id,
            seq,
        } => {
            let key = event_key(&worker_id, &run_id.0, &task_id);
            state.sequence_owners.insert(
                key.clone(),
                FleetEventSequenceOwner {
                    run_id,
                    worker_id,
                    task_id,
                },
            );
            state
                .latest_seq
                .entry(key)
                .and_modify(|current| *current = (*current).max(seq))
                .or_insert(seq);
        }
        FleetLedgerRecord::Heartbeat {
            worker_id,
            timestamp,
            cpu_percent,
            memory_mb,
        } => {
            state.heartbeats.insert(
                worker_id.clone(),
                FleetHeartbeatState {
                    timestamp,
                    cpu_percent,
                    memory_mb,
                },
            );
            if state
                .workers
                .get(&worker_id)
                .cloned()
                .unwrap_or(FleetWorkerStatus::Unknown)
                != FleetWorkerStatus::Busy
            {
                state.workers.insert(worker_id, FleetWorkerStatus::Online);
            }
        }
        FleetLedgerRecord::ReceiptRecorded { receipt } => {
            let key = task_key(&receipt.run_id.0, &receipt.task_id);
            state.receipts.insert(key, *receipt);
        }
        FleetLedgerRecord::AlertSent {
            run_id,
            task_id,
            channel,
            timestamp,
            worker_id,
            attempt,
            seq,
        } => {
            // Old AlertSent records had no attempt. They were appended after
            // terminalization, so the replay projection at this point carries
            // the exact attempt that delivered them. Normalize immediately;
            // compaction then upgrades the durable record too.
            let attempt = attempt.or_else(|| {
                state
                    .tasks
                    .get(&task_key(&run_id.0, &task_id))
                    .map(|task| task.entry.attempts)
                    .filter(|attempt| *attempt > 0)
            });
            let key = alert_key(&run_id, &task_id, attempt, &channel);
            let is_new = !state.alerts.contains_key(&key);
            state.alerts.entry(key).or_insert_with(|| FleetLedgerAlert {
                run_id: run_id.clone(),
                task_id: task_id.clone(),
                channel: channel.clone(),
                timestamp: timestamp.clone(),
                worker_id: worker_id.clone(),
                attempt,
                seq,
            });
            if is_new && let (Some(worker_id), Some(seq)) = (worker_id, seq) {
                apply_record(
                    state,
                    FleetLedgerRecord::EventAppended {
                        event: FleetWorkerEvent {
                            seq,
                            run_id,
                            worker_id,
                            task_id,
                            timestamp,
                            payload: FleetWorkerEventPayload::Escalated {
                                channel: alert_channel_label(&channel).to_string(),
                                alert_id: None,
                            },
                            extra: BTreeMap::new(),
                        },
                    },
                );
            }
        }
    }
}

fn sanitize_run_for_ledger(run: &FleetRun) -> FleetRun {
    let mut run = run.clone();
    for task in &mut run.task_specs {
        if let Some(policy) = &mut task.alert_policy {
            for channel in &mut policy.channels {
                match channel {
                    FleetAlertChannel::Slack { webhook } => {
                        webhook.url = webhook.url.as_ref().map(|_| "<redacted>".to_string());
                    }
                    FleetAlertChannel::Webhook { endpoint } => {
                        *endpoint = FleetAlertEndpoint {
                            url: endpoint.url.as_ref().map(|_| "<redacted>".to_string()),
                            url_ref: endpoint
                                .url_ref
                                .as_ref()
                                .map(|_| FleetSecretRef::new("<redacted>")),
                            secret_ref: endpoint
                                .secret_ref
                                .as_ref()
                                .map(|_| FleetSecretRef::new("<redacted>")),
                        };
                    }
                    FleetAlertChannel::PagerDuty { routing_key, .. } => {
                        *routing_key = "<redacted>".to_string();
                    }
                }
            }
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn ledger_append_wakes_subscribers() {
        let workspace = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        let appends = subscribe_fleet_ledger_appends(&fleet_ledger_path(workspace.path()));
        let notified = appends.notified();
        tokio::pin!(notified);
        let appended = tokio::task::spawn_blocking(move || {
            ledger.create_run(&sample_run("run-1")).unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), &mut notified)
            .await
            .expect("append should wake subscribers");
        appended.await.unwrap();
    }

    #[tokio::test]
    async fn ledger_wake_is_scoped_to_its_file() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let ledger = FleetLedger::open(first.path()).unwrap();
        let appends = subscribe_fleet_ledger_appends(&fleet_ledger_path(second.path()));
        ledger.create_run(&sample_run("run-1")).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), appends.notified())
                .await
                .is_err(),
            "an append must not wake another ledger's subscribers"
        );
    }

    /// The workspace lock sidecar is created through the confined, private
    /// file helper: owner-only, never umask-widened.
    #[cfg(unix)]
    #[test]
    fn ledger_lock_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let workspace = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&ledger.lock_path), 0o600);
        assert_eq!(mode(ledger.path()), 0o600);
    }

    #[test]
    fn ledger_rejects_replaced_lock_identity() {
        let workspace = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        std::fs::remove_file(&ledger.lock_path).unwrap();
        std::fs::write(&ledger.lock_path, b"").unwrap();
        assert!(ledger.rebuild_state().is_err());
        assert!(ledger.compact().is_err());
    }

    #[test]
    fn ledger_rejects_hard_linked_ledger_and_lock() {
        for name in ["fleet.jsonl", "fleet.lock"] {
            let workspace = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            std::fs::create_dir(workspace.path().join(".codewhale")).unwrap();
            let canary = outside.path().join("canary.txt");
            std::fs::write(&canary, b"OUTSIDE_SYNTHETIC_HARD_LINK_CANARY").unwrap();
            std::fs::hard_link(&canary, workspace.path().join(".codewhale").join(name)).unwrap();
            assert!(FleetLedger::open(workspace.path()).is_err());
            assert_eq!(
                std::fs::read(canary).unwrap(),
                b"OUTSIDE_SYNTHETIC_HARD_LINK_CANARY"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ledger_rejects_parent_and_final_linked_paths() {
        use std::os::unix::fs::symlink;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), workspace.path().join(".codewhale")).unwrap();
        assert!(FleetLedger::open(workspace.path()).is_err());
        assert!(!outside.path().join("fleet.jsonl").exists());
        assert!(!outside.path().join("fleet.lock").exists());
        for name in ["fleet.jsonl", "fleet.lock"] {
            for hard_link in [false, true] {
                let workspace = TempDir::new().unwrap();
                std::fs::create_dir(workspace.path().join(".codewhale")).unwrap();
                let outside = TempDir::new().unwrap();
                let canary = outside.path().join("canary.txt");
                std::fs::write(&canary, b"OUTSIDE_SYNTHETIC_LEDGER_CANARY").unwrap();
                let linked = workspace.path().join(".codewhale").join(name);
                if hard_link {
                    std::fs::hard_link(&canary, linked).unwrap();
                } else {
                    symlink(&canary, linked).unwrap();
                }
                assert!(
                    FleetLedger::open(workspace.path()).is_err(),
                    "accepted linked {name}"
                );
                assert_eq!(
                    std::fs::read(canary).unwrap(),
                    b"OUTSIDE_SYNTHETIC_LEDGER_CANARY"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn ledger_compaction_ignores_preexisting_temporary_links() {
        use std::os::unix::fs::symlink;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        let canary = outside.path().join("canary.txt");
        std::fs::write(&canary, b"OUTSIDE_SYNTHETIC_COMPACTION_CANARY").unwrap();
        symlink(&canary, ledger.path().with_extension(".tmp")).unwrap();
        ledger.compact().unwrap();
        assert_eq!(
            std::fs::read(canary).unwrap(),
            b"OUTSIDE_SYNTHETIC_COMPACTION_CANARY"
        );
        assert!(ledger.rebuild_state().unwrap().runs.is_empty());
        assert!(
            !std::fs::symlink_metadata(ledger.path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ledger_rejects_linked_ledger_and_replaced_lock_after_open() {
        use std::os::unix::fs::symlink;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        let canary = outside.path().join("canary.txt");
        std::fs::write(&canary, b"OUTSIDE_SYNTHETIC_LEDGER_CANARY").unwrap();
        std::fs::remove_file(ledger.path()).unwrap();
        symlink(&canary, ledger.path()).unwrap();
        assert!(ledger.compact().is_err());
        assert!(ledger.rebuild_state().is_err());
        assert_eq!(
            std::fs::read(&canary).unwrap(),
            b"OUTSIDE_SYNTHETIC_LEDGER_CANARY"
        );
        std::fs::remove_file(ledger.path()).unwrap();
        std::fs::write(ledger.path(), b"").unwrap();
        std::fs::remove_file(&ledger.lock_path).unwrap();
        std::fs::write(&ledger.lock_path, b"").unwrap();
        assert!(ledger.compact().is_err());
        assert!(ledger.rebuild_state().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ledger_uses_pinned_parent_after_workspace_path_swap() {
        use std::os::unix::fs::symlink;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ledger = FleetLedger::open(workspace.path()).unwrap();
        std::fs::rename(
            workspace.path().join(".codewhale"),
            workspace.path().join("retained"),
        )
        .unwrap();
        symlink(outside.path(), workspace.path().join(".codewhale")).unwrap();
        ledger.compact().unwrap();
        assert!(
            std::fs::read(workspace.path().join("retained/fleet.jsonl"))
                .unwrap()
                .starts_with(b"{\"record\":\"replay_epoch\"")
        );
        assert!(!outside.path().join("fleet.jsonl").exists());
        assert!(!outside.path().join("fleet.lock").exists());
    }

    fn sample_run(id: &str) -> FleetRun {
        FleetRun {
            id: FleetRunId::from(id),
            name: "smoke".to_string(),
            status: FleetRunStatus::Running,
            target: None,
            workflow: None,
            roles: Vec::new(),
            max_workers: None,
            usage_ceiling: None,
            task_specs: vec![],
            worker_specs: vec![],
            labels: BTreeMap::new(),
            security_policy: None,
            created_at: "2026-06-12T17:00:00Z".to_string(),
            updated_at: None,
            completed_at: None,
        }
    }

    fn sample_entry(run_id: &str, task_id: &str) -> FleetInboxEntry {
        FleetInboxEntry {
            run_id: FleetRunId::from(run_id),
            task_id: task_id.to_string(),
            priority: 0,
            enqueued_at: "2026-06-12T17:00:00Z".to_string(),
            lease_deadline: None,
            attempts: 0,
        }
    }

    #[test]
    fn usage_ceiling_breach_pauses_run_refuses_admissions_and_alerts_once() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let mut run = sample_run("budget-run");
        run.usage_ceiling = Some(codewhale_protocol::fleet::FleetUsageCeiling {
            max_total_tokens: 1_000,
        });
        ledger.create_run(&run).unwrap();
        ledger
            .enqueue(sample_entry("budget-run", "task-a"))
            .unwrap();
        ledger
            .enqueue(sample_entry("budget-run", "task-b"))
            .unwrap();

        // Under the ceiling, admission works.
        assert!(
            ledger
                .lease_task_if_enqueued(
                    &run.id,
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:00:01Z",
                    None,
                    None,
                )
                .unwrap()
        );

        // Usage receipts accumulate; the second crosses the 1 000 ceiling and
        // must pause the run and fire exactly one durable alert.
        for (ts, input, output) in [
            ("2026-06-12T17:00:02Z", 600, 0),
            ("2026-06-12T17:00:03Z", 300, 200),
        ] {
            ledger
                .append_event_next_seq(
                    &run.id,
                    "worker-1",
                    "task-a",
                    ts,
                    FleetWorkerEventPayload::UsageReport {
                        input_tokens: input,
                        output_tokens: output,
                    },
                )
                .unwrap();
        }
        let budget_alerts = |state: &FleetLedgerState| {
            state
                .alerts
                .keys()
                .filter(|(run, task, _, channel)| {
                    run == "budget-run"
                        && task == BUDGET_ALERT_TASK
                        && channel == BUDGET_ALERT_CHANNEL
                })
                .count()
        };
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.run_usage(&run.id).total_tokens(), 1_100);
        assert!(state.run_ceiling_breached(&run.id).is_some());
        assert_eq!(budget_alerts(&state), 1);
        assert_eq!(
            state.run_status_overrides["budget-run"],
            FleetRunStatus::Paused
        );

        // A later receipt cannot duplicate the alert…
        ledger
            .append_event_next_seq(
                &run.id,
                "worker-1",
                "task-a",
                "2026-06-12T17:00:04Z",
                FleetWorkerEventPayload::UsageReport {
                    input_tokens: 50,
                    output_tokens: 0,
                },
            )
            .unwrap();
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(budget_alerts(&state), 1);

        // …and the breached run admits nothing new.
        assert!(
            !ledger
                .lease_task_if_enqueued(
                    &run.id,
                    "task-b",
                    "worker-2",
                    "2026-06-12T17:00:05Z",
                    None,
                    None,
                )
                .unwrap()
        );
    }

    #[test]
    fn fleet_ledger_create_and_rebuild_run() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let run = sample_run("run-1");
        ledger.create_run(&run).unwrap();
        ledger
            .update_run_status(&run.id, FleetRunStatus::Completed, "2026-06-12T18:00:00Z")
            .unwrap();

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.runs.len(), 1);
        assert_eq!(
            state.run_status_overrides["run-1"],
            FleetRunStatus::Completed
        );
    }

    #[test]
    fn fleet_event_replay_is_durable_cursor_bounded_and_privacy_safe() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let mut run = sample_run("managed-run");
        run.target = Some(FleetRuntimeTarget::ThisComputer);
        run.workflow = Some(FleetWorkflowDescriptor {
            id: "release-check".to_string(),
            kind: FleetWorkflowKind::Parallel,
        });
        run.roles = vec!["reviewer".to_string()];
        ledger.create_run(&run).unwrap();
        ledger
            .update_run_status(&run.id, FleetRunStatus::Paused, "2026-06-12T17:00:10Z")
            .unwrap();
        ledger
            .update_run_status(&run.id, FleetRunStatus::Running, "2026-06-12T17:00:20Z")
            .unwrap();
        ledger
            .enqueue(sample_entry("managed-run", "task-a"))
            .unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 1,
                run_id: run.id.clone(),
                worker_id: "worker-1".to_string(),
                task_id: "task-a".to_string(),
                timestamp: "2026-06-12T17:01:00Z".to_string(),
                payload: FleetWorkerEventPayload::Artifact(FleetArtifactRef {
                    kind: FleetArtifactKind::Report,
                    path: PathBuf::from(".codewhale/private/full-report.md"),
                    checksum: Some("sha256:private-checksum".to_string()),
                    mime_type: Some("text/markdown".to_string()),
                    size_bytes: Some(42),
                }),
                extra: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .record_receipt(FleetReceipt {
                run_id: run.id.clone(),
                task_id: "task-a".to_string(),
                worker_id: "worker-1".to_string(),
                attempt: Some(1),
                terminal_seq: Some(2),
                completed_at: "2026-06-12T17:02:00Z".to_string(),
                result: FleetTaskResult::Fail,
                failure_kind: Some(FleetTaskFailureKind::Task),
                artifacts: vec![FleetArtifactRef {
                    kind: FleetArtifactKind::Report,
                    path: PathBuf::from(".codewhale/private/full-report.md"),
                    checksum: Some("sha256:private-checksum".to_string()),
                    mime_type: Some("text/markdown".to_string()),
                    size_bytes: Some(42),
                }],
                score: Some(FleetScore {
                    value: 0.0,
                    max: Some(1.0),
                    notes: Some("verifier note contained super-secret".to_string()),
                }),
                resolved_route: None,
                saved_session_id: None,
                effective_permissions: None,
            })
            .unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 2,
                run_id: run.id.clone(),
                worker_id: "worker-1".to_string(),
                task_id: "task-a".to_string(),
                timestamp: "2026-06-12T17:02:00Z".to_string(),
                payload: FleetWorkerEventPayload::Failed {
                    // Low-entropy fixtures on purpose: realistic tokens trip
                    // push-time secret scanners (GitGuardian flagged the originals).
                    reason: "provider failed: api_key = super-secret; bearer sk-aaaaaaaaaaaaaaaa; authorization: Bearer aaaaaaaaaaaaaa"
                        .to_string(),
                    recoverable: true,
                },
                extra: BTreeMap::new(),
            })
            .unwrap();

        let page = ledger.replay_events(&run.id, None, 100).unwrap();
        assert_eq!(page.run_id, run.id);
        assert!(!page.history_truncated);
        assert!(
            page.events
                .iter()
                .any(|event| event.event == "fleet.run.created")
        );
        assert!(
            page.events
                .iter()
                .any(|event| event.event == "fleet.worker.artifact")
        );
        assert!(
            page.events
                .iter()
                .any(|event| event.event == "fleet.worker.failed")
        );
        let encoded = serde_json::to_string(&page).unwrap();
        assert!(!encoded.contains("super-secret"));
        assert!(!encoded.contains("sk-aaaaaaaaaaaaaaaa"));
        assert!(!encoded.contains("aaaaaaaaaaaaaa\""));
        assert!(!encoded.contains("private/full-report.md"));
        assert!(!encoded.contains("private-checksum"));

        let first_cursor = page.events[0].cursor.clone();
        drop(ledger);
        let reopened = FleetLedger::open(tmp.path()).unwrap();
        let after_restart = reopened
            .replay_events(&run.id, Some(&first_cursor), 100)
            .unwrap();
        assert_eq!(after_restart.events, page.events[1..]);
        assert!(after_restart.next_cursor.is_some());

        let tail = reopened.replay_events(&run.id, None, 2).unwrap();
        assert_eq!(tail.events.len(), 2);
        assert!(tail.history_truncated);
        assert!(!tail.has_more);

        assert!(matches!(
            reopened.replay_events(&run.id, Some("fev1_missing_worker"), 100),
            Err(FleetEventReplayError::CursorUnavailable { .. })
        ));

        reopened.compact().unwrap();
        assert!(
            matches!(
                reopened.replay_events(&run.id, Some(&first_cursor), 100),
                Err(FleetEventReplayError::CursorUnavailable { .. })
            ),
            "even a retained RunCreated transition must rotate its cursor when compaction deletes intervening history"
        );
        let compacted = reopened.replay_events(&run.id, None, 100).unwrap();
        assert!(compacted.history_truncated);
        assert!(
            !compacted.events.is_empty(),
            "current projection remains replayable after compaction"
        );
        assert!(
            compacted
                .events
                .iter()
                .all(|event| !page.events.iter().any(|old| old.cursor == event.cursor)),
            "a compaction epoch must rotate every retained transition cursor"
        );
    }

    #[test]
    fn fleet_ledger_enqueue_and_claim() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-b")).unwrap();

        let claimed = ledger
            .claim_next("worker-1", &[], "2026-06-12T17:01:00Z")
            .unwrap();
        assert!(claimed.is_some());
        let claimed = claimed.unwrap();
        assert_eq!(claimed.task_id, "task-a");

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.tasks.len(), 2);
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert_eq!(
            state.tasks["run-1:task-a"].leased_to.as_deref(),
            Some("worker-1")
        );
        assert_eq!(
            state.tasks["run-1:task-b"].status,
            FleetTaskLedgerStatus::Enqueued
        );
    }

    #[test]
    fn concurrent_ledgers_allocate_unique_monotonic_event_sequences() {
        const WRITERS: usize = 2;
        const EVENTS_PER_WRITER: usize = 8;

        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles = (0..WRITERS)
            .map(|_| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let ledger = FleetLedger::open(&root).unwrap();
                    barrier.wait();
                    for _ in 0..EVENTS_PER_WRITER {
                        ledger
                            .append_event_next_seq(
                                &FleetRunId::from("run-1"),
                                "worker-1",
                                "task-a",
                                "2026-06-12T17:01:00Z",
                                FleetWorkerEventPayload::Running,
                            )
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let mut sequences = std::fs::read_to_string(ledger.path())
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<FleetLedgerRecord>(line).ok())
            .filter_map(|record| match record {
                FleetLedgerRecord::EventAppended { event }
                    if event.run_id.0 == "run-1"
                        && event.worker_id == "worker-1"
                        && event.task_id == "task-a" =>
                {
                    Some(event.seq)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        sequences.sort_unstable();
        assert_eq!(
            sequences,
            (1..=(WRITERS * EVENTS_PER_WRITER) as u64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn concurrent_ledgers_claim_one_queued_task_once() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["worker-a", "worker-b"].map(|worker_id| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let ledger = FleetLedger::open(&root).unwrap();
                barrier.wait();
                ledger
                    .claim_next(worker_id, &[], "2026-06-12T17:01:00Z")
                    .unwrap()
            })
        });
        let claims = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].task_id, "task-a");
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.tasks["run-1:task-a"].entry.attempts, 1);
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
    }

    #[test]
    fn concurrent_start_and_cancel_leave_one_terminal_task_without_late_progress() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));

        let start_root = root.clone();
        let start_barrier = Arc::clone(&barrier);
        let starter = thread::spawn(move || {
            let ledger = FleetLedger::open(&start_root).unwrap();
            start_barrier.wait();
            ledger
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![
                        FleetWorkerEventPayload::Leased {
                            lease_expires_at: None,
                        },
                        FleetWorkerEventPayload::Starting,
                        FleetWorkerEventPayload::Artifact(FleetArtifactRef {
                            kind: FleetArtifactKind::Log,
                            path: PathBuf::from(".codewhale/fleet/run-1/task-a/worker-1.log"),
                            checksum: None,
                            mime_type: Some("text/plain".to_string()),
                            size_bytes: Some(0),
                        }),
                        FleetWorkerEventPayload::Running,
                    ],
                    || Ok(()),
                )
                .unwrap()
        });
        let cancel_root = root.clone();
        let cancel_barrier = Arc::clone(&barrier);
        let canceller = thread::spawn(move || {
            let ledger = FleetLedger::open(&cancel_root).unwrap();
            cancel_barrier.wait();
            ledger
                .cancel_task_if_active(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    None,
                    "2026-06-12T17:02:00Z",
                    Some("operator"),
                    Some("operator"),
                )
                .unwrap()
        });

        let started = starter.join().unwrap();
        assert!(canceller.join().unwrap());
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Cancelled
        );
        assert_eq!(
            state.tasks["run-1:task-a"].entry.attempts,
            u32::from(started)
        );
        if started {
            assert!(matches!(
                state.latest_events["worker-1:run-1:task-a"].payload,
                FleetWorkerEventPayload::Cancelled { .. }
            ));
        }
        let artifacts_before = state.artifact_events.len();
        assert!(
            ledger
                .append_event_if_leased(
                    &FleetRunId::from("run-1"),
                    "worker-1",
                    "task-a",
                    u32::from(started),
                    "2026-06-12T17:03:00Z",
                    FleetWorkerEventPayload::Artifact(FleetArtifactRef {
                        kind: FleetArtifactKind::Log,
                        path: PathBuf::from("late.log"),
                        checksum: None,
                        mime_type: None,
                        size_bytes: None,
                    }),
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            ledger.rebuild_state().unwrap().artifact_events.len(),
            artifacts_before
        );
    }

    #[test]
    fn cancelled_queue_does_not_run_start_projection_callback() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .cancel_task_if_active(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    None,
                    "2026-06-12T17:01:00Z",
                    None,
                    Some("operator"),
                )
                .unwrap()
        );
        let callback_ran = AtomicBool::new(false);
        assert!(
            !ledger
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:02:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || {
                        callback_ran.store(true, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .unwrap()
        );
        assert!(!callback_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_start_projection_leaves_task_unleased_and_without_events() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();

        let error = ledger
            .start_task_if_enqueued(
                &FleetRunId::from("run-1"),
                "task-a",
                "worker-1",
                "2026-06-12T17:02:00Z",
                None,
                Some(1),
                vec![FleetWorkerEventPayload::Running],
                || Err(anyhow!("forced coordination persistence failure")),
            )
            .expect_err("projection failure must abort the lease");
        assert!(error.to_string().contains("forced coordination"));

        let state = ledger.rebuild_state().unwrap();
        let task = &state.tasks["run-1:task-a"];
        assert_eq!(task.status, FleetTaskLedgerStatus::Enqueued);
        assert_eq!(task.entry.attempts, 0);
        assert!(task.leased_to.is_none());
        assert!(state.latest_events.is_empty());
        assert!(state.latest_seq.is_empty());
        assert!(!state.heartbeats.contains_key("worker-1"));
    }

    #[test]
    fn concurrent_restarts_compare_and_set_one_new_attempt() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let state = ledger.rebuild_state().unwrap();
        let expected_seq = state.latest_seq["worker-1:run-1:task-a"];
        let expected_heartbeat = state.heartbeats["worker-1"].timestamp.clone();
        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                let expected_heartbeat = expected_heartbeat.clone();
                thread::spawn(move || {
                    let ledger = FleetLedger::open(&root).unwrap();
                    barrier.wait();
                    ledger
                        .restart_task_if_unchanged(
                            &FleetRunId::from("run-1"),
                            "task-a",
                            "worker-1",
                            FleetTaskLedgerStatus::Leased,
                            1,
                            expected_seq,
                            Some(&expected_heartbeat),
                            "2026-06-12T17:02:00Z",
                            None,
                            1,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let winners = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|won| *won)
            .count();

        assert_eq!(winners, 1);
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.tasks["run-1:task-a"].entry.attempts, 2);
        assert_eq!(state.latest_seq["worker-1:run-1:task-a"], expected_seq + 2);
    }

    #[test]
    fn stale_verifier_status_cannot_overwrite_restarted_attempt() {
        let tmp = TempDir::new().unwrap();
        let verifier = FleetLedger::open(tmp.path()).unwrap();
        verifier.create_run(&sample_run("run-1")).unwrap();
        verifier.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            verifier
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let terminal = verifier
            .append_terminal_event_if_leased(
                &FleetRunId::from("run-1"),
                "worker-1",
                "task-a",
                1,
                "2026-06-12T17:02:00Z",
                FleetWorkerEventPayload::Completed {
                    exit_code: Some(0),
                    summary: None,
                },
            )
            .unwrap()
            .unwrap();
        let state = verifier.rebuild_state().unwrap();
        let heartbeat = state.heartbeats["worker-1"].timestamp.clone();
        let restarter = FleetLedger::open(tmp.path()).unwrap();
        assert!(
            restarter
                .restart_task_if_unchanged(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    FleetTaskLedgerStatus::Completed,
                    1,
                    terminal.seq,
                    Some(&heartbeat),
                    "2026-06-12T17:03:00Z",
                    None,
                    1,
                )
                .unwrap()
        );
        assert!(
            !verifier
                .mark_task_terminal_status_if_unchanged(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    FleetTaskLedgerStatus::Completed,
                    1,
                    terminal.seq,
                    "2026-06-12T17:04:00Z",
                    FleetTaskLedgerStatus::Failed,
                )
                .unwrap()
        );
        let state = verifier.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert_eq!(state.tasks["run-1:task-a"].entry.attempts, 2);
    }

    #[test]
    fn fresh_heartbeat_invalidates_stale_scheduler_failure() {
        let tmp = TempDir::new().unwrap();
        let scheduler = FleetLedger::open(tmp.path()).unwrap();
        scheduler.create_run(&sample_run("run-1")).unwrap();
        scheduler.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            scheduler
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let stale = scheduler
            .append_event_if_leased(
                &FleetRunId::from("run-1"),
                "worker-1",
                "task-a",
                1,
                "2026-06-12T17:02:00Z",
                FleetWorkerEventPayload::Stale {
                    last_heartbeat_at: Some("2026-06-12T17:01:00Z".to_string()),
                },
            )
            .unwrap()
            .unwrap();
        let worker = FleetLedger::open(tmp.path()).unwrap();
        worker
            .heartbeat("worker-1", "2026-06-12T17:02:30Z", None, None)
            .unwrap();

        assert!(
            scheduler
                .append_terminal_event_if_lease_unchanged(
                    &FleetRunId::from("run-1"),
                    "worker-1",
                    "task-a",
                    1,
                    stale.seq,
                    Some("2026-06-12T17:01:00Z"),
                    "2026-06-12T17:03:00Z",
                    FleetWorkerEventPayload::Failed {
                        reason: "stale retry budget exhausted".to_string(),
                        recoverable: false,
                    },
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            scheduler.rebuild_state().unwrap().tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
    }

    #[test]
    fn cancellation_and_completion_are_compare_and_set_terminal_transitions() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .lease_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                )
                .unwrap()
        );
        assert!(
            ledger
                .cancel_task_if_active(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    Some("worker-1"),
                    "2026-06-12T17:02:00Z",
                    Some("operator"),
                    Some("operator"),
                )
                .unwrap()
        );
        assert!(
            ledger
                .append_terminal_event_if_leased(
                    &FleetRunId::from("run-1"),
                    "worker-1",
                    "task-a",
                    1,
                    "2026-06-12T17:03:00Z",
                    FleetWorkerEventPayload::Completed {
                        exit_code: Some(0),
                        summary: None,
                    },
                )
                .unwrap()
                .is_none()
        );
        assert!(
            !ledger
                .cancel_task_if_active(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    Some("worker-1"),
                    "2026-06-12T17:04:00Z",
                    Some("operator"),
                    Some("operator"),
                )
                .unwrap()
        );
        assert_eq!(
            ledger.rebuild_state().unwrap().tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Cancelled
        );
    }

    #[test]
    fn fleet_ledger_survives_restart() {
        let tmp = TempDir::new().unwrap();
        {
            let ledger = FleetLedger::open(tmp.path()).unwrap();
            ledger.create_run(&sample_run("run-1")).unwrap();
            ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
            ledger
                .lease_task(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                )
                .unwrap();
        }
        // Re-open simulates process restart.
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.runs.len(), 1);
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
    }

    #[test]
    fn heartbeat_between_stale_snapshot_and_append_fences_stale_event() {
        let tmp = TempDir::new().unwrap();
        let scheduler = FleetLedger::open(tmp.path()).unwrap();
        scheduler.create_run(&sample_run("run-1")).unwrap();
        scheduler.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            scheduler
                .start_task_if_enqueued(
                    &FleetRunId::from("run-1"),
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let snapshot = scheduler.rebuild_state().unwrap();
        let expected_seq = snapshot.latest_seq["worker-1:run-1:task-a"];
        let expected_heartbeat = snapshot.heartbeats["worker-1"].timestamp.clone();

        FleetLedger::open(tmp.path())
            .unwrap()
            .heartbeat("worker-1", "2026-06-12T17:02:30Z", None, None)
            .unwrap();

        assert!(
            scheduler
                .append_event_if_lease_unchanged(
                    &FleetRunId::from("run-1"),
                    "worker-1",
                    "task-a",
                    1,
                    expected_seq,
                    Some(&expected_heartbeat),
                    "2026-06-12T17:03:00Z",
                    FleetWorkerEventPayload::Stale {
                        last_heartbeat_at: Some(expected_heartbeat.clone()),
                    },
                )
                .unwrap()
                .is_none()
        );
        let state = scheduler.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert!(matches!(
            state.latest_events["worker-1:run-1:task-a"].payload,
            FleetWorkerEventPayload::Running
        ));
    }

    #[test]
    fn stale_attempt_cannot_finalize_or_replace_restarted_attempt_receipt() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let run_id = FleetRunId::from("run-1");
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .start_task_if_enqueued(
                    &run_id,
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let before_restart = ledger.rebuild_state().unwrap();
        assert!(
            ledger
                .restart_task_if_unchanged(
                    &run_id,
                    "task-a",
                    "worker-1",
                    FleetTaskLedgerStatus::Leased,
                    1,
                    before_restart.latest_seq["worker-1:run-1:task-a"],
                    Some(&before_restart.heartbeats["worker-1"].timestamp),
                    "2026-06-12T17:02:00Z",
                    None,
                    1,
                )
                .unwrap()
        );

        let receipt = |attempt, result| FleetReceipt {
            run_id: run_id.clone(),
            task_id: "task-a".to_string(),
            worker_id: "worker-1".to_string(),
            attempt: Some(attempt),
            terminal_seq: None,
            completed_at: "2026-06-12T17:03:00Z".to_string(),
            result,
            failure_kind: None,
            artifacts: Vec::new(),
            score: None,
            resolved_route: None,
            saved_session_id: None,
            effective_permissions: None,
        };
        assert!(
            ledger
                .finalize_task_attempt_if_leased(
                    &run_id,
                    "worker-1",
                    "task-a",
                    1,
                    "2026-06-12T17:03:00Z",
                    FleetWorkerEventPayload::Completed {
                        exit_code: Some(0),
                        summary: Some("late attempt one".to_string()),
                    },
                    None,
                    receipt(1, FleetTaskResult::Fail),
                )
                .unwrap()
                .is_none()
        );
        let winning_event = ledger
            .finalize_task_attempt_if_leased(
                &run_id,
                "worker-1",
                "task-a",
                2,
                "2026-06-12T17:04:00Z",
                FleetWorkerEventPayload::Completed {
                    exit_code: Some(0),
                    summary: Some("attempt two".to_string()),
                },
                None,
                receipt(2, FleetTaskResult::Pass),
            )
            .unwrap()
            .unwrap();

        let state = ledger.rebuild_state().unwrap();
        let durable = &state.receipts["run-1:task-a"];
        assert_eq!(durable.attempt, Some(2));
        assert_eq!(durable.terminal_seq, Some(winning_event.seq));
        assert_eq!(durable.result, FleetTaskResult::Pass);
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Completed
        );
    }

    #[test]
    fn fleet_ledger_quarantines_unterminated_tail_before_next_valid_record() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        // Simulate a process dying before its trailing newline, then use the
        // normal append path. The next record must not be concatenated to and
        // lost with the malformed crash tail.
        let mut file = OpenOptions::new().append(true).open(ledger.path()).unwrap();
        write!(file, "{{\"record\":\"run_created\",\"run\":").unwrap();
        file.sync_all().unwrap();
        drop(file);
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.runs.len(), 1);
        assert!(state.runs.contains_key("run-1"));
        assert!(state.tasks.contains_key("run-1:task-a"));
    }

    #[test]
    fn fleet_ledger_event_and_heartbeat_reconstruct_worker_status() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 1,
                run_id: FleetRunId::from("run-1"),
                worker_id: "worker-1".to_string(),
                task_id: "task-a".to_string(),
                timestamp: "2026-06-12T17:01:00Z".to_string(),
                payload: FleetWorkerEventPayload::Running,
                extra: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .heartbeat("worker-1", "2026-06-12T17:02:00Z", Some(12.5), Some(1024))
            .unwrap();

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.workers["worker-1"], FleetWorkerStatus::Busy);
        assert_eq!(state.heartbeats["worker-1"].cpu_percent, Some(12.5));
    }

    #[test]
    fn fleet_ledger_replays_typed_workflow_receipt_with_distinct_run_ids() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("fleet-run-1")).unwrap();
        ledger
            .enqueue(sample_entry("fleet-run-1", "task-a"))
            .unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 1,
                run_id: FleetRunId::from("fleet-run-1"),
                worker_id: "worker-1".to_string(),
                task_id: "task-a".to_string(),
                timestamp: "2026-07-10T00:00:00Z".to_string(),
                payload: FleetWorkerEventPayload::WorkflowEvent {
                    workflow_run_id: "workflow_1".to_string(),
                    event: serde_json::json!({"type": "task_completed"}),
                },
                extra: BTreeMap::new(),
            })
            .unwrap();

        let state = ledger.rebuild_state().unwrap();
        let event = &state.latest_events["worker-1:fleet-run-1:task-a"];
        assert!(matches!(
            &event.payload,
            FleetWorkerEventPayload::WorkflowEvent {
                workflow_run_id,
                event,
            } if workflow_run_id == "workflow_1" && event["type"] == "task_completed"
        ));
        let last_line = std::fs::read_to_string(ledger.path())
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        assert_eq!(last_line.matches("\"run_id\"").count(), 1);
        assert_eq!(last_line.matches("\"workflow_run_id\"").count(), 1);
    }

    #[test]
    fn fleet_ledger_terminal_events_ignore_late_progress_regressions() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger
            .enqueue(sample_entry("run-1", "task-failed"))
            .unwrap();
        ledger
            .enqueue(sample_entry("run-1", "task-cancelled"))
            .unwrap();

        ledger
            .append_event(FleetWorkerEvent {
                seq: 1,
                run_id: FleetRunId::from("run-1"),
                worker_id: "worker-1".to_string(),
                task_id: "task-failed".to_string(),
                timestamp: "2026-06-12T17:03:00Z".to_string(),
                payload: FleetWorkerEventPayload::Failed {
                    reason: "test failed".to_string(),
                    recoverable: false,
                },
                extra: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 2,
                run_id: FleetRunId::from("run-1"),
                worker_id: "worker-2".to_string(),
                task_id: "task-cancelled".to_string(),
                timestamp: "2026-06-12T17:04:00Z".to_string(),
                payload: FleetWorkerEventPayload::Cancelled {
                    cancelled_by: Some("operator".to_string()),
                },
                extra: BTreeMap::new(),
            })
            .unwrap();
        // A live worker can flush progress after an out-of-process operator
        // command has already made the task terminal. Preserve the raw
        // sequence for append ordering without projecting the task or worker
        // back to a running state.
        ledger
            .append_event(FleetWorkerEvent {
                seq: 3,
                run_id: FleetRunId::from("run-1"),
                worker_id: "worker-2".to_string(),
                task_id: "task-cancelled".to_string(),
                timestamp: "2026-06-12T17:04:01Z".to_string(),
                payload: FleetWorkerEventPayload::Running,
                extra: BTreeMap::new(),
            })
            .unwrap();

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-failed"].status,
            FleetTaskLedgerStatus::Failed
        );
        assert_eq!(
            state.tasks["run-1:task-cancelled"].status,
            FleetTaskLedgerStatus::Cancelled
        );
        assert_eq!(state.workers["worker-2"], FleetWorkerStatus::Online);
        assert_eq!(
            state.latest_seq["worker-2:run-1:task-cancelled"], 3,
            "raw sequence ownership must still advance past ignored progress"
        );
        assert!(matches!(
            state.latest_events["worker-2:run-1:task-cancelled"].payload,
            FleetWorkerEventPayload::Cancelled { .. }
        ));

        ledger.compact().unwrap();
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-failed"].status,
            FleetTaskLedgerStatus::Failed
        );
        assert_eq!(
            state.tasks["run-1:task-cancelled"].status,
            FleetTaskLedgerStatus::Cancelled
        );
        assert_eq!(state.workers["worker-2"], FleetWorkerStatus::Online);
        assert_eq!(
            state.latest_seq["worker-2:run-1:task-cancelled"], 3,
            "compaction must preserve the ignored event sequence high-water mark"
        );
        assert!(matches!(
            state.latest_events["worker-2:run-1:task-cancelled"].payload,
            FleetWorkerEventPayload::Cancelled { .. }
        ));
        let next = ledger
            .append_event_next_seq(
                &FleetRunId::from("run-1"),
                "worker-2",
                "task-cancelled",
                "2026-06-12T17:04:02Z",
                FleetWorkerEventPayload::Running,
            )
            .unwrap();
        assert_eq!(next.seq, 4, "compaction must never permit sequence reuse");
    }

    #[test]
    fn fleet_ledger_compact_preserves_current_state() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        ledger
            .lease_task(
                &FleetRunId::from("run-1"),
                "task-a",
                "worker-1",
                "2026-06-12T17:01:00Z",
                None,
            )
            .unwrap();
        ledger
            .append_event(FleetWorkerEvent {
                seq: 7,
                run_id: FleetRunId::from("run-1"),
                worker_id: "worker-1".to_string(),
                task_id: "task-a".to_string(),
                timestamp: "2026-06-12T17:01:30Z".to_string(),
                payload: FleetWorkerEventPayload::Running,
                extra: BTreeMap::new(),
            })
            .unwrap();
        ledger
            .heartbeat("worker-1", "2026-06-12T17:02:00Z", Some(12.5), Some(1024))
            .unwrap();
        ledger
            .record_receipt(FleetReceipt {
                run_id: FleetRunId::from("run-1"),
                task_id: "task-a".to_string(),
                worker_id: "worker-1".to_string(),
                attempt: Some(1),
                terminal_seq: None,
                completed_at: "2026-06-12T17:03:00Z".to_string(),
                result: FleetTaskResult::Pass,
                failure_kind: None,
                artifacts: vec![],
                score: None,
                resolved_route: None,
                saved_session_id: None,
                effective_permissions: None,
            })
            .unwrap();

        let lifecycle_seq_before_compaction =
            ledger.rebuild_state().unwrap().tasks["run-1:task-a"].lifecycle_seq;
        assert_eq!(
            lifecycle_seq_before_compaction, 2,
            "enqueue and lease are the two effective owner states"
        );

        ledger.compact().unwrap();
        let contents = std::fs::read_to_string(ledger.path()).unwrap();
        assert!(contents.lines().count() >= 5, "{contents}");

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.runs.len(), 1);
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert_eq!(state.workers["worker-1"], FleetWorkerStatus::Busy);
        assert_eq!(state.heartbeats["worker-1"].memory_mb, Some(1024));
        assert_eq!(
            state.tasks["run-1:task-a"].entry.attempts, 1,
            "compaction must not mint a synthetic retry attempt"
        );
        assert_eq!(
            state.tasks["run-1:task-a"].lifecycle_seq, lifecycle_seq_before_compaction,
            "compaction must not mint an owner lifecycle transition"
        );
        assert!(state.latest_seq.values().any(|seq| *seq == 7));
        assert_eq!(state.receipts["run-1:task-a"].result, FleetTaskResult::Pass);
    }

    #[test]
    fn fleet_compaction_preserves_multilease_lifecycle_high_water() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        ledger
            .lease_task(
                &FleetRunId::from("run-1"),
                "task-a",
                "worker-1",
                "2026-06-12T17:01:00Z",
                None,
            )
            .unwrap();
        ledger
            .lease_task(
                &FleetRunId::from("run-1"),
                "task-a",
                "worker-1",
                "2026-06-12T17:02:00Z",
                None,
            )
            .unwrap();
        let before = ledger.rebuild_state().unwrap();
        assert_eq!(before.tasks["run-1:task-a"].lifecycle_seq, 3);

        ledger.compact().unwrap();
        let after = ledger.rebuild_state().unwrap();
        assert_eq!(
            after.tasks["run-1:task-a"].lifecycle_seq, 3,
            "compaction must not reuse lower Work Graph idempotency keys"
        );
    }

    #[test]
    fn compaction_preserves_verifier_failure_override_after_completed_exit() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let run_id = FleetRunId::from("run-1");
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .start_task_if_enqueued(
                    &run_id,
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        let terminal = ledger
            .finalize_task_attempt_if_leased(
                &run_id,
                "worker-1",
                "task-a",
                1,
                "2026-06-12T17:02:00Z",
                FleetWorkerEventPayload::Completed {
                    exit_code: Some(0),
                    summary: Some("process succeeded but verification failed".to_string()),
                },
                Some(FleetTaskLedgerStatus::Failed),
                FleetReceipt {
                    run_id: run_id.clone(),
                    task_id: "task-a".to_string(),
                    worker_id: "worker-1".to_string(),
                    attempt: Some(1),
                    terminal_seq: None,
                    completed_at: "2026-06-12T17:02:00Z".to_string(),
                    result: FleetTaskResult::Fail,
                    failure_kind: Some(FleetTaskFailureKind::Verifier),
                    artifacts: Vec::new(),
                    score: None,
                    resolved_route: None,
                    saved_session_id: None,
                    effective_permissions: None,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            ledger.rebuild_state().unwrap().tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Failed
        );

        ledger.compact().unwrap();
        let reopened = FleetLedger::open(tmp.path()).unwrap();
        let state = reopened.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Failed
        );
        assert_eq!(state.receipts["run-1:task-a"].attempt, Some(1));
        assert_eq!(
            state.receipts["run-1:task-a"].terminal_seq,
            Some(terminal.seq)
        );
        assert!(matches!(
            state.latest_events["worker-1:run-1:task-a"].payload,
            FleetWorkerEventPayload::Completed { .. }
        ));
    }

    #[test]
    fn legacy_attemptless_alert_suppresses_duplicate_delivery_after_upgrade() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let run_id = FleetRunId::from("run-1");
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();
        assert!(
            ledger
                .start_task_if_enqueued(
                    &run_id,
                    "task-a",
                    "worker-1",
                    "2026-06-12T17:01:00Z",
                    None,
                    Some(1),
                    vec![FleetWorkerEventPayload::Running],
                    || Ok(()),
                )
                .unwrap()
        );
        ledger
            .append_terminal_event_if_leased(
                &run_id,
                "worker-1",
                "task-a",
                1,
                "2026-06-12T17:02:00Z",
                FleetWorkerEventPayload::Failed {
                    reason: "attempt exhausted before upgrade".to_string(),
                    recoverable: false,
                },
            )
            .unwrap()
            .unwrap();
        ledger
            .record_alert(&run_id, "task-a", "slack", "2026-06-12T17:02:01Z")
            .unwrap();
        assert!(
            !ledger
                .record_failed_attempt_alert_once(
                    &run_id,
                    "task-a",
                    "worker-1",
                    1,
                    "slack",
                    "slack#0",
                    "2026-06-12T17:03:00Z",
                )
                .unwrap()
        );
        assert_eq!(ledger.rebuild_state().unwrap().alerts.len(), 1);

        ledger.compact().unwrap();
        assert!(
            !ledger
                .record_failed_attempt_alert_once(
                    &run_id,
                    "task-a",
                    "worker-1",
                    1,
                    "slack",
                    "slack#0",
                    "2026-06-12T17:04:00Z",
                )
                .unwrap()
        );
        assert_eq!(ledger.rebuild_state().unwrap().alerts.len(), 1);

        let attempt_one = ledger.rebuild_state().unwrap();
        assert!(
            ledger
                .restart_task_if_unchanged(
                    &run_id,
                    "task-a",
                    "worker-1",
                    FleetTaskLedgerStatus::Failed,
                    1,
                    attempt_one.latest_seq["worker-1:run-1:task-a"],
                    Some(&attempt_one.heartbeats["worker-1"].timestamp),
                    "2026-06-12T17:05:00Z",
                    None,
                    1,
                )
                .unwrap()
        );
        ledger
            .append_terminal_event_if_leased(
                &run_id,
                "worker-1",
                "task-a",
                2,
                "2026-06-12T17:06:00Z",
                FleetWorkerEventPayload::Failed {
                    reason: "attempt two also exhausted".to_string(),
                    recoverable: false,
                },
            )
            .unwrap()
            .unwrap();
        assert!(
            ledger
                .record_failed_attempt_alert_once(
                    &run_id,
                    "task-a",
                    "worker-1",
                    2,
                    "slack",
                    "slack#0",
                    "2026-06-12T17:07:00Z",
                )
                .unwrap()
        );
        assert!(
            !ledger
                .record_failed_attempt_alert_once(
                    &run_id,
                    "task-a",
                    "worker-1",
                    2,
                    "slack",
                    "slack#0",
                    "2026-06-12T17:08:00Z",
                )
                .unwrap()
        );
        assert_eq!(ledger.rebuild_state().unwrap().alerts.len(), 2);
    }

    #[test]
    fn compaction_lock_keeps_concurrent_append_on_replacement_ledger() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        ledger.create_run(&sample_run("run-1")).unwrap();
        ledger.enqueue(sample_entry("run-1", "task-a")).unwrap();

        let root = tmp.path().to_path_buf();
        let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let compact_root = root.clone();
        let compactor = thread::spawn(move || {
            let ledger = FleetLedger::open(&compact_root).unwrap();
            ledger
                .compact_with_snapshot_hook(|| {
                    snapshot_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .unwrap();
        });
        snapshot_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("compaction never reached its locked snapshot");

        let contender = FleetLedger::open(&root).unwrap();
        let lock_file = contender.open_lock_file().unwrap();
        let mut lock = fd_lock::RwLock::new(lock_file);
        match lock.try_write() {
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock),
            Ok(_) => panic!("compaction snapshot did not retain the Fleet ledger lock"),
        }

        let (append_started_tx, append_started_rx) = mpsc::sync_channel(0);
        let (append_done_tx, append_done_rx) = mpsc::sync_channel(0);
        let append_root = root.clone();
        let appender = thread::spawn(move || {
            let ledger = FleetLedger::open(&append_root).unwrap();
            append_started_tx.send(()).unwrap();
            let event = ledger
                .append_event_next_seq(
                    &FleetRunId::from("run-1"),
                    "worker-1",
                    "task-a",
                    "2026-06-12T17:01:00Z",
                    FleetWorkerEventPayload::Running,
                )
                .unwrap();
            append_done_tx.send(event.seq).unwrap();
        });
        append_started_rx.recv().unwrap();
        assert!(
            append_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "append completed while compaction still held the ledger lock"
        );

        release_tx.send(()).unwrap();
        compactor.join().unwrap();
        assert_eq!(
            append_done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            1
        );
        appender.join().unwrap();

        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.latest_seq["worker-1:run-1:task-a"], 1);
        assert!(matches!(
            &state.latest_events["worker-1:run-1:task-a"].payload,
            FleetWorkerEventPayload::Running
        ));
    }

    #[test]
    fn fleet_ledger_receipt_round_trip() {
        let tmp = TempDir::new().unwrap();
        let ledger = FleetLedger::open(tmp.path()).unwrap();
        let receipt = FleetReceipt {
            run_id: FleetRunId::from("run-1"),
            task_id: "task-a".to_string(),
            worker_id: "worker-1".to_string(),
            attempt: None,
            terminal_seq: None,
            completed_at: "2026-06-12T17:03:00Z".to_string(),
            result: FleetTaskResult::Pass,
            failure_kind: None,
            artifacts: vec![],
            score: None,
            resolved_route: None,
            saved_session_id: None,
            effective_permissions: None,
        };
        ledger.record_receipt(receipt.clone()).unwrap();
        let state = ledger.rebuild_state().unwrap();
        assert_eq!(state.receipts["run-1:task-a"].result, FleetTaskResult::Pass);
    }
}

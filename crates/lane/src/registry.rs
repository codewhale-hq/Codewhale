//! Durable lane registry under `$CODEWHALE_HOME/lanes/`.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::runtime::RuntimeBackendKind;

const LANES_SUBDIR: &str = "lanes";
const LOGS_SUBDIR: &str = "logs";

/// Lifecycle status for a running workflow instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneStatus {
    Pending,
    Running,
    Stopped,
    Failed,
    Completed,
}

impl LaneStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Completed => "completed",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

/// Result of an attempted terminal transition.
///
/// The caller needs all three cases distinguished to report a truthful
/// receipt: "I stopped it", "it was already terminal", and "it moved on since
/// you read it, so I refused". Collapsing them into a bool made a concurrent
/// stop by another process indistinguishable from our own transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTransition {
    /// This call performed the active -> terminal transition.
    Transitioned,
    /// The record was already terminal; nothing changed.
    AlreadyTerminal,
    /// The caller pinned a lifecycle generation and the record has moved on.
    /// Nothing was changed and no backend teardown ran.
    FenceMismatch { observed: u64 },
}

impl TerminalTransition {
    #[must_use]
    pub const fn transitioned(self) -> bool {
        matches!(self, Self::Transitioned)
    }
}

/// One lane record: a running (or completed) workflow instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub runtime: RuntimeBackendKind,
    pub status: LaneStatus,
    /// Monotonic durable lifecycle sequence used by Work Graph reconciliation.
    #[serde(default)]
    pub lifecycle_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// tmux session name when `runtime == tmux`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    /// Explicit tmux server socket used for this Lane. Pinning the socket keeps
    /// start/attach/stop/reconcile in the same server namespace even when
    /// `TMUX_TMPDIR` or the caller environment changes between commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_socket: Option<PathBuf>,
    /// Absolute path to the stream-json / NDJSON journal for this lane.
    pub log_path: PathBuf,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    /// Optional human-readable attach target (e.g. `tmux attach -t …`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_target: Option<String>,
    /// Worktree cleanup TTL in seconds (None = no auto-cleanup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_ttl_secs: Option<u64>,
}

impl LaneRecord {
    pub fn new_id() -> String {
        let short = uuid::Uuid::new_v4().to_string();
        format!("lane-{}", &short[..8])
    }

    pub fn now_rfc3339() -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

/// Registry root: `$CODEWHALE_HOME/lanes`.
pub fn lanes_dir() -> Result<PathBuf> {
    codewhale_config::ensure_state_dir(LANES_SUBDIR)
}

/// Where the Lane registry *would* live, without creating it.
///
/// [`lanes_dir`] creates the directory as a side effect, which makes it
/// useless for answering "is there a durable Lane registry?" — a read-only
/// status surface must not conjure the store it is reporting on. Availability
/// probing goes through this instead (see [`crate::control::ControlContext`]).
pub fn lane_registry_root() -> Result<PathBuf> {
    Ok(codewhale_config::codewhale_home()?.join(LANES_SUBDIR))
}

/// A lane id names files under the registry root, so it must be a plain
/// name: ASCII letters, digits, `-` and `_` only.
fn checked_lane_id(id: &str) -> Result<()> {
    let plain = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    anyhow::ensure!(plain, "invalid lane id {id:?}");
    Ok(())
}

/// Persist and load lane records.
#[derive(Debug, Clone)]
pub struct LaneRegistry {
    root: PathBuf,
}

impl LaneRegistry {
    /// Open the default registry under `$CODEWHALE_HOME/lanes`.
    pub fn open_default() -> Result<Self> {
        Self::open(lanes_dir()?)
    }

    /// Open a registry at an explicit root (tests / custom homes).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)
            .with_context(|| format!("create lane registry {}", root.display()))?;
        fs::create_dir_all(root.join(LOGS_SUBDIR))
            .with_context(|| format!("create lane logs under {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_SUBDIR)
    }

    pub fn record_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    pub fn log_path_for(&self, id: &str) -> PathBuf {
        self.logs_dir().join(format!("{id}.ndjson"))
    }

    pub fn save(&self, record: &LaneRecord) -> Result<()> {
        checked_lane_id(&record.id)?;
        let path = self.record_path(&record.id);
        let json = serde_json::to_string_pretty(record).context("serialize lane record")?;
        // A fresh, exclusively created temporary: a predictable name could be
        // pre-placed as a link and written through.
        let tmp = self.root.join(format!(
            ".{}.{}.{}.tmp",
            record.id,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("write {}", tmp.display()));
        }
        if let Err(error) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("rename {}", path.display()));
        }
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<LaneRecord> {
        checked_lane_id(id)?;
        let path = self.record_path(id);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read lane record {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse lane record {}", path.display()))
    }

    pub fn list(&self) -> Result<Vec<LaneRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("read lane registry {}", self.root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_str::<LaneRecord>(&text) {
                Ok(record) => records.push(record),
                Err(err) => {
                    // Skip corrupt records rather than failing the whole list.
                    eprintln!(
                        "warning: skip corrupt lane record {}: {err}",
                        path.display()
                    );
                }
            }
        }
        records.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(records)
    }

    /// Create a pending lane with log file reserved.
    pub fn create_pending(
        &self,
        workflow: Option<String>,
        fleet: Option<String>,
        issue: Option<String>,
        goal: Option<String>,
        runtime: RuntimeBackendKind,
        worktree_ttl_secs: Option<u64>,
    ) -> Result<LaneRecord> {
        let id = LaneRecord::new_id();
        let log_path = self.log_path_for(&id);
        // Touch the log so `lane logs` works immediately.
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&log_path)
            .with_context(|| format!("create log {}", log_path.display()))?;
        let record = LaneRecord {
            id,
            workflow,
            fleet,
            issue,
            goal,
            runtime,
            status: LaneStatus::Pending,
            lifecycle_seq: 1,
            worktree_path: None,
            branch: None,
            tmux_session: None,
            tmux_socket: None,
            log_path,
            started_at: LaneRecord::now_rfc3339(),
            stopped_at: None,
            attach_target: None,
            worktree_ttl_secs,
        };
        self.save(&record)?;
        Ok(record)
    }

    /// Record a freshly provisioned worktree on the stored Lane.
    ///
    /// Runs under the per-Lane lock and changes only `worktree_path` and
    /// `branch`, so whichever terminal record a failed start or a concurrent
    /// stop leaves behind still references the worktree.
    pub fn persist_worktree(&self, record: &LaneRecord) -> Result<()> {
        let lock_path = self.root.join(format!("{}.lock", record.id));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open lane lock {}", lock_path.display()))?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("lock lane record {}", record.id))?;

        let mut current = self.load(&record.id)?;
        current.worktree_path = record.worktree_path.clone();
        current.branch = record.branch.clone();
        self.save(&current)
    }

    /// Atomically promote a Pending Lane to Running.
    ///
    /// A concurrent `lane stop` is allowed to win while a backend is still
    /// launching. In that case this returns `false`, reloads the terminal
    /// record, and the backend must tear down any process it just created.
    pub fn mark_running_if_pending(&self, record: &mut LaneRecord) -> Result<bool> {
        self.mark_running_if_pending_with(record, || Ok(()), || Ok(()))
    }

    /// Atomically launch backend state and promote a Pending Lane to Running.
    ///
    /// The per-Lane lock spans the durable Pending check, `before_transition`,
    /// and the Running save. A concurrent stop therefore either wins before
    /// launch (and this returns `false` without calling the closure) or waits
    /// until the Running record is visible. Backend metadata is first saved in
    /// the Pending record so a failed final save still leaves enough data for
    /// a later stop. `rollback` is attempted if that final save fails.
    pub fn mark_running_if_pending_with<Start, Rollback>(
        &self,
        record: &mut LaneRecord,
        before_transition: Start,
        rollback: Rollback,
    ) -> Result<bool>
    where
        Start: FnOnce() -> Result<()>,
        Rollback: FnOnce() -> Result<()>,
    {
        let lock_path = self.root.join(format!("{}.lock", record.id));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open lane lock {}", lock_path.display()))?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("lock lane record {}", record.id))?;

        let current = self.load(&record.id)?;
        if current.status != LaneStatus::Pending {
            *record = current;
            return Ok(false);
        }
        record.lifecycle_seq = current.lifecycle_seq.max(1);

        // `record` carries backend metadata (tmux session, worktree, attach
        // target) populated during launch. Persist it while still Pending so
        // a failed launch/final save remains discoverable and stoppable.
        record.status = LaneStatus::Pending;
        record.stopped_at = None;
        self.save(record)?;

        before_transition()?;
        record.status = LaneStatus::Running;
        record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
        record.stopped_at = None;
        if let Err(save_error) = self.save(record) {
            record.status = LaneStatus::Pending;
            record.lifecycle_seq = current.lifecycle_seq.max(1);
            if let Err(rollback_error) = rollback() {
                return Err(save_error).context(format!(
                    "persist running Lane; backend rollback also failed: {rollback_error:#}"
                ));
            }
            return Err(save_error).context("persist running Lane after backend launch");
        }
        Ok(true)
    }

    /// Atomically transition an active Lane to a terminal state.
    ///
    /// Detached tmux reconciliation, an explicit stop, and a second status
    /// reader can race in separate CLI processes. Serialize those terminal
    /// decisions on a per-Lane advisory lock, reload the live record under the
    /// lock, and only let the first active -> terminal transition win.
    pub fn mark_terminal_if_active(
        &self,
        record: &mut LaneRecord,
        status: LaneStatus,
    ) -> Result<bool> {
        self.mark_terminal_if_active_with(record, status, |_| Ok(()))
    }

    /// Atomically perform backend teardown and transition an active Lane.
    ///
    /// `before_transition` runs while holding the per-Lane lifecycle lock.
    /// If teardown fails, the record remains active. This keeps a failed tmux
    /// kill from being persisted as Stopped and prevents cleanup racing a
    /// concurrent reconciliation decision.
    pub fn mark_terminal_if_active_with<F>(
        &self,
        record: &mut LaneRecord,
        status: LaneStatus,
        before_transition: F,
    ) -> Result<bool>
    where
        F: FnOnce(&LaneRecord) -> Result<()>,
    {
        self.mark_terminal_if_active_fenced(record, status, None, before_transition)
            .map(TerminalTransition::transitioned)
    }

    /// [`mark_terminal_if_active_with`] with an optional lifecycle fence.
    ///
    /// `expected_lifecycle_seq` is evaluated **after** the live record is
    /// reloaded under the per-Lane advisory lock, not by the caller before it.
    /// A pre-lock check is a TOCTOU: another process can transition the record
    /// between the caller's read and this write, and the caller would then act
    /// on a generation it never observed. Checking here means a stale fence
    /// refuses without running `before_transition`, so no backend teardown
    /// happens for a run the caller did not actually target.
    ///
    /// [`mark_terminal_if_active_with`]: Self::mark_terminal_if_active_with
    pub fn mark_terminal_if_active_fenced<F>(
        &self,
        record: &mut LaneRecord,
        status: LaneStatus,
        expected_lifecycle_seq: Option<u64>,
        before_transition: F,
    ) -> Result<TerminalTransition>
    where
        F: FnOnce(&LaneRecord) -> Result<()>,
    {
        if status.is_active() {
            bail!("terminal lane transition requires a terminal status");
        }

        let lock_path = self.root.join(format!("{}.lock", record.id));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open lane lock {}", lock_path.display()))?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .with_context(|| format!("lock lane record {}", record.id))?;

        let mut current = self.load(&record.id)?;
        // Fence first: a mismatched generation must not run backend teardown.
        if let Some(expected) = expected_lifecycle_seq
            && expected != current.lifecycle_seq
        {
            let observed = current.lifecycle_seq;
            *record = current;
            return Ok(TerminalTransition::FenceMismatch { observed });
        }
        if !current.status.is_active() {
            *record = current;
            return Ok(TerminalTransition::AlreadyTerminal);
        }
        before_transition(&current)?;
        current.lifecycle_seq = current.lifecycle_seq.max(1).saturating_add(1);
        current.status = status;
        current.stopped_at = Some(LaneRecord::now_rfc3339());
        current.attach_target = None;
        self.save(&current)?;
        *record = current;
        Ok(TerminalTransition::Transitioned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use tempfile::tempdir;

    #[test]
    fn registry_persists_across_open() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let record = reg
            .create_pending(
                Some("stopship".into()),
                Some("stopship".into()),
                Some("4375".into()),
                None,
                RuntimeBackendKind::Tmux,
                Some(3600),
            )
            .unwrap();
        let id = record.id.clone();

        let reg2 = LaneRegistry::open(dir.path()).unwrap();
        let loaded = reg2.load(&id).unwrap();
        assert_eq!(loaded.workflow.as_deref(), Some("stopship"));
        assert_eq!(loaded.fleet.as_deref(), Some("stopship"));
        assert_eq!(loaded.issue.as_deref(), Some("4375"));
        assert_eq!(loaded.runtime, RuntimeBackendKind::Tmux);
        assert_eq!(loaded.status, LaneStatus::Pending);
        assert_eq!(loaded.lifecycle_seq, 1);
        assert!(loaded.log_path.is_file() || loaded.log_path.exists());

        let listed = reg2.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
    }

    #[test]
    fn terminal_lane_cannot_launch_after_stop_wins() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let mut record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        assert!(
            reg.mark_terminal_if_active(&mut record, LaneStatus::Stopped)
                .unwrap()
        );
        let starts = AtomicUsize::new(0);
        assert!(
            !reg.mark_running_if_pending_with(
                &mut record,
                || {
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                || Ok(()),
            )
            .unwrap()
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert_eq!(record.status, LaneStatus::Stopped);
        assert_eq!(record.lifecycle_seq, 2);
        let loaded = reg.load(&record.id).unwrap();
        assert_eq!(loaded.status, LaneStatus::Stopped);
        assert_eq!(loaded.lifecycle_seq, 2);
    }

    #[test]
    fn start_and_stop_are_serialized_across_backend_launch() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        let id = record.id.clone();
        let starts = Arc::new(AtomicUsize::new(0));
        let teardowns = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let start_reg = reg.clone();
        let start_count = Arc::clone(&starts);
        let start_thread = std::thread::spawn(move || {
            let mut record = record;
            let started = start_reg
                .mark_running_if_pending_with(
                    &mut record,
                    || {
                        start_count.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    },
                    || Ok(()),
                )
                .unwrap();
            assert!(started);
        });
        entered_rx.recv().unwrap();

        let stop_reg = reg.clone();
        let stop_id = id.clone();
        let teardown_count = Arc::clone(&teardowns);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stop_thread = std::thread::spawn(move || {
            let mut record = stop_reg.load(&stop_id).unwrap();
            let stopped = stop_reg
                .mark_terminal_if_active_with(&mut record, LaneStatus::Stopped, |_| {
                    teardown_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
            stopped_tx.send(stopped).unwrap();
        });
        assert!(matches!(
            stopped_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release_tx.send(()).unwrap();
        start_thread.join().unwrap();
        assert!(stopped_rx.recv().unwrap());
        stop_thread.join().unwrap();

        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
        let loaded = reg.load(&id).unwrap();
        assert_eq!(loaded.status, LaneStatus::Stopped);
        assert_eq!(
            loaded.lifecycle_seq, 3,
            "pending, running, and stopped are three durable owner states"
        );
    }

    #[test]
    fn teardown_failure_keeps_durable_lane_active() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let mut record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        assert!(reg.mark_running_if_pending(&mut record).unwrap());
        let error = reg
            .mark_terminal_if_active_with(&mut record, LaneStatus::Stopped, |_| {
                bail!("backend still alive")
            })
            .unwrap_err();
        assert!(error.to_string().contains("backend still alive"));
        assert_eq!(record.status, LaneStatus::Running);
        assert_eq!(record.lifecycle_seq, 2);
        let loaded = reg.load(&record.id).unwrap();
        assert_eq!(loaded.status, LaneStatus::Running);
        assert_eq!(loaded.lifecycle_seq, 2);
    }

    #[test]
    fn ids_that_are_not_plain_names_are_refused() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path().join("lanes")).unwrap();
        std::fs::write(dir.path().join("outside.json"), "{}").unwrap();

        for id in ["../outside", "a/b", "", "lane 1", "lane.json"] {
            assert!(reg.load(id).is_err(), "{id:?} must be refused");
        }
        let mut record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        record.id = "../outside".into();
        assert!(reg.save(&record).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("outside.json")).unwrap(),
            "{}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_does_not_write_through_a_planted_temporary_link() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path().join("lanes")).unwrap();
        let record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(
            &victim,
            reg.record_path(&record.id).with_extension("json.tmp"),
        )
        .unwrap();

        reg.save(&record).unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(reg.load(&record.id).unwrap().id, record.id);
    }
}

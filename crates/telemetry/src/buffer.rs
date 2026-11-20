//! The on-disk ring buffer, the tombstone, and the wipe.
//!
//! Everything lives under `$CODEWHALE_HOME/telemetry/`, created `0700`, with
//! every file `0600`:
//!
//! | file | role |
//! |---|---|
//! | `spool/<nonce>.jsonl` | one process's header line, then one JSON event per line, awaiting its flush |
//! | `spool/<nonce>.lock` | held exclusively by that process while it lives |
//! | `buffer.jsonl` | the pre-spool sink, only ever truncated now |
//! | `buffer.jsonl.lock` | the **privacy** lock, a sibling file; never a data file |
//! | `dryrun.jsonl` | the sink when the endpoint resolves empty, same ring policy |
//! | `state.json` | last version seen and last flush attempt |
//! | `install_id.json` | the random install id |
//! | `disabled` | the tombstone: present ⇒ nothing is appended, drained, or sent |
//!
//! Appends, compaction, delivery, identity/state writes, startup arming, and
//! wipe share one sibling lock. Threads of one process take turns on it;
//! between processes, runtime and exit paths take it with `try_write()`, and on
//! contention the event or batch is dropped. The panic hook and signal path
//! never wait, not even for a sibling thread. Only startup arming and the
//! user-requested wipe may wait for another process. That keeps the panic hook
//! and SIGINT path non-blocking while also making opt-out an ordering boundary —
//! after wipe returns, no pre-wipe writer or sender can still publish data.
//!
//! Appenders re-open per append, so a compaction rewrite cannot leave anyone
//! writing to a stale inode.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Newest events retained in either sink.
pub const MAX_EVENTS: usize = 512;
/// Byte ceiling for either sink.
pub const MAX_BYTES: u64 = 256 * 1024;
/// A single append must fit in one atomic `write(2)`.
pub const MAX_LINE_BYTES: usize = 4096;

const PRIVACY_LOCK_BUDGET: Duration = Duration::from_secs(3);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Exact contents of the opt-out tombstone observed by a consent decision.
///
/// A fresh nonce is written when a machine transitions into an opted-out
/// period. Arming may clear only the exact generation its decision observed,
/// so an older consent token cannot erase a newer opt-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TombstoneGeneration(Vec<u8>);

const MAX_TOMBSTONE_BYTES: u64 = 128;

/// Below this size a sink cannot possibly hold [`MAX_EVENTS`] lines, so an
/// append skips the count probe entirely. The shortest serializable event line
/// is well over 8 bytes, and `512 * 9 > 4096`, so this bound is safe by
/// construction — `probe_threshold_cannot_hide_an_over_cap_buffer` pins it.
const PROBE_BYTES: u64 = 4096;

/// `buffer.jsonl` — the pending-event sink.
#[must_use]
pub fn buffer_path(root: &Path) -> PathBuf {
    root.join("buffer.jsonl")
}

/// `dryrun.jsonl` — where batches go when the endpoint resolves to `None`.
///
/// Reached by configuring `telemetry_endpoint` empty; an unconfigured endpoint
/// resolves to `codewhale_config::DEFAULT_TELEMETRY_ENDPOINT` instead.
#[must_use]
pub fn dryrun_path(root: &Path) -> PathBuf {
    root.join("dryrun.jsonl")
}

/// `buffer.jsonl.lock` — the sibling lock file. Never the data file, and never
/// unlinked: replacing it would leave appenders and compactors holding
/// different inodes and serialising against nothing.
#[must_use]
pub fn lock_path(root: &Path) -> PathBuf {
    root.join("buffer.jsonl.lock")
}

/// `disabled` — the tombstone.
#[must_use]
pub fn tombstone_path(root: &Path) -> PathBuf {
    root.join("disabled")
}

/// `install_id.json`.
#[must_use]
pub fn install_id_path(root: &Path) -> PathBuf {
    root.join("install_id.json")
}

/// `state.json`.
#[must_use]
pub fn state_path(root: &Path) -> PathBuf {
    root.join("state.json")
}

/// Whether the tombstone is present.
///
/// Re-checked on **every** append and immediately before **every** send. This is
/// what makes `codewhale config set telemetry false` — an external write by
/// another process — observable to a session that is already running.
#[must_use]
pub fn tombstone_present(root: &Path) -> bool {
    tombstone_path(root).exists()
}

/// Read the exact tombstone generation, or `None` when collection has never
/// been disabled in this home.
pub(crate) fn tombstone_generation(root: &Path) -> Result<Option<TombstoneGeneration>> {
    let path = tombstone_path(root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(
                anyhow::Error::new(error).context(format!("failed to open {}", path.display()))
            );
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_TOMBSTONE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 > MAX_TOMBSTONE_BYTES {
        anyhow::bail!("{} exceeds the tombstone size limit", path.display());
    }
    Ok(Some(TombstoneGeneration(bytes)))
}

/// Create the telemetry directory `0700`, if it is missing.
pub fn ensure_dir(root: &Path) -> Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(root)
        .with_context(|| format!("failed to create {}", root.display()))
}

#[cfg(unix)]
fn secure(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .context("failed to restrict telemetry file permissions")
}

#[cfg(not(unix))]
fn secure(_file: &File) -> Result<()> {
    Ok(())
}

/// Append one serialized event or batch to `path`.
///
/// Returns `None` — never an error — when the tombstone is present, the privacy
/// lock is held, the line would not fit in one atomic write, or any filesystem
/// step fails. The lock acquisition is non-blocking, including on the panic and
/// signal paths.
pub fn append(root: &Path, path: &Path, line: &str) -> Option<()> {
    append_with_limit(root, path, line, MAX_LINE_BYTES, Turn::Never)
}

pub(crate) fn append_in_turn(root: &Path, path: &Path, line: &str) -> Option<()> {
    append_with_limit(root, path, line, MAX_LINE_BYTES, Turn::Wait)
}

/// Append a line that is too large for one atomic `write(2)`, serialising
/// against other writers with the compaction lock instead.
///
/// Only the dry-run sink uses this: a whole batch does not fit under
/// `PIPE_BUF`, and the flush path is neither the panic hook nor the signal
/// handler, so it may wait for a sibling thread. Another process holding the
/// lock still drops the batch, which is the same fail-open behavior as a failed
/// POST.
pub fn append_locked(root: &Path, path: &Path, line: &str) -> Option<()> {
    append_with_limit(root, path, line, MAX_BYTES as usize, Turn::Wait)
}

fn append_with_limit(root: &Path, path: &Path, line: &str, limit: usize, turn: Turn) -> Option<()> {
    let bytes = line.as_bytes();
    if bytes.is_empty() || bytes.len() + 1 > limit {
        return None;
    }

    let mut buf = Vec::with_capacity(bytes.len() + 1);
    buf.extend_from_slice(bytes);
    buf.push(b'\n');

    let operation = || append_under_lock(root, path, &buf);
    let attempt = match turn {
        Turn::Never => try_with_lock(root, operation),
        Turn::Wait => try_with_lock_in_turn(root, operation),
    };
    let wrote = attempt.ok().flatten().unwrap_or(false);
    if !wrote {
        return None;
    }

    enforce_ring(root, path);
    Some(())
}

/// Append bytes while the caller holds the sibling privacy lock.
fn append_under_lock(root: &Path, path: &Path, buf: &[u8]) -> Result<bool> {
    if tombstone_present(root) {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    secure(&file)?;
    // One `write(2)`, not `write_fmt` and not two calls: a split write is what
    // a concurrent appender would interleave with.
    (&file)
        .write_all(buf)
        .with_context(|| format!("failed to append to {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(true)
}

pub(crate) fn append_header(root: &Path, path: &Path, line: &str) -> Result<bool> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    append_under_lock(root, path, &buf)
}

fn keep_spool_header(path: &Path, mut lines: Vec<String>) -> Vec<String> {
    use std::io::BufRead as _;
    let Ok(file) = File::open(path) else {
        return lines;
    };
    let mut first = String::new();
    if std::io::BufReader::new(file.take(MAX_LINE_BYTES as u64 + 2))
        .read_line(&mut first)
        .is_err()
    {
        return lines;
    }
    let first = first.trim_end().to_string();
    if first.is_empty() || !crate::spool::is_header_line(&first) || lines.first() == Some(&first) {
        return lines;
    }
    lines.insert(0, first);
    let size = |lines: &[String]| lines.iter().map(|l| l.len() as u64 + 1).sum::<u64>();
    while lines.len() > 1 && (lines.len() > MAX_EVENTS || size(&lines) > MAX_BYTES) {
        lines.remove(1);
    }
    lines
}

/// Keep the newest [`MAX_EVENTS`] lines and at most [`MAX_BYTES`], under the
/// compaction lock. On lock contention this cycle is skipped: the next append
/// tries again, and the cap is a ceiling on disk footprint, not an invariant
/// that must hold at every instant.
fn enforce_ring(root: &Path, path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let len = meta.len();
    if len < PROBE_BYTES {
        return;
    }
    let _ = try_with_lock(root, || {
        // Re-read under the same lock as wipe. Reusing a snapshot captured
        // before a concurrent wipe would resurrect the records it truncated.
        if tombstone_present(root) {
            return Ok(());
        }
        let Ok(meta) = fs::metadata(path) else {
            return Ok(());
        };
        let len = meta.len();
        if len < PROBE_BYTES {
            return Ok(());
        }
        let lines = keep_spool_header(path, read_ring_tail(path)?);
        let mut body = lines.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        if body.len() as u64 == len {
            return Ok(());
        }
        rewrite(path, body.as_bytes())
    });
}

fn read_ring_tail(path: &Path) -> Result<Vec<String>> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a regular telemetry sink",
        path.display()
    );
    let start = metadata.len().saturating_sub(MAX_BYTES);
    let mut partial = false;
    if start > 0 {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        partial = previous[0] != b'\n';
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_BYTES) as usize);
    (&mut file).take(MAX_BYTES).read_to_end(&mut bytes)?;
    let contents = if partial {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |index| &bytes[index + 1..])
    } else {
        &bytes
    };
    let mut lines = Vec::new();
    let mut retained = 0;
    for line in contents.split(|byte| *byte == b'\n').rev() {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let size = line.len() as u64 + 1;
        if lines.len() == MAX_EVENTS || retained + size > MAX_BYTES {
            break;
        }
        retained += size;
        lines.push(line.to_string());
    }
    lines.reverse();
    Ok(lines)
}

/// Replace `path` atomically through a sibling temp file in the same directory.
fn rewrite(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("failed to stage a rewrite of {}", path.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("failed to write a rewrite of {}", path.display()))?;
    tmp.flush()
        .with_context(|| format!("failed to flush a rewrite of {}", path.display()))?;
    secure(tmp.as_file())?;
    tmp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to persist {}", path.display()))?;
    Ok(())
}

/// Open (creating if needed) the sibling lock file.
fn open_lock(root: &Path) -> Result<File> {
    ensure_dir(root)?;
    let path = lock_path(root);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // The file is only a lock handle; its contents are never read and
        // truncating it would race other holders for no benefit.
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    secure(&file)?;
    Ok(file)
}

#[derive(Clone, Copy)]
enum Turn {
    Never,
    Wait,
}

fn process_gate(root: &Path) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    GATES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

fn wait_turn(gate: &Mutex<()>) -> MutexGuard<'_, ()> {
    gate.lock().unwrap_or_else(PoisonError::into_inner)
}

fn try_flock<T>(root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<Option<T>> {
    let file = open_lock(root)?;
    let mut lock = fd_lock::RwLock::new(file);
    match lock.try_write() {
        Ok(_guard) => operation().map(Some),
        Err(_) => Ok(None),
    }
}

fn with_flock<T>(
    root: &Path,
    timeout: Option<Duration>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut lock = fd_lock::RwLock::new(open_lock(root)?);
    let Some(timeout) = timeout else {
        let _guard = lock
            .write()
            .context("failed to acquire the telemetry privacy lock")?;
        return operation();
    };
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_write() {
            Ok(_guard) => return operation(),
            Err(_) if Instant::now() < deadline => std::thread::sleep(LOCK_RETRY_INTERVAL),
            Err(_) => anyhow::bail!("the telemetry compaction lock stayed busy"),
        }
    }
}

/// Run `operation` holding the exclusive compaction lock.
///
/// Startup arming, the opt-out wipe, and the spool header write use this.
/// `flock` has no timeout of its own, so a suspended process holding the lock
/// would wedge every launch behind it; the wait is bounded by
/// `PRIVACY_LOCK_BUDGET` and a still-busy lock returns an error, which every
/// caller treats as skip-and-continue rather than fatal.
pub fn with_lock<T>(root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let gate = process_gate(root);
    let _turn = wait_turn(&gate);
    with_flock(root, Some(PRIVACY_LOCK_BUDGET), operation)
}

/// Run `operation` holding the exclusive compaction lock if it is free.
///
/// Returns `Ok(None)` when the lock is held elsewhere, by this process or
/// another. Never blocks.
pub fn try_with_lock<T>(root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<Option<T>> {
    let gate = process_gate(root);
    let _turn = match gate.try_lock() {
        Ok(turn) => turn,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return Ok(None),
    };
    try_flock(root, operation)
}

pub(crate) fn try_with_lock_in_turn<T>(
    root: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    let gate = process_gate(root);
    let _turn = wait_turn(&gate);
    try_flock(root, operation)
}

/// Read the newest bounded tail from `path`, skipping invalid UTF-8 lines.
///
/// `std::process::exit` on the signal path can truncate a concurrent write, so
/// the last line may be a partial JSON document. Skipping unparseable lines is
/// the whole tolerance: a drain must never fail because one record was cut.
#[must_use]
pub fn read_lines(path: &Path) -> Vec<String> {
    read_ring_tail(path).unwrap_or_default()
}

/// Truncate a file to zero length, leaving the inode in place. A missing file
/// is not an error.
pub fn truncate(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    secure(&file)?;
    Ok(())
}

/// Wipe every trace of collection, leaving a permanent tombstone.
///
/// Order matters and is the whole of the guarantee:
///
/// 1. take the bounded lock — this is not an exit path;
/// 2. write the tombstone **first**, and never remove it here — a lock that
///    stays busy still installs the tombstone on the error path;
/// 3. truncate `buffer.jsonl` and `dryrun.jsonl` — do **not** unlink them, and
///    never unlink the privacy lock file — and delete every spool;
/// 4. remove `install_id.json` and `state.json`.
///
/// If any step after the tombstone fails, the error is returned and the caller
/// logs it — but the tombstone alone already makes the buffer permanently
/// undrainable, so a failed wipe fails **closed**.
pub fn wipe(root: &Path) -> Result<()> {
    wipe_with_timeout(root, Some(PRIVACY_LOCK_BUDGET))
}

pub fn wipe_after_preference_change(root: &Path) -> Result<()> {
    wipe_with_timeout(root, None)
}

fn wipe_with_timeout(root: &Path, timeout: Option<Duration>) -> Result<()> {
    let gate = process_gate(root);
    let _turn = wait_turn(&gate);
    match with_flock(root, timeout, || {
        write_tombstone(root)?;

        let mut failure: Option<anyhow::Error> = None;
        for path in [buffer_path(root), dryrun_path(root)] {
            if let Err(error) = truncate(&path) {
                failure.get_or_insert(error);
            }
        }
        for path in [install_id_path(root), state_path(root)] {
            if path.exists()
                && let Err(error) = fs::remove_file(&path)
            {
                failure.get_or_insert(
                    anyhow::Error::new(error)
                        .context(format!("failed to remove {}", path.display())),
                );
            }
        }
        if let Err(error) = fs::remove_dir_all(crate::spool::dir(root))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            failure.get_or_insert(anyhow::Error::new(error).context("failed to remove the spools"));
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }) {
        Ok(()) => Ok(()),
        Err(error) => {
            write_tombstone(root)?;
            Err(error)
        }
    }
}

fn write_tombstone(root: &Path) -> Result<()> {
    let tombstone = tombstone_path(root);
    // A tombstone generation identifies one durable opted-out period. Keep
    // it stable across later launches that re-observe the same persistent
    // choice; re-enable removes the file, so the next real opt-out creates
    // a naturally distinct generation. An unreadable or oversized file is
    // repaired in the fail-closed direction by replacing it here; legacy
    // empty tombstones remain valid stable generations.
    if tombstone_generation(root).ok().flatten().is_some() {
        return Ok(());
    }
    let mut tmp = tempfile::NamedTempFile::new_in(root)
        .with_context(|| format!("failed to stage {}", tombstone.display()))?;
    secure(tmp.as_file())?;
    tmp.write_all(uuid::Uuid::new_v4().to_string().as_bytes())
        .with_context(|| format!("failed to write {}", tombstone.display()))?;
    tmp.as_file()
        .sync_data()
        .with_context(|| format!("failed to sync {}", tombstone.display()))?;
    tmp.persist(&tombstone)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install {}", tombstone.display()))?;
    Ok(())
}

/// Clear the tombstone, deleting every spool when there was one, and truncate
/// the pre-spool buffer.
///
/// Called by `init` on every arming. Both the exact tombstone generation and a
/// fresh durable permission check must still match while the wipe lock is held.
/// A stale buffer left by an earlier run or by a bug cannot enter the new
/// process's batch.
pub(crate) fn arm(
    root: &Path,
    observed_generation: Option<&TombstoneGeneration>,
    permission_still_enabled: impl Fn() -> bool,
) -> Result<()> {
    ensure_dir(root)?;
    with_lock(root, || {
        let current_generation = tombstone_generation(root)?;
        if current_generation.as_ref() != observed_generation {
            anyhow::bail!("telemetry permission changed before arming");
        }
        if !permission_still_enabled() {
            anyhow::bail!("telemetry permission is no longer enabled");
        }
        let tombstone = tombstone_path(root);
        if observed_generation.is_some() && tombstone.exists() {
            if tombstone_generation(root)?.as_ref() != observed_generation {
                anyhow::bail!("telemetry permission changed before arming");
            }
            let _ = fs::remove_dir_all(crate::spool::dir(root));
            fs::remove_file(&tombstone)
                .with_context(|| format!("failed to remove {}", tombstone.display()))?;
            if !permission_still_enabled() {
                let _ = write_tombstone(root);
                anyhow::bail!("telemetry permission is no longer enabled");
            }
        }
        if tombstone.exists() {
            anyhow::bail!("telemetry permission changed before arming");
        }
        truncate(&buffer_path(root))
    })
}

//! Workspace drive lease: at most one driving engine per workspace, process-wide.
//!
//! The store process-owner lock serializes writers of one *store*, but a
//! workspace is shared across stores — a per-workspace store (the VS Code
//! extension), a per-session store (the interactive TUI) and the user-level
//! default store can all point at the same working tree, the same snapshot
//! side-repo (keyed by canonical workspace path) and the same git index, with
//! nothing serializing them across processes. The lease lifts the exclusivity
//! unit to the workspace itself.
//!
//! Drive vs watch: only a *driving* engine takes the lease. Watch-only
//! observers never do — their token intent already forbids mutation, and they
//! attach to the owner that holds the lease rather than admitting their own
//! workspace scope.
//!
//! The mechanism mirrors the store process-owner lock: an fd lock the kernel
//! releases when the holder dies, plus a small holder record a contender can
//! read to name the holder in a legible refusal. The kernel lock is the
//! authority; recorded holder facts never authorize replacing its file.
//!
//! Two capabilities are involved and they are asked about separately. The
//! directory admission needs ownership it can *prove*; HarmonyOS hmdfs — the
//! 2in1 user directory — synthesises the owner and mode of every object created
//! on it, so that proof can never hold there. Exclusivity itself is the kernel
//! lock, which is a different capability and does hold there. Admission
//! therefore relaxes only the proof, and the lease is still taken; refusing on
//! the proof turned a limit on evidence into a failed engine start.
//!
//! A mount that refuses the lock itself is the one case where exclusivity is
//! genuinely unavailable. It is reported to the caller — three outcomes, of
//! which only contention is a refusal — so that decision is named rather than
//! silently taken.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use codewhale_config::private_directory::PrivateDirectory;

/// Upper bound on the bytes read back from a lease holder record. Also the
/// offset the Windows byte-range lock starts at, so a contender can still read
/// the record of the holder it is contending with.
const HOLDER_RECORD_MAX_BYTES: u64 = 512;

/// Facts about who holds a workspace drive lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLeaseHolder {
    pub pid: u32,
    /// Liveness token of the holder process (`unix_process_start` shape), when
    /// recorded. Absent on Windows and on records written before it existed.
    pub process_start: Option<String>,
    /// Human-readable client label, e.g. `vscode`, `desktop`, `tui`.
    pub client: Option<String>,
    /// Store root of the holding engine, for legible refusal messages.
    pub store_root: Option<PathBuf>,
    /// Wall-clock acquisition time (ms since UNIX_EPOCH).
    pub since_ms: u64,
}

impl WorkspaceLeaseHolder {
    fn to_record(&self) -> String {
        let mut record = format!("version=1\npid={}\nsince_ms={}\n", self.pid, self.since_ms);
        if let Some(process_start) = &self.process_start {
            record.push_str(&format!("process_start={process_start}\n"));
        }
        if let Some(client) = &self.client {
            record.push_str(&format!("client={}\n", client.replace(['\n', '='], " ")));
        }
        if let Some(store_root) = &self.store_root {
            record.push_str(&format!(
                "store_root={}\n",
                store_root.to_string_lossy().replace(['\n', '='], " ")
            ));
        }
        record
    }

    fn from_record(text: &str) -> Option<Self> {
        let mut version = None;
        let mut pid = None;
        let mut process_start = None;
        let mut client = None;
        let mut store_root = None;
        let mut since_ms = None;
        for line in text.lines() {
            match line.split_once('=') {
                Some(("version", value)) => version = value.parse::<u32>().ok(),
                Some(("pid", value)) => pid = value.parse::<u32>().ok(),
                Some(("process_start", value)) => process_start = Some(value.to_string()),
                Some(("client", value)) => client = Some(value.to_string()),
                Some(("store_root", value)) => store_root = Some(PathBuf::from(value)),
                Some(("since_ms", value)) => since_ms = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        if version != Some(1) {
            return None;
        }
        Some(Self {
            pid: pid?,
            process_start,
            client,
            store_root,
            since_ms: since_ms?,
        })
    }

    /// How long the lease has been held, best effort.
    pub fn held_for(&self) -> Duration {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH + Duration::from_millis(self.since_ms))
            .unwrap_or_default()
    }
}

/// A shared process-wide drive lease handle.
///
/// The first admission for a workspace acquires the OS lease; later admissions
/// in the *same process* (a successor manager, a second engine in one test
/// process) share it by counted acquisition instead of refusing each other.
/// The last admitted handle drop releases it.
pub struct SharedWorkspaceDriveLease {
    inner: std::sync::Arc<LeaseInner>,
}

struct LeaseInner {
    /// The OS lease plus the live shared-admission count, or nothing when a
    /// foreign process held it at (first) acquire time.
    state: std::sync::Mutex<Option<(WorkspaceDriveLease, usize)>>,
}

/// Why the lease could not be taken even though no other process holds it.
///
/// This is a capability the filesystem lacks, not a refusal: the caller decides
/// whether to run without workspace exclusivity, and the reason is carried so
/// that decision can be explained to the user instead of looking like success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseUnavailability {
    /// The lease state root whose filesystem could not carry the lease.
    pub directory: PathBuf,
    /// What the mount did, in terms a user can act on.
    pub reason: String,
}

impl LeaseUnavailability {
    fn new(directory: &Path, reason: impl Into<String>) -> Self {
        Self {
            directory: directory.to_path_buf(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for LeaseUnavailability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cannot provide workspace exclusivity: {}",
            self.directory.display(),
            self.reason
        )
    }
}

/// One raw attempt to take a workspace's drive lease.
///
/// Public because [`WorkspaceDriveLease::try_acquire`] is: it is how an
/// embedder that wants the lease without the shared-slot bookkeeping asks.
pub enum LeaseAttempt {
    Acquired(WorkspaceDriveLease),
    /// A foreign process holds the kernel lock.
    Contended,
    /// No lock can be taken here at all.
    Unsupported(LeaseUnavailability),
}

/// The outcome of admitting one driving engine for a workspace.
pub enum LeaseAdmission {
    /// This process holds (or shares) the workspace drive lease.
    Held(SharedWorkspaceDriveLease),
    /// Another process drives this workspace: refuse and name its holder.
    Contended,
    /// This filesystem cannot carry an exclusive lease. Running without one is
    /// the caller's decision, and must be reported rather than assumed.
    Unsupported(LeaseUnavailability),
}

/// The held handle is a guard, not a fact worth printing; the other two
/// outcomes are what a caller or a test needs to see.
impl std::fmt::Debug for LeaseAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held(_) => f.write_str("Held(..)"),
            Self::Contended => f.write_str("Contended"),
            Self::Unsupported(unavailable) => {
                f.debug_tuple("Unsupported").field(unavailable).finish()
            }
        }
    }
}

impl SharedWorkspaceDriveLease {
    /// Acquire (or join) this process's drive lease for the workspace.
    ///
    /// [`LeaseAdmission::Contended`] means a *foreign process* holds it — the
    /// caller refuses with the holder's facts from
    /// [`WorkspaceDriveLease::read_holder`]. [`LeaseAdmission::Unsupported`]
    /// means the filesystem cannot carry the lease at all, which is a different
    /// answer and a different decision. Same-process joins never fail and never
    /// re-lock.
    pub fn acquire(
        workspace: &Path,
        client: Option<&str>,
        store_root: Option<&Path>,
    ) -> Result<LeaseAdmission> {
        let lease_key = WorkspaceDriveLease::lease_path(workspace)?;
        let slot = {
            let mut registry = registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(slot) = registry.get(&lease_key) {
                slot.clone()
            } else {
                let lease = match WorkspaceDriveLease::try_acquire(workspace, client, store_root)? {
                    LeaseAttempt::Acquired(lease) => lease,
                    LeaseAttempt::Contended => return Ok(LeaseAdmission::Contended),
                    LeaseAttempt::Unsupported(unavailable) => {
                        return Ok(LeaseAdmission::Unsupported(unavailable));
                    }
                };
                let inner = std::sync::Arc::new(LeaseInner {
                    state: std::sync::Mutex::new(Some((lease, 1))),
                });
                registry.insert(lease_key, inner.clone());
                return Ok(LeaseAdmission::Held(Self { inner }));
            }
        };
        // The returned handle is an owned clone, so moving it out never
        // borrows the guard: the state guard stays held across `try_acquire`,
        // which serializes two threads of one process racing for the same
        // workspace and makes the slot — not the fd lock — the in-process
        // authority.
        let inner = slot.clone();
        let mut state = slot
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_lease, count)) = state.as_mut() {
            *count += 1;
            return Ok(LeaseAdmission::Held(Self { inner }));
        }
        let lease = match WorkspaceDriveLease::try_acquire(workspace, client, store_root)? {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Contended => return Ok(LeaseAdmission::Contended),
            LeaseAttempt::Unsupported(unavailable) => {
                return Ok(LeaseAdmission::Unsupported(unavailable));
            }
        };
        *state = Some((lease, 1));
        Ok(LeaseAdmission::Held(Self { inner }))
    }
}

impl Drop for SharedWorkspaceDriveLease {
    fn drop(&mut self) {
        // The decrement and the removal of the OS lease happen in *one*
        // critical section. Deciding under the lock and taking afterwards
        // leaves a window in which a concurrent `acquire` observes the zero
        // count, joins the handle, and then loses the OS lease out from under
        // itself — two engines driving one workspace, which is the whole
        // failure this lease exists to prevent.
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        let Some((_lease, count)) = state.as_mut() else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            // Drop the OS lease *inside* this section. Deciding here and
            // releasing after the guard is gone leaves a window in which a
            // same-process `acquire` finds the slot empty while the OS lock is
            // still held, and is refused — two engine scopes of one process
            // refusing each other, which is precisely what the shared slot
            // exists to prevent.
            state.take();
        }
    }
}

/// One slot per workspace, kept for the life of the process.
///
/// Removing a slot on release would race its owner: a contender that read the
/// map before the removal could populate the slot that is about to be deleted,
/// and the next contender would then create a second slot for the same
/// workspace. A free slot holds nothing but an empty `Option`, so keeping it
/// costs one map entry per workspace this process has successfully acquired.
/// Refused first acquisitions do not enter the registry.
type LeaseRegistry =
    std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<LeaseInner>>>;

fn registry() -> &'static LeaseRegistry {
    static REGISTRY: std::sync::OnceLock<LeaseRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// A held workspace drive lease. Dropping releases the kernel lock.
pub struct WorkspaceDriveLease {
    file: File,
    _base: PrivateDirectory,
    _directory: PrivateDirectory,
}

impl WorkspaceDriveLease {
    /// The lease path for a workspace: alongside the snapshot side-repo, using
    /// the same canonical-path hashing (`snapshot::paths`), so worktrees of one
    /// checkout share a project bucket and one worktree maps to exactly one
    /// lease.
    pub fn lease_path(workspace: &Path) -> Result<PathBuf> {
        let base = lease_state_base()?;
        let canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let project = codewhale_paths::strip_worktree_suffix(&canonical);
        Ok(base
            .join(codewhale_paths::stable_hex(&project))
            .join(codewhale_paths::stable_hex(&canonical))
            .with_extension("lease"))
    }

    /// Try once to acquire the drive lease for this process.
    ///
    /// [`LeaseAttempt::Contended`] means the kernel lock is held: use
    /// [`Self::read_holder`] to name the recorded holder. An unsupported mount
    /// is reported, not refused.
    pub fn try_acquire(
        workspace: &Path,
        client: Option<&str>,
        store_root: Option<&Path>,
    ) -> Result<LeaseAttempt> {
        let path = Self::lease_path(workspace)?;
        let parent = path.parent().context("Workspace lease has no parent")?;
        let state_root = parent
            .parent()
            .context("Workspace lease has no state root")?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Workspace lease name is invalid")?;
        // Admission relaxes *only* the proof that the directory is
        // current-user-only, and only where the mount cannot express it. The
        // exclusivity this function exists for is the kernel lock taken below,
        // which is a different capability: on HarmonyOS hmdfs `flock` works
        // and only the reported owner is unusable, so giving up the lease here
        // would discard a guarantee that still holds. A mount that truly has
        // no advisory locks is reported further down, after the lock says so.
        let base = PrivateDirectory::admit_trusting_mount(state_root)?;
        let directory = PrivateDirectory::admit_trusting_mount(parent)?;
        let file = directory.open_internal_file(name)?;
        #[cfg(unix)]
        let contention = {
            use std::os::fd::AsRawFd as _;
            // SAFETY: `file` owns a valid descriptor retained by this guard.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            }
        };
        #[cfg(windows)]
        let contention = {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::LockFile;
            let (offset_low, offset_high, length_low, length_high) =
                (HOLDER_RECORD_MAX_BYTES as u32, 0, u32::MAX, 0x7FFF_FFFF);
            // SAFETY: `file` owns a valid handle retained by this guard.
            if unsafe {
                LockFile(
                    file.as_raw_handle() as _,
                    offset_low,
                    offset_high,
                    length_low,
                    length_high,
                )
            } == 0
            {
                Some(std::io::Error::last_os_error())
            } else {
                None
            }
        };
        #[cfg(not(any(unix, windows)))]
        let contention: Option<std::io::Error> = None;
        if let Some(error) = contention {
            if codewhale_util::lock::is_contention(&error) {
                return Ok(LeaseAttempt::Contended);
            }
            // No advisory locks on this mount at all: two drivers cannot be
            // excluded here, but that is a property of the filesystem, not a
            // holder. `EWOULDBLOCK` was handled above and stays contention.
            #[cfg(unix)]
            if codewhale_util::lock::advisory_locking_unavailable(&error) {
                return Ok(LeaseAttempt::Unsupported(LeaseUnavailability::new(
                    state_root,
                    format!("this filesystem refuses advisory file locks ({error})"),
                )));
            }
            return Err(error).context("Failed to lock workspace lease");
        }
        #[cfg(any(unix, windows))]
        anyhow::ensure!(
            base.is_at_selected_path()? && directory.is_at_selected_path()?,
            "Workspace lease directory identity changed"
        );
        let selected = directory.open_internal_file(name)?;
        anyhow::ensure!(
            PrivateDirectory::same_file_identity(&file, &selected)?,
            "Workspace lease file identity changed"
        );
        let holder = WorkspaceLeaseHolder {
            pid: std::process::id(),
            process_start: current_process_start(),
            client: client.map(ToString::to_string),
            store_root: store_root.map(Path::to_path_buf),
            since_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64),
        };
        let mut lease = Self {
            file,
            _base: base,
            _directory: directory,
        };
        // Best effort: holder diagnostics must never fail the lease holder.
        let _ = lease
            .file
            .set_len(0)
            .and_then(|()| lease.file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| lease.file.write_all(holder.to_record().as_bytes()));
        Ok(LeaseAttempt::Acquired(lease))
    }

    /// Read the recorded holder, if any. Missing or malformed data is `None`;
    /// filesystem errors remain explicit, and the record may be stale.
    pub fn read_holder(workspace: &Path) -> Result<Option<WorkspaceLeaseHolder>> {
        let path = Self::lease_path(workspace)?;
        let parent = path.parent().context("Workspace lease has no parent")?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Workspace lease name is invalid")?;
        // Naming a holder is a read. On a mount that cannot express ownership
        // the strict proof fails for every directory, and refusing would hide
        // the holder's pid behind "record unreadable" on exactly the platform
        // where a user most needs to know who holds the workspace.
        let directory = match PrivateDirectory::inspect_trusting_mount(parent) {
            Ok(directory) => directory,
            Err(error)
                if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                }) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error).context("Failed to inspect workspace lease directory"),
        };
        #[cfg(unix)]
        let bytes =
            match directory.read_private_receipt(name, HOLDER_RECORD_MAX_BYTES as usize - 1)? {
                Some((bytes, _file)) => bytes,
                None => return Ok(None),
            };
        #[cfg(windows)]
        let bytes = {
            use std::io::Read as _;
            let Some(file) = directory.open_owned_file_for_read(name)? else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            file.take(HOLDER_RECORD_MAX_BYTES).read_to_end(&mut bytes)?;
            bytes
        };
        #[cfg(not(any(unix, windows)))]
        anyhow::bail!("Workspace drive lease is unsupported on this platform");
        #[cfg(any(unix, windows))]
        {
            anyhow::ensure!(
                directory.is_at_selected_path()?,
                "Workspace lease directory identity changed"
            );
            let text = std::str::from_utf8(&bytes)?;
            Ok(WorkspaceLeaseHolder::from_record(text))
        }
    }
}

impl Drop for WorkspaceDriveLease {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: Drop runs only while `file` still owns this descriptor.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::UnlockFile;
            let (offset_low, offset_high, length_low, length_high) =
                (HOLDER_RECORD_MAX_BYTES as u32, 0, u32::MAX, 0x7FFF_FFFF);
            // SAFETY: Drop runs only while `file` still owns this handle.
            unsafe {
                UnlockFile(
                    self.file.as_raw_handle() as _,
                    offset_low,
                    offset_high,
                    length_low,
                    length_high,
                );
            }
        }
    }
}

#[cfg(all(test, unix))]
fn holder_is_dead(path: &Path) -> bool {
    use codewhale_config::private_directory::{UnixProcessStatus, unix_process_status};

    let Some(holder) =
        WorkspaceLeaseHolder::from_record(&std::fs::read_to_string(path).unwrap_or_default())
    else {
        return false;
    };
    let Some(recorded_start) = holder.process_start.as_deref() else {
        return false;
    };
    if holder.pid == std::process::id() {
        // Same-process contention: the record is either ours (a live sibling
        // scope holds the lock — alive by definition) or a same-pid
        // predecessor. Distinguish by creation token: a different one means
        // the pid was recycled and the previous holder is gone.
        return current_process_start().as_deref() != Some(recorded_start);
    }
    match unix_process_status(holder.pid) {
        Ok(UnixProcessStatus::Present { start }) => start != recorded_start,
        Ok(UnixProcessStatus::Absent) => true,
        // Identity or liveness is uncertain (EPERM, a foreign principal, a
        // pid we cannot probe). Never convert uncertainty into death.
        Err(_) => false,
    }
}

/// This process's liveness token, best effort.
#[cfg(unix)]
fn current_process_start() -> Option<String> {
    use codewhale_config::private_directory::{UnixProcessStatus, unix_process_status};

    match unix_process_status(std::process::id()) {
        Ok(UnixProcessStatus::Present { start }) => Some(start),
        _ => None,
    }
}

/// Windows records no creation token; the byte-range lock itself is the
/// liveness mechanism.
#[cfg(not(unix))]
fn current_process_start() -> Option<String> {
    None
}

fn lease_state_base() -> Result<PathBuf> {
    #[cfg(test)]
    {
        if let Some(base) = crate::test_support::unsealed_state_dir("workspace-leases") {
            PrivateDirectory::open(&base)?;
            return Ok(base);
        }
    }
    codewhale_config::resolve_state_dir("workspace-leases")
        .context("Failed to resolve workspace lease state dir")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cw-ws-lease-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).expect("create workspace");
        dir
    }

    fn write_record(workspace: &Path, holder: &WorkspaceLeaseHolder) -> PathBuf {
        let path = WorkspaceDriveLease::lease_path(workspace).expect("lease path");
        let parent = path.parent().expect("lease dir");
        let _base = PrivateDirectory::open(parent.parent().expect("lease state root"))
            .expect("private lease state root");
        let directory = PrivateDirectory::open(parent).expect("private lease dir");
        let mut file = directory
            .open_internal_file(path.file_name().unwrap().to_str().unwrap())
            .expect("private lease file");
        file.set_len(0).expect("truncate holder record");
        file.write_all(holder.to_record().as_bytes())
            .expect("write holder record");
        path
    }

    fn expect_held(admission: LeaseAdmission) -> SharedWorkspaceDriveLease {
        match admission {
            LeaseAdmission::Held(lease) => lease,
            LeaseAdmission::Contended => panic!("expected a held lease, got contention"),
            LeaseAdmission::Unsupported(unavailable) => {
                panic!("expected a held lease, got {unavailable}")
            }
        }
    }

    #[test]
    fn acquire_then_contend_reports_holder() {
        let workspace = temp_workspace("contend");
        let first =
            WorkspaceDriveLease::try_acquire(&workspace, Some("tui"), None).expect("first acquire");
        assert!(
            matches!(first, LeaseAttempt::Acquired(_)),
            "first drive is admitted"
        );
        let second = WorkspaceDriveLease::try_acquire(&workspace, Some("vscode"), None)
            .expect("contend probe");
        assert!(
            matches!(second, LeaseAttempt::Contended),
            "second drive must be refused"
        );
        let holder = WorkspaceDriveLease::read_holder(&workspace)
            .expect("read holder")
            .expect("holder recorded");
        assert_eq!(holder.pid, std::process::id());
        assert_eq!(holder.client.as_deref(), Some("tui"));
        assert_eq!(holder.process_start, current_process_start());
        drop(first);
        let third =
            WorkspaceDriveLease::try_acquire(&workspace, Some("vscode"), None).expect("reacquire");
        assert!(
            matches!(third, LeaseAttempt::Acquired(_)),
            "lease frees after drop"
        );
    }

    #[test]
    fn shared_lease_reenters_within_a_process() {
        let workspace = temp_workspace("shared");
        let first = expect_held(
            SharedWorkspaceDriveLease::acquire(&workspace, Some("tui"), None)
                .expect("first shared acquire"),
        );
        // A second engine in the same process joins by refcount instead of
        // refusing — the lease's unit is the process.
        let second = expect_held(
            SharedWorkspaceDriveLease::acquire(&workspace, Some("vscode"), None)
                .expect("second shared acquire"),
        );
        drop(first);
        // Still held by the second handle: a raw probe must be refused.
        let refused =
            WorkspaceDriveLease::try_acquire(&workspace, Some("probe"), None).expect("probe");
        assert!(
            matches!(refused, LeaseAttempt::Contended),
            "lease still held while shared"
        );
        drop(second);
        let after = expect_held(
            SharedWorkspaceDriveLease::acquire(&workspace, Some("next"), None)
                .expect("reacquire after full release"),
        );
        drop(after);
    }

    #[test]
    fn shared_handles_survive_concurrent_acquire_and_release() {
        // The registry slot is shared by every handle for one workspace, so a
        // wrong lock order here shows up as a deadlock or a lost OS lease. Both
        // are matters of the slot being taken and cleared under the lock that
        // guards the count.
        let workspace = temp_workspace("churn");
        let mut workers = Vec::new();
        for _ in 0..8 {
            let workspace = workspace.clone();
            workers.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    let lease = expect_held(
                        SharedWorkspaceDriveLease::acquire(&workspace, Some("churn"), None)
                            .expect("acquire"),
                    );
                    drop(lease);
                }
            }));
        }
        for worker in workers {
            worker.join().expect("churn worker");
        }
        // Every handle is gone: the lease is free and a fresh holder records
        // itself rather than joining a slot nothing released.
        let lease = expect_held(
            SharedWorkspaceDriveLease::acquire(&workspace, Some("after"), None)
                .expect("acquire after churn"),
        );
        let holder = WorkspaceDriveLease::read_holder(&workspace)
            .expect("read holder")
            .expect("holder recorded");
        assert_eq!(holder.client.as_deref(), Some("after"));
        drop(lease);
    }

    /// The one branch that decides fatal from degraded. A mount with no
    /// advisory locks must never be reported as a second driver — that would
    /// keep the engine from starting over a property of the filesystem — and a
    /// second driver must never be downgraded to "run anyway", which would let
    /// two engines write one tree. The two predicates are disjoint by
    /// construction; this pins that.
    #[test]
    fn only_a_held_lock_is_contention_and_only_a_lockless_mount_is_unsupported() {
        use std::io::Error;

        let held = Error::from_raw_os_error(libc::EWOULDBLOCK);
        assert!(
            codewhale_util::lock::is_contention(&held),
            "a held lock is contention"
        );
        assert!(
            !codewhale_util::lock::advisory_locking_unavailable(&held),
            "a held lock is not a mount without locks"
        );
        for code in [
            libc::EPERM,
            libc::EACCES,
            libc::ENOTSUP,
            libc::EINVAL,
            libc::ENOSYS,
        ] {
            let absent = Error::from_raw_os_error(code);
            assert!(
                !codewhale_util::lock::is_contention(&absent),
                "{code} is the mount answering, not a holder"
            );
            assert!(
                codewhale_util::lock::advisory_locking_unavailable(&absent),
                "{code} means this mount has no advisory locks"
            );
        }
    }

    #[test]
    fn lease_path_is_stable_and_worktree_aware() {
        let workspace = temp_workspace("paths");
        let one = WorkspaceDriveLease::lease_path(&workspace).expect("path");
        let two = WorkspaceDriveLease::lease_path(&workspace).expect("path");
        assert_eq!(one, two);
        assert_eq!(one.extension().and_then(|ext| ext.to_str()), Some("lease"));
        let worktree = workspace.join(".worktrees").join("feat");
        std::fs::create_dir_all(&worktree).expect("create worktree");
        let sibling = workspace.join(".worktrees").join("other");
        std::fs::create_dir_all(&sibling).expect("create sibling");
        let a = WorkspaceDriveLease::lease_path(&worktree).expect("path");
        let b = WorkspaceDriveLease::lease_path(&sibling).expect("path");
        assert_ne!(a, b, "distinct worktrees get distinct leases");
        assert_eq!(
            a.parent().unwrap(),
            b.parent().unwrap(),
            "worktrees of one project share a bucket"
        );
    }

    #[test]
    fn holder_record_round_trips_and_needs_a_version() {
        let holder = WorkspaceLeaseHolder {
            pid: 4242,
            process_start: Some("macos:1:2".into()),
            client: Some("vscode".into()),
            store_root: Some(PathBuf::from("/tmp/store")),
            since_ms: 1_700_000_000_000,
        };
        assert_eq!(
            WorkspaceLeaseHolder::from_record(&holder.to_record()),
            Some(holder)
        );
        // A record without a version is not a record we trust.
        assert_eq!(
            WorkspaceLeaseHolder::from_record("pid=1\nsince_ms=0\n"),
            None
        );
    }

    #[test]
    fn absent_or_malformed_records_read_none() {
        let workspace = temp_workspace("records");
        assert_eq!(
            WorkspaceDriveLease::read_holder(&workspace).expect("absent"),
            None
        );
        let path = write_record(
            &workspace,
            &WorkspaceLeaseHolder {
                pid: 7,
                process_start: None,
                client: None,
                store_root: None,
                since_ms: 5,
            },
        );
        assert!(
            WorkspaceDriveLease::read_holder(&workspace)
                .expect("present")
                .is_some()
        );
        std::fs::write(&path, "not a record\n").expect("write malformed");
        assert_eq!(
            WorkspaceDriveLease::read_holder(&workspace).expect("malformed"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_provably_dead_holder_record_does_not_replace_a_locked_lease() {
        /// Spawn a child, report its pid and creation token while it is alive,
        /// then reap it. Returns the recorded facts and the lease path.
        fn holder_of_a_reaped_child(workspace: &Path) -> PathBuf {
            use codewhale_config::private_directory::{UnixProcessStatus, unix_process_status};

            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .spawn()
                .expect("spawn child");
            let pid = child.id();
            let start = match unix_process_status(pid) {
                Ok(UnixProcessStatus::Present { start }) => Some(start),
                other => panic!("live child must be present: {other:?}"),
            };
            child.wait().expect("reap child");
            let path = write_record(
                workspace,
                &WorkspaceLeaseHolder {
                    pid,
                    process_start: start,
                    client: Some("crashed".into()),
                    store_root: None,
                    since_ms: 0,
                },
            );
            assert!(holder_is_dead(&path), "a reaped holder is provably dead");
            path
        }

        let workspace = temp_workspace("dead");
        holder_of_a_reaped_child(&workspace);
        let first =
            WorkspaceDriveLease::try_acquire(&workspace, Some("next"), None).expect("acquire");
        assert!(
            matches!(first, LeaseAttempt::Acquired(_)),
            "an unlocked lease remains acquirable despite a stale record"
        );
        holder_of_a_reaped_child(&workspace);
        assert!(
            matches!(
                WorkspaceDriveLease::try_acquire(&workspace, Some("contender"), None)
                    .expect("contend"),
                LeaseAttempt::Contended
            ),
            "a stale holder record must not replace a held kernel lock"
        );
        drop(first);
        assert!(
            matches!(
                WorkspaceDriveLease::try_acquire(&workspace, Some("after"), None)
                    .expect("reacquire"),
                LeaseAttempt::Acquired(_)
            ),
            "the same lease is available after its holder releases it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_live_holder_with_a_matching_token_is_never_dead() {
        use codewhale_config::private_directory::{UnixProcessStatus, unix_process_status};

        let workspace = temp_workspace("live");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        let start = match unix_process_status(pid) {
            Ok(UnixProcessStatus::Present { start }) => start,
            other => panic!("live child must be present: {other:?}"),
        };
        let path = write_record(
            &workspace,
            &WorkspaceLeaseHolder {
                pid,
                process_start: Some(start),
                client: Some("driving".into()),
                store_root: None,
                since_ms: 0,
            },
        );
        assert!(
            !holder_is_dead(&path),
            "a live holder's recorded token must never read dead"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn a_record_without_a_liveness_token_is_never_taken_over() {
        let workspace = temp_workspace("no-token");
        let path = write_record(
            &workspace,
            &WorkspaceLeaseHolder {
                // A pid that cannot be probed at all.
                pid: u32::MAX,
                process_start: None,
                client: None,
                store_root: None,
                since_ms: 0,
            },
        );
        assert!(
            !holder_is_dead(&path),
            "without a liveness token, only a live-holder conclusion is safe"
        );
    }
}

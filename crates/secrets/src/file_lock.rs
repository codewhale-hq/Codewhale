//! Shared file-store write lock. Native uses the same fd-lock 4.0.4 and
//! persistent `<filename>.lock` protocol. Never delete or replace that inode.
//! Older writers bypassing this protocol cannot participate safely.
use crate::SecretsError;
use std::{fs, path::Path};

pub(crate) fn open_private(
    path: &Path,
    create: bool,
    role: StoreFile,
) -> Result<fs::File, SecretsError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err(std::io::Error::other("The secret store must be a regular file.").into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("The secret store must be a regular file.").into());
    }
    #[cfg(unix)]
    require_private_mode(&file, path, role)?;
    #[cfg(not(unix))]
    let _ = role;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(
                std::io::Error::other("The secret store cannot be a reparse point.").into(),
            );
        }
    }
    Ok(file)
}

/// Which of the store's two files is being opened.
///
/// The distinction decides whether a wider-than-`0600` mode is repaired or
/// refused, and it is not cosmetic: one file carries the secret and the other
/// carries nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreFile {
    /// `secrets.json`. A mode that let another user in means the bytes may no
    /// longer be ours, so the file is refused rather than quietly re-narrowed —
    /// repairing the mode would hide exactly the question the caller must
    /// answer. The refusal names the mode it saw.
    Content,
    /// The `<store>.lock` sibling. It holds no secret and exists only to
    /// serialise writers, so a wider mode is hygiene: it is narrowed in place
    /// before use, and only a filesystem that cannot hold modes is accepted as
    /// it is.
    Lock,
}

/// Make sure a secret-store file is readable by its owner only.
///
/// 1. Already `0o600` or narrower: done, no syscall and no probe.
/// 2. A lock file that is wider: narrow it through the handle that was opened
///    and look again. A repair that does not take effect falls through.
/// 3. Still wider, and the mount cannot apply Unix mode bits — HarmonyOS hmdfs
///    reports the logged-in user as the owner of every object, so a process is
///    never the owner of what it creates and `chmod` answers `EPERM`; FUSE and
///    some network mounts refuse it too: accept with a warning, because
///    refusing leaves that platform with no store at all and the fallback is a
///    plaintext key in `config.toml`, a wider exposure.
/// 4. Still wider on a filesystem that does store modes: a real exposure, and
///    still refused.
#[cfg(unix)]
pub(crate) fn require_private_mode(
    file: &fs::File,
    path: &Path,
    role: StoreFile,
) -> Result<(), SecretsError> {
    use std::os::unix::fs::PermissionsExt;

    let observed = file.metadata()?.permissions().mode() & 0o777;
    if observed & 0o077 == 0 {
        return Ok(());
    }
    // Narrowing through the handle keeps it on the object that was opened, so a
    // substituted path cannot be the thing that gets changed. Only the lock is
    // repaired; see [`StoreFile`] for why the content is not.
    let narrowed = match role {
        StoreFile::Lock => file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| file.metadata())
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .ok(),
        StoreFile::Content => None,
    };
    // The probe costs a create, a stat and an unlink, so it runs only when the
    // mode is still wide after any repair.
    let mount_cannot_hold_modes = match narrowed {
        Some(mode) if mode & 0o077 == 0 => false,
        _ => mount_ignores_unix_modes(path),
    };
    match classify_store_mode(observed, narrowed, mount_cannot_hold_modes) {
        StoreMode::Narrowed => Ok(()),
        StoreMode::Unavoidable { mode } => {
            tracing::warn!(
                "{} has mode {mode:o} and its filesystem does not apply Unix mode bits; \
                 relying on the mount's own access control for the secret store",
                path.display()
            );
            Ok(())
        }
        StoreMode::Exposed { mode } => Err(SecretsError::InsecurePermissions {
            path: path.into(),
            mode,
        }),
    }
}

/// What a store file's mode turned out to be worth, given what the mount did
/// when asked for `0o600`.
///
/// Separated from the syscalls so every branch is testable: the interesting
/// cases need a mount that either refuses `chmod` or accepts it without
/// applying it, which no portable filesystem produces.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreMode {
    /// Narrower than `0o600` after the repair.
    Narrowed,
    /// Wider, and this filesystem cannot represent the mode at all.
    Unavoidable { mode: u32 },
    /// Wider, on a filesystem that does store modes: still a refusal.
    Exposed { mode: u32 },
}

#[cfg(unix)]
fn classify_store_mode(
    observed: u32,
    narrowed: Option<u32>,
    mount_cannot_hold_modes: bool,
) -> StoreMode {
    if narrowed.is_some_and(|mode| mode & 0o077 == 0) {
        return StoreMode::Narrowed;
    }
    let mode = narrowed.unwrap_or(observed);
    if mount_cannot_hold_modes {
        StoreMode::Unavoidable { mode }
    } else {
        StoreMode::Exposed { mode }
    }
}

/// Whether `path`'s directory refuses the mode a file is created with *and* the
/// one it is then asked for.
///
/// Creates one probe file beside `path`, the same way the store creates its
/// own, at `0o600`, and reads back the mode the mount stored. A filesystem that
/// keeps mode bits reports `0o600` or narrower. A wider mode alone is not
/// enough to blame the mount — a default ACL or a setgid parent widens a new
/// file on a filesystem that stores modes perfectly well — so the probe asks
/// the mount to narrow it and looks again; only a refusal, or a mode that stays
/// wide, means the mode says nothing about who may read the file. Any failure
/// to run the probe answers `false`, so a directory we cannot create files in
/// keeps the strict check instead of widening it.
///
/// Only a store file whose mode is wider than `0o600` reaches this, so a
/// filesystem that stores modes never pays for it; on the mounts that do reach
/// it, the cost is one create, one stat, at most one `chmod` and one unlink.
///
/// The same rule, with the same reasoning, lives in
/// `codewhale_config::private_directory::private_footing`. This crate is below
/// `codewhale-config` in the graph, so it cannot call it; the probe is a dozen
/// lines and the alternative is a dependency cycle. `scripts/ohos-secrets-store-smoke.sh`
/// covers the synthesising branch end to end against a real mount; the tests
/// here cover everything a portable filesystem can produce.
#[cfg(unix)]
fn mount_ignores_unix_modes(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return false;
    };
    let Ok(probe) = tempfile::Builder::new()
        .prefix(".codewhale-mode-probe-")
        .tempfile_in(parent)
    else {
        return false;
    };
    let narrows = |file: &fs::File| {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .is_ok_and(|()| {
                file.metadata()
                    .is_ok_and(|meta| meta.permissions().mode() & 0o077 == 0)
            })
    };
    match probe.as_file().metadata() {
        // `0o077` clear means the mount stored the `0o600` we created with.
        Ok(meta) if meta.permissions().mode() & 0o077 == 0 => false,
        Ok(_) => !narrows(probe.as_file()),
        Err(_) => false,
    }
}

pub(crate) fn with_write_lock<T>(
    path: &Path,
    operation: impl FnOnce(&Path) -> Result<T, SecretsError>,
) -> Result<T, SecretsError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Invalid secret store path."))?;
    let path = parent.canonicalize()?.join(filename);
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let mut lock =
        fd_lock::RwLock::new(open_private(Path::new(&lock_name), true, StoreFile::Lock)?);
    match lock.write() {
        Ok(_guard) => operation(&path),
        // HarmonyOS hmdfs refuses `flock` and `fcntl(F_SETLK)` with EPERM for
        // every file, so the cross-process guard cannot be taken there. Fall
        // back to serialising this process's own writers and say so: refusing
        // outright would leave no way to store a credential on that mount at
        // all. The tradeoff is real and narrow — two Codewhale processes whose
        // snapshots of `secrets.json` overlap can lose one of the two writes,
        // which needs both to rewrite the same store in the same instant.
        Err(error) if advisory_locking_unavailable(&error) => {
            tracing::warn!(
                "{} cannot take a file lock ({error}); serialising secret-store writes \
                 within this process only",
                lock_name.to_string_lossy()
            );
            let _guard = in_process_write_guard();
            operation(&path)
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether a failed advisory lock means the filesystem has none at all, as
/// opposed to a lock that is genuinely held by someone else.
///
/// `EWOULDBLOCK` is deliberately absent: a held lock must still wait or fail
/// rather than fall back. The rest are the mount saying "no locking here".
#[cfg(unix)]
fn advisory_locking_unavailable(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EPERM)
            | Some(libc::EACCES)
            | Some(libc::ENOTSUP)
            | Some(libc::EINVAL)
            | Some(libc::ENOSYS)
    )
}

/// Windows always offers `LockFileEx` through `fd-lock`, so a failed write
/// lock there is genuine contention, never an absent advisory lock.
#[cfg(not(unix))]
fn advisory_locking_unavailable(_error: &std::io::Error) -> bool {
    false
}

/// Serialises secret-store writers inside this process when the filesystem
/// offers no advisory lock. Coarse on purpose: store writes are rare and short.
fn in_process_write_guard() -> std::sync::MutexGuard<'static, ()> {
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// Put a store file on disk with exactly `mode`, the way a user running
    /// `chmod` on an existing `secrets.json` leaves it.
    fn store_at(dir: &Path, mode: u32) -> PathBuf {
        let path = dir.join("secrets.json");
        fs::write(&path, "{\"entries\":{}}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            mode,
            "the test filesystem did not store {mode:o} verbatim"
        );
        path
    }

    fn names_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn owner_only_store_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let path = store_at(tmp.path(), 0o600);
        open_private(&path, false, StoreFile::Content).expect("an 0600 store must open");
        assert_eq!(
            names_in(tmp.path()),
            vec!["secrets.json".to_string()],
            "an 0600 store must be accepted without writing a probe file"
        );
    }

    #[test]
    fn store_created_on_first_write_is_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secrets.json");
        open_private(&path, true, StoreFile::Content).expect("create must succeed");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a freshly created store must be owner-only"
        );
    }

    /// Every wider mode on a filesystem that stores mode bits is a real
    /// exposure and must stay refused. The probe runs here for real, against
    /// this directory, and has to answer "this mount stores modes".
    #[test]
    fn wider_store_is_refused_with_its_observed_mode() {
        for mode in [0o660, 0o640, 0o644, 0o666, 0o606] {
            let tmp = tempfile::tempdir().unwrap();
            let path = store_at(tmp.path(), mode);
            let err = open_private(&path, false, StoreFile::Content).unwrap_err();
            match err {
                SecretsError::InsecurePermissions {
                    path: seen,
                    mode: seen_mode,
                } => {
                    assert_eq!(seen, path);
                    assert_eq!(seen_mode, mode);
                }
                other => panic!("mode {mode:o} must be refused; got: {other}"),
            }
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode,
                "a refused store is left exactly as it was found"
            );
        }
    }

    /// The lock file carries no secret, so a wide mode on it is hygiene rather
    /// than evidence, and the "narrow what can be narrowed" rule applies: the
    /// store opens and the file ends up `0600`. This is the case a user hits
    /// after restoring a backup, and the one whose refusal read as "your secret
    /// backend is broken".
    #[test]
    fn wide_lock_file_is_repaired_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join("secrets.json.lock");
        fs::write(&lock, b"").unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o660)).unwrap();
        open_private(&lock, false, StoreFile::Lock)
            .expect("a wide lock must be repaired, not refused");
        assert_eq!(
            fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600,
            "the repair must actually be applied"
        );
        assert_eq!(
            names_in(tmp.path()),
            vec!["secrets.json.lock".to_string()],
            "a repair must not need the probe"
        );
    }

    /// The classification is the whole decision, and the cases that matter need
    /// a mount that refuses `chmod` or accepts it without applying it, which no
    /// portable
    /// filesystem produces. They are pinned here instead.
    #[test]
    fn store_mode_classification_covers_every_outcome() {
        // The mount stored the narrowing we asked for.
        assert_eq!(
            classify_store_mode(0o660, Some(0o600), false),
            StoreMode::Narrowed
        );
        // hmdfs: the call is accepted and the mount keeps its own mode.
        assert_eq!(
            classify_store_mode(0o660, Some(0o660), true),
            StoreMode::Unavoidable { mode: 0o660 }
        );
        // hmdfs/POSIX-less mount: the call is refused.
        assert_eq!(
            classify_store_mode(0o660, None, true),
            StoreMode::Unavoidable { mode: 0o660 }
        );
        // A mode-storing mount that accepted the call and ignored it is a real
        // exposure, and the observed mode is what the caller is told about.
        assert_eq!(
            classify_store_mode(0o660, Some(0o660), false),
            StoreMode::Exposed { mode: 0o660 }
        );
        assert_eq!(
            classify_store_mode(0o660, None, false),
            StoreMode::Exposed { mode: 0o660 }
        );
        // A successful narrowing to something still wider is not a repair.
        assert_eq!(
            classify_store_mode(0o666, Some(0o660), false),
            StoreMode::Exposed { mode: 0o660 }
        );
    }

    #[test]
    fn probe_reports_a_mode_storing_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            !mount_ignores_unix_modes(&tmp.path().join("secrets.json")),
            "a local filesystem stores the 0600 a file is created with"
        );
    }

    #[test]
    fn probe_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let before = names_in(tmp.path());
        assert!(!mount_ignores_unix_modes(&tmp.path().join("secrets.json")));
        assert_eq!(names_in(tmp.path()), before);
    }

    #[test]
    fn probe_answers_false_without_a_parent_directory() {
        assert!(!mount_ignores_unix_modes(Path::new("secrets.json")));
    }

    #[test]
    fn a_held_lock_is_not_mistaken_for_a_missing_one() {
        // EWOULDBLOCK is what a lock held by another process reports; treating
        // it as "no locking on this mount" would silently drop serialisation.
        assert!(!advisory_locking_unavailable(
            &std::io::Error::from_raw_os_error(libc::EWOULDBLOCK)
        ));
        assert!(!advisory_locking_unavailable(
            &std::io::Error::from_raw_os_error(libc::EINTR)
        ));
        for errno in [libc::EPERM, libc::EACCES, libc::ENOTSUP, libc::EINVAL] {
            assert!(
                advisory_locking_unavailable(&std::io::Error::from_raw_os_error(errno)),
                "errno {errno} means the filesystem has no advisory locks"
            );
        }
    }
}

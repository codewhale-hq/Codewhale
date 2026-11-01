use std::io;
use std::path::{Path, PathBuf};

pub const STAMP_NAME: &str = ".stamp";

/// A historical bundle identity used by snapshot garbage collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedBundle {
    pub name: &'static str,
    pub digest: String,
}

impl EmbeddedBundle {
    /// Directory name of this build's snapshot under the snapshots root.
    pub fn snapshot_dir_name(&self) -> String {
        format!("{}-{}", self.name, self.digest)
    }
}

/// The bundles this build would materialize. Garbage collection compares
/// on-disk snapshots against this list to decide which belong to other builds.
pub fn embedded_bundles() -> Vec<EmbeddedBundle> {
    Vec::new()
}

/// `<home>/builtin-plugins/snapshots`, the only place snapshots are published.
pub fn snapshots_dir(home: &Path) -> PathBuf {
    home.join("builtin-plugins").join("snapshots")
}

/// Split `<bundle>-<64 hex>` into the embedded bundle name and its digest.
/// Anything else in the snapshots root is not ours to classify.
pub fn parse_snapshot_dir_name(
    dir_name: &str,
    bundles: &[EmbeddedBundle],
) -> Option<(&'static str, String)> {
    bundles.iter().find_map(|bundle| {
        let digest = dir_name.strip_prefix(bundle.name)?.strip_prefix('-')?;
        (digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| (bundle.name, digest.to_string()))
    })
}

/// Atomic no-replace directory publication. A check followed by ordinary Unix
/// rename is insufficient: rename is allowed to replace an existing empty dir.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
pub(super) fn publish_snapshot(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: the nul-terminated paths remain alive for the syscall. Exclusive
    // rename never follows/replaces the destination entry, even if it is a link.
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let result = unsafe {
        // Static musl may lack the libc wrapper; use the same kernel operation.
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(super) fn publish_snapshot(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::PCWSTR;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut attempt = 0;
    loop {
        // SAFETY: both paths are nul-terminated and live through the call. Omitting
        // MOVEFILE_REPLACE_EXISTING preserves every existing destination entry.
        let error = match unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        } {
            Ok(()) => return Ok(()),
            Err(_) => io::Error::last_os_error(),
        };
        let Some(backoff) = codewhale_util::utils::windows_publish_retry_delay(&error, attempt)
        else {
            return Err(error);
        };
        std::thread::sleep(backoff);
        attempt += 1;
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    windows
)))]
pub(super) fn publish_snapshot(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic built-in snapshot publication is unsupported on this platform",
    ))
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod snapshot_tests;

//! Bounded package content digest shared by audit and mutation.
//!
//! Kept separate so `install` can write metadata v2 without depending on the
//! audit module (which itself depends on install marker constants).

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::install::{
    DEFAULT_MAX_SIZE_BYTES, INSTALLED_FROM_MARKER, TRUSTED_MARKER, is_reserved_root_metadata,
};

pub const PACKAGE_DIGEST_MAX_BYTES: u64 = DEFAULT_MAX_SIZE_BYTES;
pub const PACKAGE_DIGEST_MAX_FILES: usize = 256;
pub const PACKAGE_DIGEST_MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageDigestError {
    Unreadable,
    SymlinkPresent,
    EscapedRoot,
    Cycle,
    Oversized,
    TooManyFiles,
    TooDeep,
}

impl std::fmt::Display for PackageDigestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreadable => "unreadable package file",
            Self::SymlinkPresent => "symlink present in package",
            Self::EscapedRoot => "path escaped package root",
            Self::Cycle => "symlink/directory cycle",
            Self::Oversized => "package exceeded size limit",
            Self::TooManyFiles => "package exceeded file limit",
            Self::TooDeep => "package exceeded depth limit",
        })
    }
}

impl std::error::Error for PackageDigestError {}

/// SHA-256 hex of the normalized package manifest (relative path + len + bytes).
pub fn compute_package_digest(package_dir: &Path) -> Result<String, PackageDigestError> {
    let canonical_package =
        fs::canonicalize(package_dir).map_err(|_| PackageDigestError::Unreadable)?;

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut visited = HashSet::new();

    walk(
        package_dir,
        &canonical_package,
        0,
        &mut visited,
        &mut files,
        &mut total_bytes,
    )?;

    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, bytes) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn walk(
    dir: &Path,
    package_root: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    files: &mut Vec<(String, Vec<u8>)>,
    total_bytes: &mut u64,
) -> Result<(), PackageDigestError> {
    if depth > PACKAGE_DIGEST_MAX_DEPTH {
        return Err(PackageDigestError::TooDeep);
    }
    let meta = fs::symlink_metadata(dir).map_err(|_| PackageDigestError::Unreadable)?;
    if meta.file_type().is_symlink() {
        return Err(PackageDigestError::SymlinkPresent);
    }
    let canonical = fs::canonicalize(dir).map_err(|_| PackageDigestError::Unreadable)?;
    if !canonical.starts_with(package_root) {
        return Err(PackageDigestError::EscapedRoot);
    }
    if !visited.insert(canonical.clone()) {
        return Err(PackageDigestError::Cycle);
    }

    let entries = fs::read_dir(&canonical).map_err(|_| PackageDigestError::Unreadable)?;
    for entry in entries {
        let entry = entry.map_err(|_| PackageDigestError::Unreadable)?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(PackageDigestError::Unreadable)?;

        let meta = fs::symlink_metadata(&path).map_err(|_| PackageDigestError::Unreadable)?;
        if meta.file_type().is_symlink() {
            return Err(PackageDigestError::SymlinkPresent);
        }

        // Only local bookkeeping at the package root is outside the receipt.
        // Hidden files and backup/temp names can contain executable payloads.
        if depth == 0 && is_reserved_root_metadata(Path::new(name)) {
            if meta.is_file()
                && [
                    INSTALLED_FROM_MARKER,
                    TRUSTED_MARKER,
                    ".system-installed-version",
                ]
                .contains(&name)
            {
                continue;
            }
            // A variant may be an ordinary payload on a case-sensitive disk
            // or alias the receipt itself on another disk. Refuse ambiguity:
            // the owner must rename it before the package can be trusted.
            return Err(PackageDigestError::Unreadable);
        }

        if meta.is_dir() {
            walk(&path, package_root, depth + 1, visited, files, total_bytes)?;
            continue;
        }
        if !meta.is_file() {
            return Err(PackageDigestError::Unreadable);
        }
        if files.len() >= PACKAGE_DIGEST_MAX_FILES {
            return Err(PackageDigestError::TooManyFiles);
        }
        let remaining = PACKAGE_DIGEST_MAX_BYTES.saturating_sub(*total_bytes);
        if meta.len() > remaining {
            return Err(PackageDigestError::Oversized);
        }
        let mut file = fs::File::open(&path).map_err(|_| PackageDigestError::Unreadable)?;
        let bytes = read_package_file(&mut file, remaining)?;
        *total_bytes += bytes.len() as u64;
        let rel = path
            .strip_prefix(package_root)
            .map_err(|_| PackageDigestError::EscapedRoot)?
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .ok_or(PackageDigestError::Unreadable)
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        files.push((rel, bytes));
    }
    Ok(())
}

fn read_package_file(file: &mut fs::File, remaining: u64) -> Result<Vec<u8>, PackageDigestError> {
    // Metadata is only an early check: a package file may grow before reading.
    let mut bytes = Vec::new();
    file.take(remaining + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PackageDigestError::Unreadable)?;
    if bytes.len() as u64 > remaining {
        return Err(PackageDigestError::Oversized);
    }
    Ok(bytes)
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_roots_keep_nested_paths_and_match_canonical_roots() {
        let cwd = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir_in(&cwd).unwrap();
        let relative = tmp.path().strip_prefix(&cwd).unwrap();
        assert!(!relative.is_absolute());
        fs::create_dir(relative.join("before")).unwrap();
        fs::create_dir(relative.join("after")).unwrap();
        fs::write(relative.join("before/payload.txt"), b"same bytes").unwrap();
        let canonical = fs::canonicalize(relative).unwrap();

        let before = compute_package_digest(relative).unwrap();
        assert_eq!(before, compute_package_digest(&canonical).unwrap());
        fs::rename(
            relative.join("before/payload.txt"),
            relative.join("after/payload.txt"),
        )
        .unwrap();
        let after = compute_package_digest(relative).unwrap();
        assert_ne!(before, after);
        assert_eq!(after, compute_package_digest(&canonical).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn aliased_ancestors_keep_nested_paths_but_symlink_package_roots_are_refused() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("real");
        let package = parent.join("package");
        fs::create_dir_all(package.join("before")).unwrap();
        fs::create_dir(package.join("after")).unwrap();
        fs::write(package.join("before/payload.txt"), b"same bytes").unwrap();
        let alias = tmp.path().join("alias");
        symlink(&parent, &alias).unwrap();
        let aliased_package = alias.join("package");
        let canonical = fs::canonicalize(&package).unwrap();

        let before = compute_package_digest(&aliased_package).unwrap();
        assert_eq!(before, compute_package_digest(&canonical).unwrap());
        fs::rename(
            package.join("before/payload.txt"),
            package.join("after/payload.txt"),
        )
        .unwrap();
        let after = compute_package_digest(&aliased_package).unwrap();
        assert_ne!(before, after);
        assert_eq!(after, compute_package_digest(&canonical).unwrap());

        let root_link = tmp.path().join("package-link");
        symlink(&package, &root_link).unwrap();
        assert_eq!(
            compute_package_digest(&root_link),
            Err(PackageDigestError::SymlinkPresent)
        );
    }

    #[cfg(unix)]
    #[test]
    fn literal_backslash_filename_differs_from_nested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let package = fs::canonicalize(tmp.path()).unwrap();
        fs::create_dir(package.join("a")).unwrap();
        let literal = package.join(r"a\b");
        fs::write(&literal, b"same bytes").unwrap();
        let before = compute_package_digest(&package).unwrap();

        fs::rename(literal, package.join("a/b")).unwrap();
        assert_ne!(before, compute_package_digest(&package).unwrap());
    }

    #[test]
    fn case_variant_metadata_fails_closed_instead_of_omitting_payload_or_hashing_itself() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".TRUSTED"), b"payload or aliased receipt").unwrap();
        assert_eq!(
            compute_package_digest(tmp.path()),
            Err(PackageDigestError::Unreadable)
        );
    }

    #[test]
    fn reserved_metadata_directories_cannot_hide_payload() {
        let tmp = tempfile::tempdir().unwrap();
        for marker in [
            INSTALLED_FROM_MARKER,
            TRUSTED_MARKER,
            ".system-installed-version",
        ] {
            let path = tmp.path().join(marker);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("payload"), b"must not be omitted").unwrap();
            assert_eq!(
                compute_package_digest(tmp.path()),
                Err(PackageDigestError::Unreadable)
            );
            fs::remove_dir_all(path).unwrap();
        }
    }

    // Linux permits non-UTF-8 filenames; macOS APFS rejects the fixture itself.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_payload_names_fail_closed() {
        use std::os::unix::ffi::OsStringExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let name = std::ffi::OsString::from_vec(b"payload-\xff".to_vec());
        fs::write(tmp.path().join(name), b"payload").unwrap();
        assert_eq!(
            compute_package_digest(tmp.path()),
            Err(PackageDigestError::Unreadable)
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_package_entries_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(tmp.path().join("socket")).unwrap();
        assert_eq!(
            compute_package_digest(tmp.path()),
            Err(PackageDigestError::Unreadable)
        );
    }

    #[test]
    fn bounded_read_rejects_growth_after_metadata_without_reading_the_whole_file() {
        use std::io::Seek as _;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("growing");
        fs::write(&path, b"ok").unwrap();
        let allowed = fs::metadata(&path).unwrap().len();
        fs::write(&path, b"grew beyond the remaining budget").unwrap();
        let mut file = fs::File::open(path).unwrap();
        assert_eq!(
            read_package_file(&mut file, allowed),
            Err(PackageDigestError::Oversized)
        );
        assert_eq!(file.stream_position().unwrap(), allowed + 1);
    }

    #[test]
    fn bounded_read_accepts_exact_remaining_bytes() {
        // A named file in a temp dir, like the neighbouring test: Windows CI
        // denies the anonymous `tempfile()` under the hermetic test home.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exact");
        fs::write(&path, b"exact").unwrap();
        let mut file = fs::File::open(path).unwrap();
        assert_eq!(read_package_file(&mut file, 5).unwrap(), b"exact");
    }
}

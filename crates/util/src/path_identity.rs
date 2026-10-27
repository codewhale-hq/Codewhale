use std::path::Path;

use sha2::Digest;

/// Return true for every pathname indirection the plugin boundary rejects.
///
/// `FileType::is_symlink` is insufficient on Windows: junctions, mount points,
/// and other name-surrogate objects carry `FILE_ATTRIBUTE_REPARSE_POINT`
/// without necessarily using the symbolic-link reparse tag. Keep this one
/// predicate shared by discovery, manifest validation, staging, and ACL
/// hardening so no surface silently follows a broader class than another.
#[cfg(windows)]
pub fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x0000_0400 != 0
}

#[cfg(not(windows))]
pub fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsFileIdentity {
    pub volume: u32,
    pub index: u64,
    pub links: u32,
    pub attributes: u32,
}

/// Query stable handle-relative Windows identity without relying on Rust's
/// still-unstable `windows_by_handle` metadata extensions.
#[cfg(windows)]
pub fn windows_file_identity(file: &std::fs::File) -> std::io::Result<WindowsFileIdentity> {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` retains a valid handle and `information` points to live,
    // writable storage for the duration of the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(std::io::Error::other)?;
    Ok(WindowsFileIdentity {
        volume: information.dwVolumeSerialNumber,
        index: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        links: information.nNumberOfLinks,
        attributes: information.dwFileAttributes,
    })
}

/// Add a lossless, platform-scoped OS path to a digest.
///
/// Plugin identity must never pass through Unicode replacement. Unix paths
/// are byte strings and Windows paths are UTF-16 strings; two distinct native
/// paths can therefore have the same `to_string_lossy()` representation. The
/// framing below also prevents a future platform/domain change from silently
/// reusing an existing trust receipt.
pub fn hash_os_path(hasher: &mut impl Digest, domain: &'static [u8], path: &Path) {
    hasher.update(b"codewhale-os-path-v1\0");
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let bytes = path.as_os_str().as_bytes();
        hasher.update(b"unix-bytes\0");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        hasher.update(b"windows-utf16le\0");
        hasher.update((units.len() as u64).to_le_bytes());
        for unit in units {
            hasher.update(unit.to_le_bytes());
        }
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        // `as_encoded_bytes` is lossless for the platform's `OsStr`
        // representation within one Rust implementation. Keep this fallback
        // separately tagged so receipts can never cross into Unix/Windows.
        let bytes = path.as_os_str().as_encoded_bytes();
        hasher.update(b"rust-osstr-encoded\0");
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Sha256;

    fn digest(path: &Path) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_os_path(&mut hasher, b"test-domain", path);
        hasher.finalize().to_vec()
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_reparse_points_even_when_not_symbolic_links() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let junction = directory.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .expect("invoke Windows junction creation");
        assert!(
            output.status.success(),
            "failed to create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let metadata = std::fs::symlink_metadata(&junction).unwrap();
        assert!(metadata_is_link_or_reparse(&metadata));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_unicode_paths_do_not_collapse_to_replacement_text() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let first = OsString::from_vec(vec![b'a', 0xff]);
        let second = OsString::from_vec(vec![b'a', 0xfe]);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(digest(Path::new(&first)), digest(Path::new(&second)));
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_utf16_paths_do_not_collapse_to_replacement_text() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        let first = OsString::from_wide(&[b'a' as u16, 0xd800]);
        let second = OsString::from_wide(&[b'a' as u16, 0xd801]);
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(digest(Path::new(&first)), digest(Path::new(&second)));
    }
}

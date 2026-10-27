use std::fs;
use std::path::Path;

#[cfg(unix)]
pub fn open_bundle_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
pub fn open_bundle_file(path: &Path) -> std::io::Result<fs::File> {
    open_bundle_file_with_access(
        path,
        windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ,
    )
}

/// Same protected reader, retaining the right to move this exact opened object.
#[cfg(windows)]
pub fn open_bundle_file_for_retirement(path: &Path) -> std::io::Result<fs::File> {
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_READ};
    open_bundle_file_with_access(path, FILE_GENERIC_READ | DELETE)
}

#[cfg(windows)]
fn open_bundle_file_with_access(path: &Path, access: u32) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let file = fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(0x0000_0001) // deny concurrent writes and replacement
        .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
        .open(path)?;
    let metadata = file.metadata()?;
    let identity = crate::path_identity::windows_file_identity(&file)?;
    if !metadata.is_file()
        || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || identity.links != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "plugin file is a reparse point, hard link, or non-regular file",
        ));
    }
    Ok(file)
}

#[cfg(all(not(unix), not(windows)))]
pub fn open_bundle_file(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(windows)]
pub fn open_bundle_directory(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001)
        .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)?;
    let metadata = file.metadata()?;
    let identity = crate::path_identity::windows_file_identity(&file)?;
    if !metadata.is_dir() || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "plugin directory is a reparse point or non-directory",
        ));
    }
    Ok(file)
}

/// Reopen a reviewed path only to compare its current handle identity with a
/// retained authority handle. Desired access is zero and sharing is permissive
/// because the retained handle remains responsible for denying writes and
/// replacement throughout the comparison.
#[cfg(windows)]
pub fn open_bundle_identity_probe(
    path: &Path,
    expect_directory: bool,
) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x0000_0007;
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if expect_directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ_WRITE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    let metadata = file.metadata()?;
    let identity = crate::path_identity::windows_file_identity(&file)?;
    let expected_kind = if expect_directory {
        metadata.is_dir()
    } else {
        metadata.is_file() && identity.links == 1
    };
    if !expected_kind || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "plugin identity probe found a reparse point, hard link, or unexpected object",
        ));
    }
    Ok(file)
}

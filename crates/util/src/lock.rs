use std::fs::File;
use std::io;

/// Upper bound on the bytes read back from a lock file's holder record.
pub const OWNER_LOCK_HOLDER_MAX_BYTES: u64 = 128;

/// The Windows byte range `LockFile` takes: `(offset low, offset high, length
/// low, length high)`. A Windows byte-range lock is mandatory, so locking the
/// whole file would stop a contender reading the holder record at the front
/// (#6573). The range starts after the record and still overlaps the
/// whole-file range older builds take, so mixed versions exclude each other.
#[cfg(windows)]
const OWNER_LOCK_RANGE: (u32, u32, u32, u32) =
    (OWNER_LOCK_HOLDER_MAX_BYTES as u32, 0, u32::MAX, 0x7FFF_FFFF);

pub fn is_contention(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub fn try_lock_exclusive(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `file` owns a valid descriptor and is retained by the
        // caller while the lock is held.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::LockFile;
        let (offset_low, offset_high, length_low, length_high) = OWNER_LOCK_RANGE;
        // SAFETY: `file` owns a valid handle retained by the caller.
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
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(())
    }
}

pub fn unlock(file: &File) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `file` owns a valid descriptor for the duration of the call.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::UnlockFile;
        let (offset_low, offset_high, length_low, length_high) = OWNER_LOCK_RANGE;
        // SAFETY: `file` owns a valid handle for the duration of the call.
        unsafe {
            UnlockFile(
                file.as_raw_handle() as _,
                offset_low,
                offset_high,
                length_low,
                length_high,
            );
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
    }
}

/// Try to take an exclusive OS lock on `file` without blocking: `Ok(true)`
/// when acquired (released when the file closes), `Ok(false)` when another
/// open file description holds it.
pub fn try_lock_file_exclusive(file: &File) -> std::io::Result<bool> {
    match try_lock_exclusive(file) {
        Ok(()) => Ok(true),
        Err(error) if is_contention(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

use std::io::Write as _;
use std::path::Path;

/// Open or create a file for appending at `path`, optionally syncing after
/// every write. Use this for append-only logs like `audit.log`.
///
/// The returned `BufWriter<fs::File>` wraps the append handle. Call
/// `.flush()` followed by `.get_ref().sync_all()` after each batch.
pub fn open_append(path: &Path) -> std::io::Result<std::io::BufWriter<std::fs::File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = private_log_options().append(true).open(path)?;
    restrict_to_owner(&file)?;
    Ok(std::io::BufWriter::new(file))
}

/// Open options for an owner-only log or lock file: created 0600 and never
/// opened through a link at the final component (Unix). Callers add the
/// access mode they need.
pub fn private_log_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

/// Tighten a log created by an earlier version at the default mode. No-op
/// outside Unix.
///
/// Best effort on purpose. The file was created `0600`, so this only narrows a
/// log left wider by an earlier version or a different umask — and a mount that
/// assigns modes itself (HarmonyOS hmdfs answers `EPERM` to every `chmod`) will
/// refuse it for every object it holds. Failing here would turn a write that
/// already succeeded into "permission denied"; a log left wide is reported
/// instead, so an operator can still see it.
pub fn restrict_to_owner(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .is_err()
            && file
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o077 != 0)
        {
            tracing::warn!(
                "a private log could not be narrowed to 0600 and is still readable by group or \
                 other; this filesystem does not apply Unix mode bits"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// Flush a `BufWriter` wrapping a `File`, then `fsync` the underlying file.
pub fn flush_and_sync(writer: &mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()> {
    writer.flush()?;
    writer.get_ref().sync_all()
}

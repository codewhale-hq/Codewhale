//! TUI runtime logging. Initializes a `tracing-subscriber` that writes to a
//! per-process file under `~/.codewhale/logs/tui-YYYY-MM-DD-PID.log`, and (on
//! Unix and Windows) redirects the process's `stderr` handle/fd to that same
//! file for the lifetime of the alt-screen TUI.
//!
//! Why this exists:
//!
//! The TUI runs inside an alt-screen buffer drawn by `ratatui` using an
//! incremental diff renderer. The renderer assumes nothing else is writing
//! to the terminal — its internal "current cells" model is the only source
//! of truth for what's on screen. If anything emits raw bytes to stdout or
//! stderr while the alt-screen is active (an `eprintln!` from a sub-agent,
//! a `tracing` warning that defaulted to `stderr`, a panic message, a
//! third-party crate's verbose output, …) those bytes land in the alt-screen
//! buffer at the current cursor position, scroll the buffer up, and leave
//! the renderer's model out of sync with reality. The visible symptom is
//! "scroll demon": the TUI content drifts down, leaving a band of blank
//! rows above the header. This was the regression in issue #1085 (fixed in
//! v0.8.18 by adding a viewport-reset path) and re-surfaced in v0.8.27
//! when the flicker fix dropped the `\x1b[2J\x1b[3J` deep-clear that had
//! been masking the underlying leak.
//!
//! Defence-in-depth:
//!   1. A `tracing-subscriber` writes formatted logs to
//!      `~/.codewhale/logs/tui-YYYY-MM-DD-PID.log` so `tracing::warn!` /
//!      `tracing::error!` calls go somewhere observable instead of
//!      disappearing into the void (the TUI previously had no global
//!      subscriber, so contributors reached for `eprintln!`).
//!   2. On Unix and Windows the process's stderr handle/fd is redirected to
//!      the same log file for the lifetime of `TuiLogGuard`. Any raw stderr
//!      write — ours, a dependency's, a panic message — lands in the log
//!      file instead of the alt-screen. The guard restores the original
//!      stderr handle/fd on drop so post-TUI shutdown messages still reach
//!      the user's terminal.
//!   3. Crate-level `#![deny(clippy::print_stderr, clippy::print_stdout)]`
//!      on the TUI runtime modules forbids new `eprintln!` / `println!`
//!      calls at compile time. CLI-output paths (`main.rs` eval, init,
//!      `runtime_api::print_*`, `logging::info`/`warn`) keep their existing
//!      prints via `#[allow(clippy::print_stderr)]` because they run before
//!      the alt-screen is entered.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const DEFAULT_LOG_RETENTION_DAYS: u64 = 7;
const LOG_RETENTION_ENV: &str = "DEEPSEEK_LOG_RETENTION_DAYS";
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Owns the active tracing subscriber and (on Unix/Windows) a saved copy of
/// the original `stderr` handle/fd so it can be restored on drop. Dropped when
/// the TUI exits the alt-screen.
pub struct TuiLogGuard {
    #[cfg(unix)]
    saved_stderr_fd: Option<libc::c_int>,
    #[cfg(windows)]
    saved_stderr_handle: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(windows)]
    redirected_stderr_handle: Option<windows::Win32::Foundation::HANDLE>,
    _file: File,
}

#[cfg(unix)]
impl Drop for TuiLogGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved_stderr_fd.take() {
            // SAFETY: `saved` came from `libc::dup` of the original stderr
            // fd in `init`; calling `dup2` to restore it is the standard
            // pairing. If `dup2` fails we just leak the saved fd — the
            // process is exiting anyway.
            unsafe {
                let _ = libc::dup2(saved, libc::STDERR_FILENO);
                let _ = libc::close(saved);
            }
        }
    }
}

#[cfg(windows)]
impl Drop for TuiLogGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.saved_stderr_handle.take() {
            // SAFETY: `handle` is owned here via take; Drop runs once.
            unsafe {
                let _ = windows::Win32::System::Console::SetStdHandle(
                    windows::Win32::System::Console::STD_ERROR_HANDLE,
                    handle,
                );
            }
        }
        // Close the duplicated handle that was serving as the redirected
        // stderr target. This is safe because `SetStdHandle` above already
        // restored the original handle, so nothing references this one.
        if let Some(dup) = self.redirected_stderr_handle.take() {
            // SAFETY: `dup` is owned here via take; nothing references it.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(dup);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
impl Drop for TuiLogGuard {
    fn drop(&mut self) {}
}

/// Initialize the TUI logging subsystem. Idempotent across re-entry by way
/// of `set_default` — if a global subscriber is already set we still install
/// the stderr redirect.
///
/// Returns a guard that must outlive the alt-screen session. Drop it after
/// `LeaveAlternateScreen` so any shutdown messages reach the user.
pub fn init() -> Result<TuiLogGuard> {
    let log_dir = log_directory().context("could not resolve TUI log directory")?;
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create {}", log_dir.display()))?;
    let _ = prune_old_logs(&log_dir, log_retention_days());

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let log_path = log_dir.join(log_file_name(&date, std::process::id()));

    let file = open_log_file(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;

    // The tracing-subscriber consumes a clone of the file handle for its
    // writer. We keep our own handle for the dup2 redirect below — we need
    // the same on-disk file but a separate fd so the subscriber's writes
    // and the raw-stderr writes don't fight over the same kernel offset.
    let subscriber_file = file
        .try_clone()
        .context("failed to clone log file handle for subscriber")?;

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let log_path_clone = log_path.clone();
    let subscriber = tracing_subscriber::registry().with(env_filter).with(
        fmt::layer()
            .with_writer(move || -> Box<dyn std::io::Write + Send> {
                // Clone the file handle for each write. If clone fails (fd exhaustion),
                // fall back to reopening the same path, or ultimately stderr.
                match subscriber_file.try_clone() {
                    Ok(f) => Box::new(f),
                    Err(e) => {
                        tracing::warn!("Failed to clone log file handle: {e}, reopening");
                        match open_log_file(&log_path_clone) {
                            Ok(f) => Box::new(f),
                            Err(_) => Box::new(std::io::stderr()),
                        }
                    }
                }
            })
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(false),
    );

    // Best-effort: if a subscriber is already set (e.g., re-entry, or a
    // host process installed one), we skip ours rather than panic. The
    // stderr redirect below still happens.
    let _ = tracing::subscriber::set_global_default(subscriber);

    #[cfg(unix)]
    let saved_stderr_fd = redirect_stderr_to(&file).ok();
    #[cfg(windows)]
    let (saved_stderr_handle, redirected_stderr_handle) = match redirect_stderr_to(&file) {
        Ok((saved, dup)) => (Some(saved), Some(dup)),
        Err(e) => {
            tracing::warn!("Failed to redirect stderr to log file: {e}");
            (None, None)
        }
    };

    Ok(TuiLogGuard {
        #[cfg(unix)]
        saved_stderr_fd,
        #[cfg(windows)]
        saved_stderr_handle,
        #[cfg(windows)]
        redirected_stderr_handle,
        _file: file,
    })
}

/// Open (creating if needed) the per-process log for append. Logs can hold
/// prompts and paths, so on Unix the file is owner-only: created 0600, an
/// existing file is tightened to 0600, and a symlink at the log path is
/// refused (`O_NOFOLLOW`) instead of followed to wherever it points.
fn open_log_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(path)?;
        // Through the handle that was opened `O_NOFOLLOW`: re-resolving `path`
        // here could narrow a substitute, and this log is the object the caller
        // is about to write prompts and paths into. `tighten_mode_of` keeps the
        // refusal for a filesystem that stores modes and excuses only one that
        // cannot apply them, so a log still readable by group or other is
        // reported rather than ignored.
        codewhale_config::private_directory::tighten_mode_of(&file, path, 0o600)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        options.open(path)
    }
}

pub fn log_directory() -> Option<PathBuf> {
    // $CODEWHALE_HOME is a hard override of the base data directory
    // (docs/CONFIGURATION.md): when SET, logs live under it and we do NOT fall
    // back to the legacy ~/.deepseek path — silent fallback would defeat the
    // isolation the override promises (CI, containers, test harnesses). We
    // check the env var directly rather than codewhale_home()'s Ok/Err because
    // that helper succeeds (returns $HOME/.codewhale) even when the override is
    // unset, which would short-circuit the legacy fallback below.
    if let Some(home) = codewhale_paths::codewhale_home_override().ok().flatten() {
        return Some(home.join("logs"));
    }
    let resolve = |base: PathBuf| -> Option<PathBuf> {
        let primary = base.join(".codewhale").join("logs");
        if primary.exists() {
            return Some(primary);
        }
        let legacy = base.join(".deepseek").join("logs");
        if legacy.exists() {
            return Some(legacy);
        }
        Some(primary)
    };
    codewhale_paths::user_home().and_then(resolve)
}

fn log_file_name(date: &str, pid: u32) -> String {
    format!("tui-{date}-{pid}.log")
}

fn log_retention_days() -> u64 {
    std::env::var(LOG_RETENTION_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|days| *days > 0)
        .unwrap_or(DEFAULT_LOG_RETENTION_DAYS)
}

fn prune_old_logs(log_dir: &Path, retention_days: u64) -> std::io::Result<usize> {
    let retention = Duration::from_secs(retention_days.saturating_mul(SECONDS_PER_DAY));
    let cutoff = SystemTime::now()
        .checked_sub(retention)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;

    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        if !is_tui_log_file_name(&entry.file_name()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        if modified < cutoff && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

fn is_tui_log_file_name(file_name: &std::ffi::OsStr) -> bool {
    file_name
        .to_str()
        .is_some_and(|name| name.starts_with("tui-") && name.ends_with(".log"))
}

#[cfg(unix)]
fn redirect_stderr_to(file: &File) -> Result<libc::c_int> {
    use std::os::fd::AsRawFd;
    let target = file.as_raw_fd();
    // SAFETY: `libc::dup` and `libc::dup2` are the documented fd-management
    // primitives. We save the current stderr fd before reassigning so the
    // guard can restore it on drop.
    unsafe {
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 {
            return Err(
                anyhow::Error::from(std::io::Error::last_os_error()).context("dup(STDERR_FILENO)")
            );
        }
        if libc::dup2(target, libc::STDERR_FILENO) < 0 {
            let err = std::io::Error::last_os_error();
            let _ = libc::close(saved);
            return Err(anyhow::Error::from(err).context("dup2(log_file, STDERR_FILENO)"));
        }
        Ok(saved)
    }
}

#[cfg(windows)]
fn redirect_stderr_to(
    file: &File,
) -> Result<(
    windows::Win32::Foundation::HANDLE,
    windows::Win32::Foundation::HANDLE,
)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
    use windows::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetStdHandle is always available; returns INVALID_HANDLE_VALUE
    // on failure or null-like handles for console-less processes.
    let saved =
        unsafe { GetStdHandle(STD_ERROR_HANDLE) }.context("GetStdHandle(STD_ERROR_HANDLE)")?;
    if saved.is_invalid() {
        return Err(anyhow::anyhow!("GetStdHandle(STD_ERROR_HANDLE) failed"));
    }

    // Duplicate the file handle so the redirected stderr owns an
    // independent HANDLE — mirroring the Unix path's `libc::dup`.
    // Without this, `_file` and stderr would alias the same HANDLE;
    // a rogue `CloseHandle` on stderr would silently invalidate `_file`.
    let raw = HANDLE(file.as_raw_handle());
    // SAFETY: pseudo-handle; no preconditions.
    let process = unsafe { GetCurrentProcess() };
    let mut dup = HANDLE::default();
    // SAFETY: `file` and `dup` are live; pseudo-handle needs no close.
    unsafe {
        DuplicateHandle(
            process,
            raw,
            process,
            &mut dup,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .context("DuplicateHandle for stderr redirect")?;
    }

    // SAFETY: SetStdHandle redirects stderr to the duplicated handle.
    // We save the original handle so the guard can restore it on drop.
    unsafe {
        if let Err(e) = SetStdHandle(STD_ERROR_HANDLE, dup) {
            let _ = CloseHandle(dup);
            return Err(anyhow::anyhow!(
                "SetStdHandle(STD_ERROR_HANDLE) failed: {e}"
            ));
        }
    }
    Ok((saved, dup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;

    #[cfg(unix)]
    #[test]
    fn log_file_is_owner_only_and_refuses_symlinks() {
        use std::io::Write as _;
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let tmp = tempfile::TempDir::new().expect("temporary dir");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };

        let fresh = tmp.path().join("fresh.log");
        open_log_file(&fresh)
            .expect("create")
            .write_all(b"one\n")
            .expect("write");
        assert_eq!(mode(&fresh), 0o600);

        // An existing world-readable log is tightened, and stays appended to.
        let old = tmp.path().join("old.log");
        std::fs::write(&old, "before\n").expect("seed");
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        open_log_file(&old)
            .expect("reopen")
            .write_all(b"after\n")
            .expect("write");
        assert_eq!(mode(&old), 0o600);
        assert_eq!(
            std::fs::read_to_string(&old).expect("read"),
            "before\nafter\n"
        );

        // A symlink planted at the log path is not followed.
        let target = tmp.path().join("target.txt");
        std::fs::write(&target, "keep").expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let link = tmp.path().join("link.log");
        symlink(&target, &link).expect("symlink");
        assert!(open_log_file(&link).is_err());
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "keep");
        assert_eq!(mode(&target), 0o644);
    }

    fn set_modified(path: &Path, modified: SystemTime) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[test]
    fn log_directory_prefers_home() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("USERPROFILE", "");
        }

        let resolved = log_directory().expect("log_directory should resolve");
        assert_eq!(resolved, tmp.path().join(".codewhale").join("logs"));

        // SAFETY: cleanup under the same lock.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    fn log_directory_uses_existing_legacy_deepseek_logs() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let legacy = tmp.path().join(".deepseek").join("logs");
        fs::create_dir_all(&legacy).unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("USERPROFILE", "");
        }

        let resolved = log_directory().expect("log_directory should resolve");
        assert_eq!(resolved, legacy);

        // SAFETY: cleanup under the same lock.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    fn log_file_name_includes_pid() {
        assert_eq!(
            log_file_name("2026-05-18", 12345),
            "tui-2026-05-18-12345.log"
        );
    }

    #[test]
    fn log_retention_days_uses_positive_env_override() {
        let _lock = crate::test_support::lock_test_env();
        let previous = std::env::var_os(LOG_RETENTION_ENV);

        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var(LOG_RETENTION_ENV, "14");
        }
        assert_eq!(log_retention_days(), 14);

        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var(LOG_RETENTION_ENV, "0");
        }
        assert_eq!(log_retention_days(), DEFAULT_LOG_RETENTION_DAYS);

        // SAFETY: cleanup under the same lock.
        unsafe {
            match previous {
                Some(value) => std::env::set_var(LOG_RETENTION_ENV, value),
                None => std::env::remove_var(LOG_RETENTION_ENV),
            }
        }
    }

    #[test]
    fn prune_old_logs_drops_only_stale_tui_logs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fresh = tmp.path().join("tui-2026-05-18-1.log");
        let stale = tmp.path().join("tui-2026-05-01-2.log");
        let legacy_stale = tmp.path().join("tui-2026-05-01.log");
        let unrelated = tmp.path().join("agent-2026-05-01.log");

        fs::write(&fresh, "fresh").unwrap();
        fs::write(&stale, "stale").unwrap();
        fs::write(&legacy_stale, "legacy").unwrap();
        fs::write(&unrelated, "other").unwrap();

        let now = SystemTime::now();
        let old = now - Duration::from_secs(10 * SECONDS_PER_DAY);
        set_modified(&stale, old);
        set_modified(&legacy_stale, old);
        set_modified(&unrelated, old);

        let removed = prune_old_logs(tmp.path(), 7).unwrap();

        assert_eq!(removed, 2);
        assert!(fresh.exists());
        assert!(!stale.exists());
        assert!(!legacy_stale.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn log_directory_honors_codewhale_home_as_hard_override() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("CODEWHALE_HOME", tmp.path());
        }
        // $CODEWHALE_HOME IS the home dir (no ".codewhale" appended), and the
        // legacy ~/.deepseek fallback is bypassed entirely.
        let resolved = log_directory().expect("log_directory should resolve");
        assert_eq!(resolved, tmp.path().join("logs"));
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("CODEWHALE_HOME");
        }
    }
}

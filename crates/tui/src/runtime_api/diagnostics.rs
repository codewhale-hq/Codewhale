//! Crash/log inspection for native clients (APPS-103).
//!
//! This is a read surface, not a telemetry store: it lists and serves files
//! the runtime already writes to disk — `logs/` rolling logs, `audit.log`,
//! and `crashes/*.log` panic dumps — so a client (including a remote or
//! headless one that cannot see the disk) can package an export locally.
//! There is deliberately no upload route and no second log store.
//!
//! Routes:
//!   GET /v1/logs            — recent log/audit entries (name, size, modified)
//!   GET /v1/logs/{name}     — bounded window of one file (?offset, ?limit, ?tail)
//!   GET /v1/crashes         — crash-dump entries
//!   GET /v1/crashes/{name}  — bounded window of one dump
//!   GET /v1/process         — pid, start time, uptime, version, RSS (Linux)

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path as FsPath, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::workspace::encode_window;
use super::{ApiError, RuntimeApiState};

/// Default read window for a file entry.
const READ_LIMIT_DEFAULT: usize = 256 * 1024;
const READ_LIMIT_MAX: usize = 4 * 1024 * 1024;
/// Listing caps — newest first, bounded so a long-lived install cannot
/// produce an unbounded response.
const LOG_LIST_CAP: usize = 64;
const CRASH_LIST_CAP: usize = 64;

/// When this API server came up. `build_router` stamps it once so process
/// facts describe the serving process, not first-call time.
static SERVER_STARTED: OnceLock<(SystemTime, Instant)> = OnceLock::new();

pub(super) fn mark_server_started() {
    let _ = SERVER_STARTED.set((SystemTime::now(), Instant::now()));
}

// ---------------------------------------------------------------------------
// Shared listing + bounded read
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct FileEntry {
    name: String,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileReadQuery {
    offset: Option<u64>,
    limit: Option<usize>,
    /// Convenience tail read: last N bytes of the file.
    tail: Option<u64>,
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

/// Basenames only — a `{name}` path segment must never reach outside the
/// listing directory. Refuse anything that is not a plain file name.
fn safe_entry_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(ApiError::bad_request("name must be a file name"));
    }
    Ok(name.to_string())
}

fn list_files(dir: &FsPath, cap: usize) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            entries.push(FileEntry {
                name,
                size: metadata.len(),
                modified: metadata.modified().ok().map(rfc3339),
            });
        }
    }
    entries.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    entries.truncate(cap);
    entries
}

/// Read `[offset, offset + limit)` of a named file inside `dir` without
/// loading the whole file. Symlinks are never followed.
fn read_named_window(dir: &FsPath, name: &str, query: FileReadQuery) -> Result<Value, ApiError> {
    let path = dir.join(name);
    // Open first without following a final symlink, then take metadata from
    // the handle, so the checked file is the file that is read.
    let mut file = open_no_follow(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ApiError::not_found("file not found"),
        _ if is_symlink_refusal(&error) => ApiError::forbidden("not a regular file"),
        _ => ApiError::internal(format!("file open failed: {error}")),
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| ApiError::internal(format!("file access failed: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ApiError::forbidden("not a regular file"));
    }
    let size = metadata.len();
    let limit = query.limit.unwrap_or(READ_LIMIT_DEFAULT);
    if !(1..=READ_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {READ_LIMIT_MAX} bytes"
        )));
    }
    let offset = match (query.offset, query.tail) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "offset and tail are mutually exclusive",
            ));
        }
        (Some(offset), None) => offset.min(size),
        (None, Some(tail)) => size.saturating_sub(tail.min(size)),
        (None, None) => 0,
    };
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| ApiError::internal(format!("file seek failed: {error}")))?;
    let mut window = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64)
        .read_to_end(&mut window)
        .map_err(|error| ApiError::internal(format!("file read failed: {error}")))?;
    let truncated = offset as usize + window.len() < size as usize;
    let (encoding, content) = encode_window(&window);
    Ok(json!({
        "name": name,
        "size": size,
        "modified": metadata.modified().ok().map(rfc3339),
        "offset": offset,
        "bytes": window.len(),
        "truncated": truncated,
        "encoding": encoding,
        "content": content,
    }))
}

/// Open `path` read-only without following a symlink in its final component.
/// `O_NONBLOCK` keeps a FIFO planted under the name from hanging the open;
/// the regular-file check on the handle rejects it afterwards.
fn open_no_follow(path: &FsPath) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// `O_NOFOLLOW` on a symlink fails with `ELOOP` (and `EMLINK` on some BSDs).
fn is_symlink_refusal(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::EMLINK)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

// ---------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------

/// Log files live under `runtime_log::log_directory()`; the audit trail sits
/// beside them at `<codewhale home>/audit.log[.1]` and is listed as extra
/// entries so one listing covers every text log the runtime writes.
fn log_sources() -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut sources = Vec::new();
    if let Some(dir) = codewhale_runtime::runtime_log::log_directory() {
        let mut singles = Vec::new();
        if let Ok(home) = codewhale_config::codewhale_home() {
            for name in ["audit.log", "audit.log.1"] {
                let path = home.join(name);
                if path.is_file() {
                    singles.push(path);
                }
            }
        }
        sources.push((dir, singles));
    }
    sources
}

/// Read the selected profile's crash store. Default profiles also retain
/// access to legacy dumps; explicit profiles never expose ambient diagnostics.
fn crash_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = codewhale_config::codewhale_home() {
        let dir = home.join("crashes");
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    if !codewhale_config::codewhale_home_is_explicit()
        && let Ok(home) = codewhale_config::legacy_deepseek_home()
    {
        let dir = home.join("crashes");
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub(super) async fn list_logs(State(_state): State<RuntimeApiState>) -> Json<Value> {
    // Directory walks and per-file stats are blocking syscalls, and a
    // diagnostics read must never park a Tokio worker — least of all while
    // the thing being diagnosed is the runtime's responsiveness (#6149).
    let sources = tokio::task::spawn_blocking(list_log_sources)
        .await
        .unwrap_or_default();
    Json(json!({ "sources": sources }))
}

fn list_log_sources() -> Vec<Value> {
    let mut sources = Vec::new();
    for (dir, singles) in log_sources() {
        let mut entries = list_files(&dir, LOG_LIST_CAP);
        for path in singles {
            if let Ok(metadata) = std::fs::symlink_metadata(&path)
                && metadata.is_file()
                && !metadata.file_type().is_symlink()
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
            {
                entries.push(FileEntry {
                    name: name.to_string(),
                    size: metadata.len(),
                    modified: metadata.modified().ok().map(rfc3339),
                });
            }
        }
        entries.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.name.cmp(&b.name))
        });
        entries.truncate(LOG_LIST_CAP);
        sources.push(json!({
            "dir": dir,
            "files": entries,
        }));
    }
    sources
}

pub(super) async fn read_log(
    State(_state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Query(query): Query<FileReadQuery>,
) -> Result<Json<Value>, ApiError> {
    let name = safe_entry_name(&name)?;
    let body = tokio::task::spawn_blocking(move || {
        // The audit trail is listed beside the log dir; resolve it from the
        // codewhale home rather than the log directory.
        if name == "audit.log" || name == "audit.log.1" {
            let home = codewhale_config::codewhale_home()
                .map_err(|error| ApiError::internal(format!("home unavailable: {error}")))?;
            return read_named_window(&home, &name, query);
        }
        let dir = codewhale_runtime::runtime_log::log_directory()
            .ok_or_else(|| ApiError::not_found("no log directory"))?;
        read_named_window(&dir, &name, query)
    })
    .await
    .map_err(|_| ApiError::internal("log read failed"))??;
    Ok(Json(body))
}

pub(super) async fn list_crashes(State(_state): State<RuntimeApiState>) -> Json<Value> {
    // Same reason as `list_logs`: `list_files` stats every entry.
    let sources = tokio::task::spawn_blocking(list_crash_sources)
        .await
        .unwrap_or_default();
    Json(json!({ "sources": sources }))
}

fn list_crash_sources() -> Vec<Value> {
    let mut sources = Vec::new();
    for dir in crash_dirs() {
        sources.push(json!({
            "dir": dir,
            "files": list_files(&dir, CRASH_LIST_CAP),
        }));
    }
    sources
}

pub(super) async fn read_crash(
    State(_state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Query(query): Query<FileReadQuery>,
) -> Result<Json<Value>, ApiError> {
    let name = safe_entry_name(&name)?;
    let body = tokio::task::spawn_blocking(move || {
        for dir in crash_dirs() {
            let candidate = dir.join(&name);
            if std::fs::symlink_metadata(&candidate)
                .map(|m| m.is_file() && !m.file_type().is_symlink())
                .unwrap_or(false)
            {
                return read_named_window(&dir, &name, query);
            }
        }
        Err(ApiError::not_found("crash capture not found"))
    })
    .await
    .map_err(|_| ApiError::internal("crash read failed"))??;
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// GET /v1/process
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(not(target_os = "linux"))]
fn rss_bytes() -> Option<u64> {
    None
}

pub(super) async fn process_info(State(_state): State<RuntimeApiState>) -> Json<Value> {
    // `rss_bytes` reads /proc on Linux and `current_exe` hits the filesystem;
    // both are blocking, and this route is polled for live health.
    let (executable, rss) =
        tokio::task::spawn_blocking(|| (std::env::current_exe().ok(), rss_bytes()))
            .await
            .unwrap_or((None, None));
    let (started_at, uptime_secs) = match SERVER_STARTED.get() {
        Some((system, instant)) => (Some(rfc3339(*system)), Some(instant.elapsed().as_secs())),
        None => (None, None),
    };
    Json(json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("CODEWHALE_BUILD_COMMIT").unwrap_or("unknown"),
        "started_at": started_at,
        "uptime_seconds": uptime_secs,
        "executable": executable,
        "rss_bytes": rss,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use axum::http::StatusCode;

    #[test]
    fn explicit_profile_never_lists_ambient_crashes() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _lock = lock_test_env();
        let tmp = tempfile::tempdir().expect("temp");
        let _home = EnvVarGuard::set("HOME", tmp.path());
        for base in [".codewhale", ".deepseek"] {
            std::fs::create_dir_all(tmp.path().join(base).join("crashes"))
                .expect("ambient fixture");
        }
        let profile = tmp.path().join("selected");
        let _profile = EnvVarGuard::set("CODEWHALE_HOME", &profile);
        assert!(crash_dirs().is_empty(), "no fallback for a fresh profile");
        let selected = profile.join("crashes");
        std::fs::create_dir_all(&selected).expect("selected fixture");
        assert_eq!(crash_dirs(), vec![selected]);
    }

    #[test]
    fn invalid_profile_does_not_fall_back_to_ambient_crashes() {
        let _lock = crate::test_support::lock_test_env();
        let _profile = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", "relative-profile");
        assert!(crash_dirs().is_empty());
    }

    fn whole_file() -> FileReadQuery {
        FileReadQuery {
            offset: None,
            limit: None,
            tail: None,
        }
    }

    #[test]
    fn named_window_reads_a_regular_file() {
        let dir = tempfile::TempDir::new().expect("temp");
        std::fs::write(dir.path().join("a.log"), b"hello").expect("write");
        let body = read_named_window(dir.path(), "a.log", whole_file()).expect("read");
        assert_eq!(body["bytes"], 5);
        assert_eq!(body["size"], 5);
    }

    #[cfg(unix)]
    #[test]
    fn named_window_refuses_a_symlink_at_open() {
        let dir = tempfile::TempDir::new().expect("temp");
        let outside = tempfile::TempDir::new().expect("temp");
        let secret = outside.path().join("secret");
        std::fs::write(&secret, b"do not serve").expect("write");
        std::os::unix::fs::symlink(&secret, dir.path().join("a.log")).expect("symlink");
        let error = read_named_window(dir.path(), "a.log", whole_file())
            .expect_err("symlink must be refused");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    #[cfg(unix)]
    #[test]
    fn named_window_refuses_a_fifo_without_blocking() {
        let dir = tempfile::TempDir::new().expect("temp");
        let fifo = dir.path().join("a.log");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("cstr");
        // SAFETY: `c_path` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let error =
            read_named_window(dir.path(), "a.log", whole_file()).expect_err("fifo must be refused");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }
}

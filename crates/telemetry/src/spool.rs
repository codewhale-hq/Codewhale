//! Per-process spool files and the once-only recovery of dead processes' spools.
//!
//! Known limitation: a spool is only as trustworthy as the user account, like
//! every other file here. Recovery therefore re-validates the header and every
//! event as untrusted input instead of inheriting any bound.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::buffer;
use crate::event::{NOTICE_VERSION, SCHEMA_VERSION, Surface, is_release_version_string};

/// Dead spools sent at one start. Older or surplus ones are deleted unsent.
pub const MAX_RECOVERED: usize = 4;
/// A dead spool older than this is deleted unsent.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const HEADER_VERSION: u32 = 1;
const MAX_VERSION_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpoolContext {
    spool: u32,
    pub schema_version: u32,
    pub notice_version: u32,
    pub app_version: String,
    pub git_sha: Option<String>,
    pub surface: Surface,
    pub tty: bool,
}

impl SpoolContext {
    pub(crate) fn new(
        surface: Surface,
        app_version: String,
        git_sha: Option<String>,
        tty: bool,
    ) -> Self {
        Self {
            spool: HEADER_VERSION,
            schema_version: SCHEMA_VERSION,
            notice_version: NOTICE_VERSION,
            app_version,
            git_sha,
            surface,
            tty,
        }
    }

    pub(crate) fn is_sendable(&self) -> bool {
        self.spool == HEADER_VERSION
            && self.schema_version == SCHEMA_VERSION
            && self.notice_version == NOTICE_VERSION
            && self.app_version.len() <= MAX_VERSION_BYTES
            && is_release_version_string(&self.app_version)
            && self.git_sha.as_deref().is_none_or(|sha| {
                sha.len() == 12 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
    }
}

pub(crate) fn is_header_line(line: &str) -> bool {
    serde_json::from_str::<SpoolContext>(line).is_ok()
}

/// `$CODEWHALE_HOME/telemetry/spool`.
#[must_use]
pub fn dir(root: &Path) -> PathBuf {
    root.join("spool")
}

/// Every spool data file under `root`, for tests and diagnostics.
#[must_use]
pub fn data_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    files
}

fn data_path(root: &Path, nonce: &str) -> PathBuf {
    dir(root).join(format!("{nonce}.jsonl"))
}

fn lock_file_path(root: &Path, nonce: &str) -> PathBuf {
    dir(root).join(format!("{nonce}.lock"))
}

fn is_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn open_lock_file(path: &Path) -> Option<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
}

fn hold_lock(path: &Path) -> bool {
    let Some(file) = open_lock_file(path) else {
        return false;
    };
    let lock = Box::leak(Box::new(fd_lock::RwLock::new(file)));
    match lock.try_write() {
        Ok(guard) => {
            std::mem::forget(guard);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn open(root: &Path, context: &SpoolContext) -> (PathBuf, String) {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let path = data_path(root, &nonce);
    if buffer::ensure_dir(&dir(root)).is_ok()
        && hold_lock(&lock_file_path(root, &nonce))
        && let Ok(header) = serde_json::to_string(context)
    {
        let _ = buffer::with_lock(root, || buffer::append_header(root, &path, &header));
    }
    (path, nonce)
}

#[must_use]
pub(crate) fn take_own(root: &Path, path: &Path) -> Vec<String> {
    if buffer::tombstone_present(root) {
        return Vec::new();
    }
    buffer::try_with_lock(root, || {
        if buffer::tombstone_present(root) {
            return Ok(Vec::new());
        }
        let lines = buffer::read_lines(path);
        if lines.is_empty() {
            return Ok(lines);
        }
        if remove_or_truncate(path) {
            Ok(lines)
        } else {
            Ok(Vec::new())
        }
    })
    .ok()
    .flatten()
    .unwrap_or_default()
}

fn remove_or_truncate(path: &Path) -> bool {
    fs::remove_file(path).is_ok() || buffer::truncate(path).is_ok_and(|()| !path.exists())
}

#[derive(Debug)]
pub(crate) struct Recovered {
    pub context: SpoolContext,
    pub lines: Vec<String>,
}

pub(crate) fn take_dead(root: &Path, own_nonce: &str, now: SystemTime) -> Vec<Recovered> {
    let Ok(entries) = fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut stems = BTreeSet::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some((stem, ext)) = name.rsplit_once('.')
            && matches!(ext, "jsonl" | "lock")
            && is_nonce(stem)
            && stem != own_nonce
        {
            stems.insert(stem.to_string());
        }
    }
    let mut ranked: Vec<(SystemTime, String)> = stems
        .into_iter()
        .map(|stem| {
            let modified = [data_path(root, &stem), lock_file_path(root, &stem)]
                .iter()
                .find_map(|path| fs::symlink_metadata(path).and_then(|m| m.modified()).ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, stem)
        })
        .collect();
    ranked.sort_by(|a, b| b.cmp(a));

    let mut recovered = Vec::new();
    for (modified, stem) in ranked {
        if buffer::tombstone_present(root) {
            break;
        }
        let data = data_path(root, &stem);
        let lock_path = lock_file_path(root, &stem);
        let eligible = recovered.len() < MAX_RECOVERED
            && now.duration_since(modified).is_ok_and(|age| age <= MAX_AGE);
        let claimed = with_dead_lock(&lock_path, || {
            let claimed = if eligible { claim(&data) } else { None };
            if claimed.is_none() {
                let _ = fs::remove_file(&data);
            }
            let _ = fs::remove_file(&lock_path);
            claimed
        });
        if let Some(Some(claimed)) = claimed {
            recovered.push(claimed);
        }
    }
    recovered
}

fn with_dead_lock<T>(lock_path: &Path, operation: impl FnOnce() -> T) -> Option<T> {
    let file = open_lock_file(lock_path)?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.try_write().ok()?;
    Some(operation())
}

fn claim(data: &Path) -> Option<Recovered> {
    let meta = fs::symlink_metadata(data).ok()?;
    if !meta.is_file() {
        return None;
    }
    let lines = buffer::read_lines(data);
    let (header, events) = lines.split_first()?;
    let context = serde_json::from_str::<SpoolContext>(header).ok()?;
    if !context.is_sendable() || events.is_empty() || !remove_or_truncate(data) {
        return None;
    }
    Some(Recovered {
        context,
        lines: events.to_vec(),
    })
}

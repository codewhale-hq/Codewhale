//! Find and scrub credentials that older builds stored in saved sessions.
//!
//! Tool output is redacted as it enters the transcript now (B1), so new
//! session files never hold a live token. Sessions written before that fix
//! can: the redaction used to run only when a request was built, while the
//! transcript on disk kept the raw tool output (a `cat ~/.codex/auth.json`
//! result, a printed bearer token). Nothing rewrites those files on its own —
//! `codewhale doctor` reports them and `codewhale sessions scrub-secrets`
//! rewrites them on request.
//!
//! Only transcript tool results, transcript tool-call arguments and Runtime
//! tool receipts (including event copies) are touched. Cleanup masks credential
//! shapes and currently configured secret values; bare secrets absent from
//! current configuration cannot be recovered.
//! Runtime rewrites require the exclusive process lease, and session rewrites
//! the session's live lease; busy stores and sessions open in an interactive
//! surface are reported and left untouched while the remaining files are
//! scrubbed.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The documented command that rewrites affected sessions.
pub(crate) const SCRUB_COMMAND: &str = "codewhale sessions scrub-secrets";

/// What a scan found (and, when applied, rewrote).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ScrubReport {
    /// Session, checkpoint, and Runtime receipt files examined.
    pub files_scanned: usize,
    /// Files holding at least one unredacted credential in tool output.
    pub flagged_files: Vec<PathBuf>,
    /// Tool-result text fields that contained a credential.
    pub flagged_tool_results: usize,
    /// Files the scan could not read or parse, left untouched.
    pub unreadable: Vec<PathBuf>,
    /// Credential-bearing files left untouched because their Runtime store is
    /// live or their session is open in an interactive surface.
    pub busy: Vec<PathBuf>,
}

/// Session/checkpoint JSON and Runtime items/events, newest first. Resolve the
/// user-selected roots (including the fixed checkpoints root), but never follow
/// symlink entries discovered inside those roots. Enumeration failures are partial
/// coverage, not a reason to discard the files that can still be scrubbed.
pub(crate) fn session_files(
    sessions_dir: &Path,
    standalone_runtime: &Path,
    unreadable: &mut Vec<PathBuf>,
) -> Vec<PathBuf> {
    let sessions_root = canonical_store_root(sessions_dir, unreadable);
    let mut dirs = Vec::new();
    let mut runtime_roots = Vec::new();
    if let Some(root) = canonical_store_root(standalone_runtime, unreadable) {
        runtime_roots.push(root);
    }
    if let Some(root) = sessions_root {
        if let Some(checkpoints) = canonical_store_root(&root.join("checkpoints"), unreadable) {
            dirs.push(checkpoints);
        }
        // Pre-import transcript copies hold the same tool output. Session
        // deletion does not follow a linked archive directory, so neither does
        // the scrub: it is reported as not covered instead.
        let archive = root.join(crate::session_manager::WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        match std::fs::symlink_metadata(&archive) {
            Ok(metadata) if metadata.is_dir() => dirs.push(archive),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => unreadable.push(archive),
        }
        for session in directory_entries(&root, unreadable) {
            if !entry_has_type(&session, true, unreadable) {
                continue;
            }
            for runtime in directory_entries(&session.path(), unreadable) {
                let name = runtime.file_name();
                let name = name.to_string_lossy();
                if (name == "runtime" || name.starts_with("runtime-recovered-"))
                    && entry_has_type(&runtime, true, unreadable)
                {
                    runtime_roots.push(runtime.path());
                }
            }
        }
        dirs.push(root);
    }
    for root in runtime_roots {
        dirs.push(root.join("items"));
        dirs.push(root.join("events"));
    }
    let mut files = Vec::new();
    for dir in dirs {
        for entry in directory_entries(&dir, unreadable) {
            let path = entry.path();
            if !entry_has_type(&entry, false, unreadable)
                || !matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("json" | "jsonl")
                )
            {
                continue;
            }
            match entry.metadata() {
                Ok(meta) => files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path)),
                Err(_) => unreadable.push(path),
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    files.dedup_by(|a, b| a.1 == b.1);
    unreadable.sort();
    unreadable.dedup();
    files.into_iter().map(|(_, path)| path).collect()
}

fn canonical_store_root(dir: &Path, unreadable: &mut Vec<PathBuf>) -> Option<PathBuf> {
    match std::fs::canonicalize(dir) {
        Ok(root) => Some(root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            unreadable.push(dir.to_path_buf());
            None
        }
    }
}

fn entry_has_type(
    entry: &std::fs::DirEntry,
    directory: bool,
    unreadable: &mut Vec<PathBuf>,
) -> bool {
    match entry.file_type() {
        Ok(kind) => {
            if directory {
                kind.is_dir()
            } else {
                kind.is_file()
            }
        }
        Err(_) => {
            unreadable.push(entry.path());
            false
        }
    }
}

fn directory_entries(dir: &Path, unreadable: &mut Vec<PathBuf>) -> Vec<std::fs::DirEntry> {
    let entries = match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Ok(meta) if meta.is_dir() => std::fs::read_dir(dir).ok(),
        _ => None,
    };
    let Some(entries) = entries else {
        unreadable.push(dir.to_path_buf());
        return Vec::new();
    };
    entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(_) => {
                unreadable.push(dir.to_path_buf());
                None
            }
        })
        .collect()
}

/// Reserve an independent budget for transcripts/checkpoints and Runtime receipts,
/// so one busy Runtime cannot evict every saved conversation from doctor's scan.
pub(crate) fn doctor_files(files: Vec<PathBuf>, per_category: usize) -> Vec<PathBuf> {
    let mut counts = [0, 0];
    files
        .into_iter()
        .filter(|path| {
            let count = &mut counts[usize::from(runtime_root(path).is_some())];
            *count += 1;
            *count <= per_category
        })
        .collect()
}

/// Called once per command from its blocking worker. This is read-only, including
/// active-key resolution; the resulting values must never appear in a report.
pub(crate) fn configured_secrets(
    config_path: Option<PathBuf>,
    profile: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let config = crate::plugins::providers::load_config(config_path, profile)?;
    Ok(crate::client::active_model_bound_secret_values(&config))
}

/// Whether `path` sits directly in `manager`'s pre-import archive directory
/// (never a linked one; see [`session_files`]).
fn is_import_archive(path: &Path, manager: &crate::session_manager::SessionManager) -> bool {
    let archive = manager
        .sessions_dir()
        .join(crate::session_manager::WORK_GRAPH_IMPORT_ARCHIVE_DIR);
    std::fs::symlink_metadata(&archive).is_ok_and(|metadata| metadata.is_dir())
        && std::fs::canonicalize(&archive)
            .is_ok_and(|archive| path.parent() == Some(archive.as_path()))
}

fn runtime_root(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    matches!(parent.file_name()?.to_str()?, "items" | "events")
        .then(|| parent.parent())
        .flatten()
}

/// Scan `files`; with `apply`, rewrite each affected file atomically with its
/// tool-result credentials masked. Each rewrite re-reads its file under the
/// same per-session lock every session save takes, so a live session saving
/// at the same moment is never overwritten with older content.
pub(crate) fn scrub_files(
    files: &[PathBuf],
    apply: Option<&crate::session_manager::SessionManager>,
    secrets: &[String],
) -> io::Result<ScrubReport> {
    let mut report = ScrubReport::default();
    for path in files {
        report.files_scanned += 1;
        // The scan is read-only; its counts are what the report prints.
        let scan = scrub_file(path, false, secrets)?;
        let scan = match (apply, scan) {
            (Some(_), FileScan::Dirty(_)) if runtime_root(path).is_some() => {
                let _lease = match crate::runtime_threads::RuntimeProcessOwnerLock::acquire(
                    runtime_root(path).unwrap(),
                ) {
                    Ok(lease) => lease,
                    Err(error)
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock) =>
                    {
                        report.busy.push(path.clone());
                        continue;
                    }
                    Err(error) => return Err(io::Error::other(error)),
                };
                scrub_file(path, true, secrets)?
            }
            (Some(manager), FileScan::Dirty(redacted)) => {
                // `<id>.json` and `checkpoints/<id>.json` share the id's lock.
                // The rewrite re-reads the file under that lock, so it masks
                // whatever the file holds at that moment.
                let session_id = path.file_stem().and_then(|stem| stem.to_str());
                match session_id.map(|id| {
                    manager
                        .with_session_file_lock(id, || scrub_file(path, true, secrets).map(|_| ()))
                }) {
                    Some(Ok(Some(()))) => FileScan::Dirty(redacted),
                    // A pre-import copy an earlier build left behind when it
                    // deleted the session. Finish that deletion: rewriting it
                    // would keep a deleted transcript, and could put back a
                    // copy a concurrent delete had just removed.
                    Some(Ok(None)) if is_import_archive(path, manager) => {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                        FileScan::Dirty(redacted)
                    }
                    // A deleted session is not resurrected by a rewrite.
                    Some(Ok(None)) => FileScan::Clean,
                    // No lockable session id: leave the file untouched.
                    None => FileScan::Unreadable,
                    Some(Err(error)) if error.kind() == io::ErrorKind::InvalidInput => {
                        FileScan::Unreadable
                    }
                    // Open in an interactive session, whose next autosave
                    // would put the credential back: report and leave it.
                    Some(Err(error)) if error.kind() == io::ErrorKind::ResourceBusy => {
                        report.busy.push(path.clone());
                        continue;
                    }
                    Some(Err(error)) => return Err(error),
                }
            }
            (_, scan) => scan,
        };
        match scan {
            FileScan::Unreadable => report.unreadable.push(path.clone()),
            FileScan::Clean => {}
            FileScan::Dirty(redacted) => {
                report.flagged_tool_results += redacted;
                report.flagged_files.push(path.clone());
            }
        }
    }
    Ok(report)
}

enum FileScan {
    Unreadable,
    Clean,
    Dirty(usize),
}

fn scrub_file(path: &Path, apply: bool, secrets: &[String]) -> io::Result<FileScan> {
    let Ok(raw) = std::fs::read(path) else {
        return Ok(FileScan::Unreadable);
    };
    let jsonl = path.extension().and_then(|ext| ext.to_str()) == Some("jsonl");
    let parsed = if jsonl {
        serde_json::Deserializer::from_slice(&raw)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()
    } else {
        serde_json::from_slice::<Value>(&raw).map(|value| vec![value])
    };
    let Ok(mut values) = parsed else {
        return Ok(FileScan::Unreadable);
    };
    let redacted = values
        .iter_mut()
        .map(|value| scrub_value(value, secrets))
        .sum();
    if redacted == 0 {
        return Ok(FileScan::Clean);
    }
    if apply {
        let mut bytes = Vec::new();
        for value in values {
            if jsonl {
                serde_json::to_writer(&mut bytes, &value).map_err(io::Error::other)?;
                bytes.push(b'\n');
            } else {
                serde_json::to_writer_pretty(&mut bytes, &value).map_err(io::Error::other)?;
            }
        }
        codewhale_config::persistence::atomic_write(path, &bytes).map_err(io::Error::other)?;
    }
    Ok(FileScan::Dirty(redacted))
}

/// Mask credentials in every `tool_result` and `tool_call` argument below
/// `value`. Returns how many fields changed.
fn scrub_value(value: &mut Value, secrets: &[String]) -> usize {
    match value {
        Value::Object(map) => {
            let mut changed = 0;
            // Owned, so the two `get_mut` calls below can borrow the map.
            let block_type = map.get("type").and_then(Value::as_str).map(str::to_string);
            if block_type.as_deref() == Some("tool_result") {
                if let Some(Value::String(content)) = map.get_mut("content") {
                    changed += usize::from(scrub_text(content, secrets));
                }
                if let Some(Value::Array(blocks)) = map.get_mut("content_blocks") {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(Value::String(text)) = block.get_mut("text")
                        {
                            changed += usize::from(scrub_text(text, secrets));
                        }
                    }
                }
            }
            if matches!(block_type.as_deref(), Some("tool_use" | "server_tool_use"))
                && let Some(input) = map.get_mut("input")
            {
                let redacted = crate::client::redact_json_model_bound_text(input, secrets);
                if *input != redacted {
                    *input = redacted;
                    changed += 1;
                }
            }
            if map.get("kind").and_then(Value::as_str) == Some("tool_call") {
                for key in ["summary", "detail", "metadata"] {
                    if let Some(value) = map.get_mut(key) {
                        let redacted = crate::client::redact_json_model_bound_text(value, secrets);
                        if *value != redacted {
                            *value = redacted;
                            changed += 1;
                        }
                    }
                }
            }
            // Strings below are only rewritten through a tool_result or receipt above,
            // so walking the rest (including this object's own fields) never
            // double-counts.
            changed
                + map
                    .values_mut()
                    .map(|value| scrub_value(value, secrets))
                    .sum::<usize>()
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|value| scrub_value(value, secrets))
            .sum(),
        _ => 0,
    }
}

fn scrub_text(text: &mut String, secrets: &[String]) -> bool {
    let redacted = crate::client::redact_model_bound_text(text, secrets);
    if redacted == *text {
        return false;
    }
    *text = redacted;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefghij";

    fn session_with_tool_output(output: &str) -> Value {
        json!({
            "schema_version": 1,
            "metadata": {"id": "s1"},
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "show auth"}]},
                {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": output,
                    "content_blocks": [{"type": "text", "text": output}]
                }]}
            ],
            "journal": {"entries": [{"message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "call-1", "content": output
            }]}}]}
        })
    }

    #[test]
    fn scan_reports_and_scrub_masks_stored_tool_output_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = format!("{{\"access_token\": \"{TOKEN}\"}}");
        let dirty = dir.path().join("dirty.json");
        std::fs::write(&dirty, session_with_tool_output(&output).to_string()).unwrap();
        let clean = dir.path().join("clean.json");
        std::fs::write(
            &clean,
            session_with_tool_output("nothing secret").to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("checkpoints")).unwrap();
        let checkpoint = dir.path().join("checkpoints").join("dirty.json");
        std::fs::write(&checkpoint, session_with_tool_output(&output).to_string()).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{not json").unwrap();

        let files = session_files(dir.path(), &dir.path().join("standalone"), &mut Vec::new());
        assert_eq!(files.len(), 4, "{files:?}");

        let report = scrub_files(&files, None, &[]).expect("scan");
        assert_eq!(report.files_scanned, 4);
        assert_eq!(report.flagged_files.len(), 2, "{report:?}");
        assert_eq!(report.flagged_tool_results, 6);
        assert_eq!(
            report.unreadable,
            vec![dir.path().join("broken.json").canonicalize().unwrap()]
        );
        assert!(
            std::fs::read_to_string(&dirty).unwrap().contains(TOKEN),
            "a scan must not rewrite anything"
        );

        let manager =
            crate::session_manager::SessionManager::new(dir.path().to_path_buf()).expect("manager");
        let applied = scrub_files(&files, Some(&manager), &[]).expect("scrub");
        assert_eq!(applied.flagged_files.len(), 2);
        assert_eq!(
            applied.unreadable,
            vec![dir.path().join("broken.json").canonicalize().unwrap()]
        );
        for path in [&dirty, &checkpoint] {
            let text = std::fs::read_to_string(path).unwrap();
            assert!(!text.contains(TOKEN), "{text}");
            let value: Value = serde_json::from_str(&text).expect("still valid JSON");
            assert_eq!(value["metadata"]["id"], "s1");
            assert_eq!(value["messages"][0]["content"][0]["text"], "show auth");
        }
        assert_eq!(
            scrub_files(&files, None, &[])
                .expect("rescan")
                .flagged_files,
            Vec::<PathBuf>::new(),
            "a scrubbed store scans clean"
        );
    }

    /// The pre-import copy of a transcript holds the same tool output. It is
    /// scrubbed with the sessions. A copy an earlier build left behind for a
    /// session it deleted is removed, finishing that deletion.
    #[test]
    fn work_graph_import_archive_copies_are_scrubbed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = format!("{{\"access_token\": \"{TOKEN}\"}}");
        let manager =
            crate::session_manager::SessionManager::new(dir.path().to_path_buf()).expect("manager");
        std::fs::write(
            dir.path().join("gone.json"),
            session_with_tool_output("nothing").to_string(),
        )
        .unwrap();
        manager.delete_session("gone").expect("delete");
        let archive_dir = dir
            .path()
            .join(crate::session_manager::WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        std::fs::create_dir_all(&archive_dir).unwrap();
        let kept = archive_dir.join("kept.json");
        let orphan = archive_dir.join("gone.json");
        for path in [&kept, &orphan] {
            std::fs::write(path, session_with_tool_output(&output).to_string()).unwrap();
        }

        let files = session_files(dir.path(), &dir.path().join("standalone"), &mut Vec::new());
        let report = scrub_files(&files, Some(&manager), &[]).expect("scrub");

        assert_eq!(report.flagged_files.len(), 2, "{report:?}");
        let text = std::fs::read_to_string(&kept).unwrap();
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!orphan.exists(), "the deleted session's copy is removed");
    }

    /// A linked archive directory is not followed, matching session deletion.
    #[cfg(unix)]
    #[test]
    fn a_linked_import_archive_is_reported_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let outside = elsewhere.join("gone.json");
        let output = format!("{{\"access_token\": \"{TOKEN}\"}}");
        std::fs::write(&outside, session_with_tool_output(&output).to_string()).unwrap();
        std::os::unix::fs::symlink(
            &elsewhere,
            sessions.join(crate::session_manager::WORK_GRAPH_IMPORT_ARCHIVE_DIR),
        )
        .unwrap();

        let mut unreadable = Vec::new();
        let files = session_files(&sessions, &dir.path().join("standalone"), &mut unreadable);

        assert!(
            !files.iter().any(|file| file.ends_with("gone.json")),
            "{files:?}"
        );
        assert_eq!(unreadable.len(), 1, "{unreadable:?}");
        assert!(unreadable[0].ends_with(crate::session_manager::WORK_GRAPH_IMPORT_ARCHIVE_DIR));
        assert!(std::fs::read_to_string(&outside).unwrap().contains(TOKEN));
    }

    /// An interactive session holds the conversation in memory; its next
    /// autosave would put a masked credential back. Such a session is
    /// reported busy and left byte-for-byte, like a live Runtime store.
    #[test]
    fn a_session_open_in_an_interactive_surface_is_reported_busy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = format!("{{\"access_token\": \"{TOKEN}\"}}");
        let open = dir.path().join("open-elsewhere.json");
        std::fs::write(&open, session_with_tool_output(&output).to_string()).unwrap();
        let before = std::fs::read(&open).unwrap();
        let manager =
            crate::session_manager::SessionManager::new(dir.path().to_path_buf()).expect("manager");
        let _held = manager.hold_live_lease_elsewhere("open-elsewhere");

        let files = session_files(dir.path(), &dir.path().join("standalone"), &mut Vec::new());
        let report = scrub_files(&files, Some(&manager), &[]).expect("scrub");
        assert_eq!(
            report.busy,
            vec![open.canonicalize().unwrap()],
            "{report:?}"
        );
        assert!(report.flagged_files.is_empty(), "{report:?}");
        assert_eq!(std::fs::read(&open).unwrap(), before);
    }

    #[test]
    fn busy_runtime_does_not_abort_scrub() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let standalone = dir.path().join("standalone");
        let roots = [sessions.join("s1/runtime"), standalone.clone()];
        let item = json!({"id":"item1", "turn_id":"turn1", "kind":"tool_call", "status":"completed", "summary": TOKEN, "detail": TOKEN, "metadata":{"stdout_summary":TOKEN, "exit_code":0}});
        let event = json!({"seq": 7, "payload":{"item":item}});
        for root in &roots {
            std::fs::create_dir_all(root.join("items")).unwrap();
            std::fs::create_dir_all(root.join("events")).unwrap();
            std::fs::write(root.join("items/item1.json"), item.to_string()).unwrap();
            std::fs::write(root.join("events/thread1.jsonl"), format!("{event}\n")).unwrap();
        }
        let files = session_files(&sessions, &standalone, &mut Vec::new());
        let report = scrub_files(&files, None, &[]).unwrap();
        assert_eq!(report.files_scanned, 4);
        assert_eq!(report.flagged_files.len(), 4);
        assert_eq!(report.flagged_tool_results, 12);
        let transcript = sessions.join("s1.json");
        std::fs::write(&transcript, session_with_tool_output(TOKEN).to_string()).unwrap();
        let manager = crate::session_manager::SessionManager::new(sessions).unwrap();
        let lease = crate::runtime_threads::RuntimeProcessOwnerLock::acquire(&standalone).unwrap();
        let standalone_file = standalone.join("items/item1.json");
        let report = scrub_files(
            &[standalone_file.clone(), transcript.clone()],
            Some(&manager),
            &[],
        )
        .unwrap();
        assert_eq!(report.busy, vec![standalone_file.clone()]);
        assert_eq!(report.flagged_files, vec![transcript.clone()]);
        assert_eq!(report.files_scanned, 2);
        assert!(
            !std::fs::read_to_string(&transcript)
                .unwrap()
                .contains(TOKEN)
        );
        assert!(
            std::fs::read_to_string(&standalone_file)
                .unwrap()
                .contains(TOKEN)
        );
        drop(lease);
        // Non-contention failures still propagate, rather than masquerading as busy.
        let lock_path = standalone.join("runtime-process.owner.lock");
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::create_dir(&lock_path).unwrap();
        assert!(scrub_files(std::slice::from_ref(&standalone_file), Some(&manager), &[]).is_err());
        std::fs::remove_dir(&lock_path).unwrap();
        scrub_files(&files, Some(&manager), &[]).unwrap();
        assert!(
            scrub_files(&files, None, &[])
                .unwrap()
                .flagged_files
                .is_empty()
        );
        for path in &files {
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(!raw.contains(TOKEN));
            let value: Value = serde_json::from_str(&raw).unwrap();
            let item = if runtime_root(path).unwrap().join("items") == path.parent().unwrap() {
                &value
            } else {
                &value["payload"]["item"]
            };
            assert_eq!(item["metadata"]["exit_code"], 0);
            assert_eq!(item["turn_id"], "turn1");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_roots_scan_without_following_entries() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sessions = root.join("sessions-real");
        let checkpoints = root.join("checkpoints-real");
        let runtime = root.join("runtime-real");
        for dir in [&sessions, &checkpoints, &runtime.join("items")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let transcript = sessions.join("s1.json");
        let checkpoint = checkpoints.join("s1.json");
        let item = runtime.join("items/item.json");
        for path in [&transcript, &checkpoint, &item] {
            std::fs::write(path, session_with_tool_output(TOKEN).to_string()).unwrap();
        }
        symlink(&sessions, root.join("sessions")).unwrap();
        symlink(&checkpoints, sessions.join("checkpoints")).unwrap();
        symlink(&runtime, root.join("runtime")).unwrap();
        // Discovered file and session-directory links must not expand the scan.
        symlink(&transcript, sessions.join("outside.json")).unwrap();
        symlink(&runtime, sessions.join("outside-session")).unwrap();
        std::fs::create_dir(sessions.join("s1")).unwrap();
        symlink(&runtime, sessions.join("s1/runtime")).unwrap();
        let mut unreadable = Vec::new();
        let files = session_files(
            &root.join("sessions"),
            &root.join("runtime"),
            &mut unreadable,
        );
        assert!(unreadable.is_empty(), "{unreadable:?}");
        assert_eq!(files.len(), 3);
        for path in [&transcript, &checkpoint, &item] {
            assert!(files.contains(path));
        }
        let manager = crate::session_manager::SessionManager::new(root.join("sessions")).unwrap();
        assert_eq!(
            scrub_files(&files, Some(&manager), &[])
                .unwrap()
                .flagged_files
                .len(),
            3
        );
        assert!(
            scrub_files(&files, None, &[])
                .unwrap()
                .flagged_files
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_keeps_partial_scan() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let blocked = root.join("blocked-session");
        std::fs::create_dir(&blocked).unwrap();
        let transcript = root.join("s1.json");
        std::fs::write(&transcript, session_with_tool_output(TOKEN).to_string()).unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut unreadable = Vec::new();
        let files = session_files(&root, &root.join("absent-runtime"), &mut unreadable);
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(unreadable, vec![blocked]);
        assert_eq!(files, vec![transcript]);
        let mut report = scrub_files(&files, None, &[]).unwrap();
        report.unreadable = unreadable;
        assert_eq!(report.flagged_files.len(), 1);
        let summary = crate::doctor_stored_secrets_summary(&report, files.len());
        assert!(summary.contains("scan incomplete") && summary.contains("blocked-session"));
        assert!(!summary.contains('✓'));
    }

    #[test]
    fn configured_bare_secret_is_scrubbed() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _env = lock_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let _user_home = EnvVarGuard::set("HOME", tmp.path());
        let _profile = EnvVarGuard::set("USERPROFILE", tmp.path());
        let config = tmp.path().join("config.toml");
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", &config);
        let secret = "abcdefghijklmnopqrst";
        std::fs::write(&config, format!("sandbox_api_key = \"{secret}\"\n")).unwrap();
        let secrets = configured_secrets(Some(config), None).unwrap();
        assert!(secrets.iter().any(|value| value == secret));
        let sessions = tmp.path().join("sessions");
        let runtime = sessions.join("s1/runtime");
        std::fs::create_dir_all(runtime.join("items")).unwrap();
        std::fs::create_dir_all(runtime.join("events")).unwrap();
        let transcript = sessions.join("s1.json");
        let item = runtime.join("items/item.json");
        let event = runtime.join("events/thread.jsonl");
        let receipt = json!({"kind":"tool_call", "summary":secret, "detail":secret, "metadata":{"stdout_summary":secret, "exit_code":0}});
        std::fs::write(&transcript, session_with_tool_output(secret).to_string()).unwrap();
        std::fs::write(&item, receipt.to_string()).unwrap();
        std::fs::write(&event, format!("{}\n", json!({"payload":{"item":receipt}}))).unwrap();
        let files = vec![transcript, item, event];
        assert!(
            scrub_files(&files, None, &[])
                .unwrap()
                .flagged_files
                .is_empty()
        );
        let report = scrub_files(&files, None, &secrets).unwrap();
        assert_eq!(report.flagged_files.len(), 3);
        assert_eq!(report.flagged_tool_results, 9);
        let manager = crate::session_manager::SessionManager::new(sessions).unwrap();
        scrub_files(&files, Some(&manager), &secrets).unwrap();
        for path in files {
            let raw = std::fs::read_to_string(path).unwrap();
            assert!(!raw.contains(secret));
            assert!(raw.contains(codewhale_config::persistence::REDACTED));
        }
    }

    #[test]
    fn doctor_reserves_transcript_scan_budget() {
        // Already ordered newest-first: an active store has over 50 receipts,
        // all newer than the credential-bearing transcript and checkpoint.
        let mut files: Vec<PathBuf> = (0..60)
            .map(|i| PathBuf::from(format!("s1/runtime/items/{i}.json")))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("s1.json");
        let checkpoint = tmp.path().join("checkpoints/s1.json");
        std::fs::create_dir_all(checkpoint.parent().unwrap()).unwrap();
        for path in [&transcript, &checkpoint] {
            std::fs::write(path, session_with_tool_output(TOKEN).to_string()).unwrap();
            files.push(path.clone());
        }
        let checked = doctor_files(files, crate::DOCTOR_SECRET_SCAN_FILES);
        assert_eq!(checked.len(), 52);
        assert!(checked.contains(&transcript) && checked.contains(&checkpoint));
        let report = scrub_files(&checked, None, &[]).unwrap();
        assert_eq!(report.flagged_files, vec![transcript, checkpoint]);
        let summary = crate::doctor_stored_secrets_summary(&report, 62);
        assert!(summary.contains("per category") && summary.contains("sessions/checkpoints"));
    }

    #[test]
    fn non_tool_result_text_is_left_alone() {
        let mut value = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": TOKEN}]}]
        });
        assert_eq!(scrub_value(&mut value, &[]), 0);
        assert_eq!(value["messages"][0]["content"][0]["text"], TOKEN);
    }
}

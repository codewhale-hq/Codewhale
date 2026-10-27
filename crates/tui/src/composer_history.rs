//! Cross-session composer input history (#366).
//!
//! Persists user-typed prompts to `~/.codewhale/composer_history.jsonl`
//! (using the legacy `~/.deepseek` root only when one already exists,
//! #3240) so pressing Up-arrow at the composer recalls
//! submissions from previous sessions, not just the current one. One entry
//! per JSON line, oldest first. Existing `composer_history.txt` files are
//! read verbatim on migration and left intact. The history is
//! capped at [`MAX_HISTORY_ENTRIES`] entries (older entries are pruned
//! at append time). Encoding every entry as a JSON string keeps multiline
//! prompts intact without mistaking old quoted prompts for encoded records.
//!
//! Slash commands are stored as well: recalling `/theme` or `/compact`
//! with Up-arrow is ordinary recall (#6006), and filtering on the `/`
//! prefix also dropped absolute paths like `cat /etc/fstab`. Empty /
//! whitespace-only inputs are still skipped.
//!
//! ## Off-thread writes (#1927)
//!
//! [`append_history`] used to block the caller for a read-then-atomic-
//! rewrite of the full file. That ran on the UI thread inside
//! `submit_input`, contributing a perceptible stall after Enter. The
//! public entry point now hands work to a dedicated writer thread via
//! [`writer_sender`] and returns immediately. Submissions stay serialised
//! in arrival order, so the on-disk file keeps its "oldest first"
//! invariant.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

/// Hard cap on persisted history. Keeps the file small (typical entries
/// are < 200 chars, so 1000 entries ≈ 200 KB) and bounds startup load
/// time.
pub const MAX_HISTORY_ENTRIES: usize = 1000;

const HISTORY_FILE_NAME: &str = "composer_history.jsonl";
const LEGACY_HISTORY_FILE_NAME: &str = "composer_history.txt";

fn default_history_path() -> Option<PathBuf> {
    // Submitting a prompt in a UI test appends to the history; an unsealed test
    // must not write the developer's real `~/.codewhale/composer_history.jsonl`.
    #[cfg(test)]
    if let Some(home) = crate::test_support::unsealed_state_dir(".") {
        return history_path_with_home(Some(home));
    }
    if codewhale_paths::codewhale_home_is_explicit() {
        return codewhale_paths::codewhale_home()
            .ok()
            .flatten()
            .map(|home| home.join(HISTORY_FILE_NAME));
    }
    history_path_with_home(codewhale_paths::user_home())
}

/// Resolve the composer-history file under `home`, preferring the CodeWhale
/// root and only falling back to the legacy `.deepseek` root when a legacy
/// file already exists.
///
/// On a fresh install (neither file present) this returns the `.codewhale`
/// path, so the writer never recreates `~/.deepseek/` at runtime (#3240),
/// while users who haven't migrated keep their existing history in that
/// root. Mirrors the primary/legacy resolution used by
/// `snapshot::paths` and `artifacts`.
fn history_path_with_home(home: Option<PathBuf>) -> Option<PathBuf> {
    let home = home?;
    let primary = home.join(".codewhale").join(HISTORY_FILE_NAME);
    if primary.exists() || primary.with_file_name(LEGACY_HISTORY_FILE_NAME).exists() {
        return Some(primary);
    }
    let legacy = home.join(".deepseek").join(HISTORY_FILE_NAME);
    if legacy.exists() || legacy.with_file_name(LEGACY_HISTORY_FILE_NAME).exists() {
        return Some(legacy);
    }
    Some(primary)
}

/// Read the persisted history into memory. Returns an empty vec if the
/// file doesn't exist or can't be parsed — this is best-effort.
#[must_use]
pub fn load_history() -> Vec<String> {
    let Some(path) = default_history_path() else {
        return Vec::new();
    };
    load_history_from(&path)
}

fn load_history_from(path: &Path) -> Vec<String> {
    read_history_from(path).unwrap_or_else(|err| {
        tracing::warn!(
            "Failed to read composer history at {}: {err}",
            path.display()
        );
        Vec::new()
    })
}

fn read_history_from(path: &Path) -> std::io::Result<Vec<String>> {
    // A separate filename is the format discriminator: arbitrary legacy
    // prompts can themselves be valid JSON strings (including escapes).
    let (source, encoded) = if path.exists() {
        (path.to_path_buf(), true)
    } else {
        (path.with_file_name(LEGACY_HISTORY_FILE_NAME), false)
    };
    let file = match fs::File::open(source) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(if encoded {
            // Keep a malformed record as text rather than losing it on
            // the next append/rewrite.
            serde_json::from_str::<String>(&line).unwrap_or(line)
        } else {
            line
        });
    }
    Ok(entries)
}

/// Append an entry to the persisted history, pruning old entries to
/// stay within [`MAX_HISTORY_ENTRIES`]. Prompts and slash commands are kept;
/// empty input is skipped.
///
/// Best-effort and non-blocking — work is forwarded to a dedicated writer
/// thread so the caller (typically the UI submit handler) returns
/// immediately. See module docs for the rationale (#1927). Failures on
/// the writer thread are logged via `tracing` but not propagated.
pub fn append_history(entry: &str) {
    let Some(path) = default_history_path() else {
        return;
    };
    append_history_dispatched(&path, entry);
}

/// Path-injectable variant of [`append_history`] used by tests. Forwards
/// the work to the dedicated writer thread (or falls back to a synchronous
/// write if the channel send fails) so callers never block on disk I/O.
fn append_history_dispatched(path: &Path, entry: &str) {
    let entry = entry.to_string();
    if let Err(err) = writer_sender().send(HistoryWrite::Append(path.to_path_buf(), entry)) {
        match err.0 {
            HistoryWrite::Append(path, entry) => append_history_to(&path, &entry),
            #[cfg(test)]
            HistoryWrite::Flush(_) => unreachable!("flush messages are only sent by tests"),
        }
    }
}

enum HistoryWrite {
    Append(PathBuf, String),
    #[cfg(test)]
    Flush(Sender<()>),
}

/// Lazy singleton sender for the dedicated composer-history writer
/// thread. Initialised on first use; the thread runs for the lifetime
/// of the process and drains queued writes in arrival order.
fn writer_sender() -> &'static Sender<HistoryWrite> {
    static SENDER: OnceLock<Sender<HistoryWrite>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = channel::<HistoryWrite>();
        let spawn_result = std::thread::Builder::new()
            .name("composer-history-writer".to_string())
            .spawn(move || {
                // recv() returns Err when all senders have dropped, which
                // only happens at process shutdown because the singleton
                // sender lives in a static for the lifetime of the process.
                while let Ok(message) = rx.recv() {
                    match message {
                        HistoryWrite::Append(path, entry) => {
                            append_history_batch(&rx, (path, entry));
                        }
                        #[cfg(test)]
                        HistoryWrite::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        if let Err(err) = spawn_result {
            tracing::warn!("Failed to spawn composer-history-writer: {err}");
        }
        tx
    })
}

fn append_history_batch(rx: &Receiver<HistoryWrite>, first: (PathBuf, String)) {
    let mut pending = vec![first];
    #[cfg(test)]
    let mut flush = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(HistoryWrite::Append(path, entry)) => pending.push((path, entry)),
            #[cfg(test)]
            Ok(HistoryWrite::Flush(done)) => {
                flush = Some(done);
                break;
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    for (path, entries) in group_history_writes_by_path(pending) {
        append_history_entries_to(&path, entries.iter().map(String::as_str));
    }

    #[cfg(test)]
    if let Some(done) = flush {
        let _ = done.send(());
    }
}

fn group_history_writes_by_path(writes: Vec<(PathBuf, String)>) -> Vec<(PathBuf, Vec<String>)> {
    let mut grouped: Vec<(PathBuf, Vec<String>)> = Vec::new();

    for (path, entry) in writes {
        if let Some((_, entries)) = grouped
            .iter_mut()
            .find(|(existing_path, _)| existing_path == &path)
        {
            entries.push(entry);
        } else {
            grouped.push((path, vec![entry]));
        }
    }

    grouped
}

fn append_history_to(path: &Path, entry: &str) {
    append_history_entries_to(path, std::iter::once(entry));
}

/// Keep immediate recall and persisted history on the same duplicate rule.
/// Callers choose whether to preserve the submitted whitespace in their copy.
pub(crate) fn push_history_entry(entries: &mut Vec<String>, entry: &str) -> bool {
    let trimmed = entry.trim();
    if trimmed.is_empty() || entries.last().is_some_and(|last| last.trim() == trimmed) {
        return false;
    }
    entries.push(entry.to_string());
    true
}

fn append_history_entries_to<'a>(
    path: &Path,
    entries_to_append: impl IntoIterator<Item = &'a str>,
) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        tracing::warn!(
            "Failed to create composer history dir {}: {err}",
            parent.display()
        );
        return;
    }

    // Read existing entries, append the new ones, prune from the front
    // until under the cap, then atomically rewrite.
    let mut entries = match read_history_from(path) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "Not rewriting composer history at {}: reading it failed: {err}",
                path.display()
            );
            return;
        }
    };
    let mut changed = false;
    for entry in entries_to_append {
        changed |= push_history_entry(&mut entries, entry.trim());
    }

    if !changed {
        return;
    }

    if entries.len() > MAX_HISTORY_ENTRIES {
        let excess = entries.len() - MAX_HISTORY_ENTRIES;
        entries.drain(0..excess);
    }

    let payload = entries
        .iter()
        .map(|entry| serde_json::to_string(entry).expect("serializing a string cannot fail"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    if let Err(err) = write_history_atomic(path, payload.as_bytes()) {
        tracing::warn!(
            "Failed to persist composer history at {}: {err}",
            path.display()
        );
    }
}

fn write_history_atomic(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    const RETRY_DELAYS: &[Duration] = &[
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(25),
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(400),
    ];

    for (attempt, delay) in RETRY_DELAYS
        .iter()
        .map(Some)
        .chain(std::iter::once(None))
        .enumerate()
    {
        match codewhale_util::utils::write_atomic(path, payload) {
            Ok(()) => return Ok(()),
            Err(err) if delay.is_some() => {
                tracing::debug!(
                    "Retrying composer history write to {} after attempt {} failed: {err}",
                    path.display(),
                    attempt + 1
                );
                std::thread::sleep(*delay.expect("delay checked"));
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("retry iterator always ends with a final write attempt")
}

#[cfg(test)]
pub(crate) fn flush_history_writer_for_tests(timeout: Duration) {
    let (done_tx, done_rx) = channel();
    writer_sender()
        .send(HistoryWrite::Flush(done_tx))
        .expect("history writer accepts flush");
    done_rx
        .recv_timeout(timeout)
        .expect("history writer flush timed out");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Tests use the path-injecting `*_from` / `*_to` helpers so they
    /// don't have to mutate `HOME` (which is not honored by
    /// `codewhale_paths::user_home()` on Windows — it reads `USERPROFILE` /
    /// `SHGetKnownFolderPath` instead). This makes the suite portable
    /// across all three CI runners without per-platform env juggling.
    fn temp_history_path() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(HISTORY_FILE_NAME);
        (tmp, path)
    }

    // #3240: a fresh install must resolve the history file under `.codewhale`,
    // never the legacy `.deepseek` dir, so normal use doesn't recreate it.
    #[test]
    fn fresh_install_uses_codewhale_not_legacy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = history_path_with_home(Some(tmp.path().to_path_buf()))
            .expect("path resolves with a home dir");
        assert_eq!(path, tmp.path().join(".codewhale").join(HISTORY_FILE_NAME));
        assert!(
            !path.starts_with(tmp.path().join(".deepseek")),
            "fresh install must not target the legacy .deepseek dir: {path:?}"
        );
    }

    // Migration care: an existing legacy text history keeps its root and
    // survives the first JSONL write byte-for-byte.
    #[test]
    fn existing_legacy_history_is_still_used() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join(".deepseek").join(LEGACY_HISTORY_FILE_NAME);
        fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("mkdir legacy");
        fs::write(&legacy, "old entry\n").expect("seed legacy history");
        let path = history_path_with_home(Some(tmp.path().to_path_buf())).expect("path resolves");
        assert_eq!(path, legacy.with_file_name(HISTORY_FILE_NAME));
        assert_eq!(load_history_from(&path), ["old entry"]);
        append_history_to(&path, "new\nentry");
        assert_eq!(load_history_from(&path), ["old entry", "new\nentry"]);
        assert_eq!(fs::read_to_string(&legacy).unwrap(), "old entry\n");
    }

    // Once a `.codewhale` history exists it wins over either legacy format.
    #[test]
    fn codewhale_history_preferred_over_legacy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join(".codewhale").join(LEGACY_HISTORY_FILE_NAME);
        let legacy = tmp.path().join(".deepseek").join(HISTORY_FILE_NAME);
        for p in [&primary, &legacy] {
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(p, "x\n").expect("seed");
        }
        let path = history_path_with_home(Some(tmp.path().to_path_buf())).expect("path resolves");
        assert_eq!(path, primary.with_file_name(HISTORY_FILE_NAME));
    }

    #[test]
    fn append_and_load_round_trip() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "first");
        append_history_to(&path, "second");
        append_history_to(&path, "third");
        assert_eq!(load_history_from(&path), vec!["first", "second", "third"]);
    }

    #[test]
    fn slash_commands_and_absolute_paths_stored() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "/help");
        append_history_to(&path, "real prompt");
        append_history_to(&path, "/cost");
        append_history_to(&path, "cat /etc/fstab");
        assert_eq!(
            load_history_from(&path),
            vec!["/help", "real prompt", "/cost", "cat /etc/fstab"]
        );
    }

    #[test]
    fn multi_line_entries_round_trip_as_one_entry() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "first");
        append_history_to(&path, "fn main() {\n    run();\n}");
        append_history_to(&path, "\"quoted\" start");
        append_history_to(&path, "last");
        assert_eq!(
            load_history_from(&path),
            vec![
                "first",
                "fn main() {\n    run();\n}",
                "\"quoted\" start",
                "last"
            ]
        );
        // The same multi-line prompt twice is still one consecutive duplicate.
        append_history_to(&path, "a\nb");
        append_history_to(&path, "a\nb");
        assert_eq!(load_history_from(&path).len(), 5);
    }

    #[test]
    fn legacy_plain_quoted_lines_keep_their_quotes() {
        let (_tmp, path) = temp_history_path();
        // Older writers trimmed entries and stored every remaining byte raw.
        // JSON-looking strings are user text, even when decoding and encoding
        // them would round-trip: the escapes must not turn into control chars.
        let legacy = [r#""yes""#, r#""a" and "b""#, r#""a\nb""#, r#""\"quoted\"""#];
        let legacy_path = path.with_file_name(LEGACY_HISTORY_FILE_NAME);
        let original = legacy.join("\n") + "\n";
        fs::write(&legacy_path, &original).expect("seed legacy file");
        assert_eq!(load_history_from(&path), legacy);
        // Rewriting on the next append must preserve every legacy prompt.
        append_history_to(&path, "next\nline");
        let expected: Vec<String> = legacy
            .into_iter()
            .chain(["next\nline"])
            .map(str::to_string)
            .collect();
        assert_eq!(load_history_from(&path), expected);
        assert_eq!(fs::read_to_string(&legacy_path).unwrap(), original);
    }

    #[test]
    fn malformed_json_records_stay_literal() {
        let (_tmp, path) = temp_history_path();
        fs::write(&path, "malformed record").expect("seed malformed record");
        assert_eq!(load_history_from(&path), ["malformed record"]);
        append_history_to(&path, "next");
        assert_eq!(load_history_from(&path), ["malformed record", "next"]);
    }

    #[test]
    fn consecutive_duplicate_commands_deduped() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "/theme");
        append_history_to(&path, "/theme");
        append_history_to(&path, "/theme");
        assert_eq!(load_history_from(&path), vec!["/theme"]);
    }

    #[test]
    fn empty_and_whitespace_skipped() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "");
        append_history_to(&path, "   ");
        append_history_to(&path, "\n\t");
        append_history_to(&path, "real");
        assert_eq!(load_history_from(&path), vec!["real"]);
    }

    #[test]
    fn consecutive_duplicates_deduped() {
        let (_tmp, path) = temp_history_path();
        append_history_to(&path, "same");
        append_history_to(&path, "same");
        append_history_to(&path, "same");
        append_history_to(&path, "different");
        append_history_to(&path, "same");
        assert_eq!(load_history_from(&path), vec!["same", "different", "same"]);
    }

    #[test]
    fn pruned_to_cap_at_append_time() {
        let (_tmp, path) = temp_history_path();
        // One batched rewrite — a per-entry loop would fsync 1000+ times.
        let entries: Vec<String> = (0..(MAX_HISTORY_ENTRIES + 50))
            .map(|i| format!("entry {i}"))
            .collect();
        append_history_entries_to(&path, entries.iter().map(String::as_str));
        let history = load_history_from(&path);
        assert_eq!(history.len(), MAX_HISTORY_ENTRIES);
        // Newest entries survive; oldest 50 were pruned.
        assert_eq!(history.first().map(String::as_str), Some("entry 50"));
        assert_eq!(
            history.last().map(String::as_str),
            Some(format!("entry {}", MAX_HISTORY_ENTRIES + 49)).as_deref()
        );

        // Keep a cheap boundary check on the singleton production wrapper:
        // seed to one below the cap in one write, then cross it with only two
        // fsyncing appends. The second append must prune exactly the oldest
        // entry rather than only enforcing the cap for batched callers.
        let (_boundary_tmp, boundary_path) = temp_history_path();
        let seeded: Vec<String> = (0..(MAX_HISTORY_ENTRIES - 1))
            .map(|i| format!("entry {i}"))
            .collect();
        append_history_entries_to(&boundary_path, seeded.iter().map(String::as_str));
        append_history_to(
            &boundary_path,
            &format!("entry {}", MAX_HISTORY_ENTRIES - 1),
        );
        append_history_to(&boundary_path, &format!("entry {MAX_HISTORY_ENTRIES}"));
        let boundary = load_history_from(&boundary_path);
        assert_eq!(boundary.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(boundary.first().map(String::as_str), Some("entry 1"));
        assert_eq!(
            boundary.last().map(String::as_str),
            Some(format!("entry {MAX_HISTORY_ENTRIES}")).as_deref()
        );
    }

    #[test]
    fn missing_file_loads_empty() {
        let (_tmp, path) = temp_history_path();
        assert!(load_history_from(&path).is_empty());
    }

    /// Regression for #1927 — the dispatched append path must return
    /// promptly even when a synchronous write of the seeded file would
    /// be slow. We pre-populate the file with ~1000 entries (the cap)
    /// so a sync read-modify-write would take real disk time on any
    /// platform, then call `append_history_dispatched` many times and
    /// assert that the cumulative wall-clock cost stays well below the
    /// stall the user reports.
    #[test]
    fn append_history_dispatched_does_not_block_the_caller() {
        let (_tmp, path) = temp_history_path();
        // Seed close to the cap so a synchronous rewrite is non-trivial.
        let seed = (0..(MAX_HISTORY_ENTRIES - 50))
            .map(|i| format!("seed entry {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, seed).expect("seed history");

        let start = Instant::now();
        for i in 0..50 {
            append_history_dispatched(&path, &format!("new entry {i}"));
        }
        let dispatch_elapsed = start.elapsed();

        // 50 sync read-modify-write cycles on a ~200KB file would be
        // measurable (tens of ms even on a fast SSD). The dispatch path
        // hands work to the writer thread and returns; the whole loop
        // should finish in single-digit ms. Pick a generous CI-safe
        // bound that still catches a regression to the old sync path.
        assert!(
            dispatch_elapsed < Duration::from_millis(150),
            "append_history dispatch was too slow: {dispatch_elapsed:?} \
             (likely re-introduced #1927: caller blocked on disk write)"
        );

        flush_history_writer_for_tests(Duration::from_secs(if cfg!(windows) { 10 } else { 5 }));

        let loaded = load_history_from(&path);
        assert!(
            loaded.iter().any(|line| line == "new entry 49"),
            "writer thread did not persist the dispatched entries; \
             loaded {} entries, last = {:?}",
            loaded.len(),
            loaded.last()
        );
        assert!(loaded.iter().any(|line| line == "new entry 0"));
    }
}

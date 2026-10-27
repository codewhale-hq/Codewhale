//! Parked-draft stash for the composer (#440).
//!
//! A stash is a side-channel from history: it holds drafts the user
//! parked deliberately (Ctrl+G or Ctrl+S) instead of submissions made in the
//! past (which live in `composer_history.rs`). Pop semantics make it
//! a LIFO — the most recent stash comes back first.
//!
//! ## On-disk format
//!
//! `~/.codewhale/composer_stash.jsonl` — one JSON object per line:
//!
//! ```jsonl
//! {"ts":"2026-05-04T01:23:45Z","text":"draft here"}
//! ```
//!
//! Self-healing parser: malformed lines are skipped silently so a
//! single bad write doesn't corrupt the rest of the stash. The
//! parser doesn't require any specific field order; only `text` is
//! mandatory.
//!
//! ## Why JSONL and not a plain text file?
//!
//! Drafts can contain newlines (they're prompts, not single-line
//! commands), so a `\n`-delimited plain file would mangle multi-line
//! drafts. JSONL escapes newlines inside JSON strings without
//! ambiguity and the timestamp / future fields land cleanly.

use std::fs;
use std::io;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STASH_FILE_NAME: &str = "composer_stash.jsonl";

/// Read-only stash facts for diagnostic output.
///
/// Unlike the ordinary composer helpers, this report never creates a state
/// directory or falls back outside an explicit `CODEWHALE_HOME` boundary. It
/// rejects a stash-file symlink observed during inspection; Unix opens also
/// use `O_NOFOLLOW` for the final leaf open.
#[derive(Debug, Clone)]
pub(crate) struct DiagnosticStashReport {
    /// Candidate stash path, when the Codewhale home could be resolved.
    pub(crate) path: Option<PathBuf>,
    /// Whether a regular stash file was present at that path.
    pub(crate) present: bool,
    /// Number of valid, non-empty draft records observed without mutation.
    pub(crate) count: usize,
    /// A safe path-shape or read error, if inspection could not complete.
    pub(crate) error: Option<String>,
}

/// Hard cap so a runaway script can't fill the user's home with
/// parked drafts. Older entries are pruned at push time when the
/// stash exceeds this count.
pub const MAX_STASH_ENTRIES: usize = 200;

/// One parked draft. Fields are `#[serde(default)]` so legacy /
/// truncated records still parse instead of poisoning the stash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StashedDraft {
    /// RFC 3339 timestamp; omitted on legacy records.
    #[serde(default)]
    pub ts: String,
    /// The parked text. Required — entries with no `text` are
    /// dropped during load (treated as malformed).
    pub text: String,
}

fn default_stash_path() -> Option<PathBuf> {
    codewhale_paths::user_home().map(|home| {
        let primary = home.join(".codewhale").join(STASH_FILE_NAME);
        let legacy = home.join(".deepseek").join(STASH_FILE_NAME);
        if primary.exists() || !legacy.exists() {
            return primary;
        }
        legacy
    })
}

/// Inspect the composer stash for `doctor` without changing product state.
///
/// Ordinary composer reads retain their historical legacy fallback behavior.
/// Diagnostics follow the same behavior only when no explicit
/// `CODEWHALE_HOME` is configured; an explicit home is an isolation boundary
/// and must not cause doctor to inspect an ambient `$HOME/.codewhale` or
/// `$HOME/.deepseek` stash.
pub(crate) fn diagnostic_stash_report() -> DiagnosticStashReport {
    let primary = match codewhale_config::codewhale_home() {
        Ok(home) => home.join(STASH_FILE_NAME),
        Err(error) => {
            return DiagnosticStashReport {
                path: None,
                present: false,
                count: 0,
                error: Some(format!(
                    "could not resolve the Codewhale stash path: {error}"
                )),
            };
        }
    };

    let explicit_home = codewhale_config::codewhale_home_is_explicit();
    let legacy = if explicit_home {
        None
    } else {
        match codewhale_config::legacy_deepseek_home() {
            Ok(home) => Some(home.join(STASH_FILE_NAME)),
            Err(error) => {
                return DiagnosticStashReport {
                    path: Some(primary),
                    present: false,
                    count: 0,
                    error: Some(format!(
                        "could not resolve the legacy composer stash path: {error}"
                    )),
                };
            }
        }
    };

    diagnostic_stash_report_from_paths(primary, legacy, explicit_home)
}

fn diagnostic_stash_report_from_paths(
    primary: PathBuf,
    legacy: Option<PathBuf>,
    explicit_home: bool,
) -> DiagnosticStashReport {
    let path = match std::fs::symlink_metadata(&primary) {
        Ok(_) => primary,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicit_home => {
            let Some(legacy) = legacy else {
                return diagnostic_stash_report_at(primary);
            };
            match std::fs::symlink_metadata(&legacy) {
                Ok(_) => legacy,
                Err(error) if error.kind() == io::ErrorKind::NotFound => primary,
                Err(error) => {
                    return DiagnosticStashReport {
                        path: Some(legacy),
                        present: false,
                        count: 0,
                        error: Some(format!(
                            "could not inspect legacy composer stash metadata: {error}"
                        )),
                    };
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => primary,
        Err(error) => {
            return DiagnosticStashReport {
                path: Some(primary),
                present: false,
                count: 0,
                error: Some(format!(
                    "could not inspect composer stash metadata: {error}"
                )),
            };
        }
    };
    diagnostic_stash_report_at(path)
}

fn diagnostic_stash_report_at(path: PathBuf) -> DiagnosticStashReport {
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return DiagnosticStashReport {
                path: Some(path),
                present: false,
                count: 0,
                error: None,
            };
        }
        Err(error) => {
            return DiagnosticStashReport {
                path: Some(path),
                present: false,
                count: 0,
                error: Some(format!(
                    "could not inspect composer stash metadata: {error}"
                )),
            };
        }
    };
    if metadata.file_type().is_symlink() {
        return DiagnosticStashReport {
            path: Some(path),
            present: false,
            count: 0,
            error: Some("composer stash path is a symlink; doctor did not follow it".to_string()),
        };
    }
    if !metadata.file_type().is_file() {
        return DiagnosticStashReport {
            path: Some(path),
            present: false,
            count: 0,
            error: Some("composer stash path is not a regular file".to_string()),
        };
    }

    match load_stash_for_diagnostic(&path) {
        Ok(entries) => DiagnosticStashReport {
            path: Some(path),
            present: true,
            count: entries.len(),
            error: None,
        },
        Err(error) => DiagnosticStashReport {
            path: Some(path),
            present: false,
            count: 0,
            error: Some(error),
        },
    }
}

fn load_stash_for_diagnostic(path: &Path) -> Result<Vec<StashedDraft>, String> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = fs::File::open(path);
    let file = file.map_err(|error| format!("could not open composer stash read-only: {error}"))?;

    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("could not read composer stash: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(draft) = serde_json::from_str::<StashedDraft>(&line)
            && !draft.text.is_empty()
        {
            entries.push(draft);
        }
    }
    Ok(entries)
}

/// Load every stashed draft from disk in the order they were
/// written (oldest first). Self-healing: malformed lines are
/// dropped silently. Returns an empty vec when the file doesn't
/// exist.
#[must_use]
pub fn load_stash() -> Vec<StashedDraft> {
    let Some(path) = default_stash_path() else {
        return Vec::new();
    };
    load_stash_from(&path)
}

fn load_stash_from(path: &Path) -> Vec<StashedDraft> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<StashedDraft>(&line).ok())
        .filter(|draft| !draft.text.is_empty())
        .collect()
}

/// Push a new draft onto the stash. Empty / whitespace-only text
/// is silently dropped so a stray stash shortcut on an empty composer
/// doesn't pollute the file. Failures are logged but never
/// propagated — stash is a UX nicety, not a correctness concern.
pub fn push_stash(text: &str) {
    let Some(path) = default_stash_path() else {
        return;
    };
    push_stash_to(&path, text);
}

fn push_stash_to(path: &Path, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        tracing::warn!(
            "Failed to create composer stash dir {}: {err}",
            parent.display()
        );
        return;
    }

    let mut entries = load_stash_from(path);
    entries.push(StashedDraft {
        ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        text: text.to_string(),
    });
    if entries.len() > MAX_STASH_ENTRIES {
        let excess = entries.len() - MAX_STASH_ENTRIES;
        entries.drain(0..excess);
    }
    write_stash_to(path, &entries);
}

/// Remove and return the most recently pushed draft, if any.
/// Rewrites the on-disk file with the remaining entries.
#[must_use]
pub fn pop_stash() -> Option<StashedDraft> {
    let path = default_stash_path()?;
    pop_stash_from(&path)
}

/// Wipe the stash file entirely. Returns the number of entries
/// that were dropped (so the caller can report it). Returns 0
/// when the file doesn't exist or had no entries.
pub fn clear_stash() -> io::Result<usize> {
    let Some(path) = default_stash_path() else {
        return Ok(0);
    };
    clear_stash_at(&path)
}

fn clear_stash_at(path: &Path) -> io::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let count = load_stash_from(path).len();
    // Clear means the file is empty afterwards, not that no *valid* draft was
    // found: malformed-only lines are still stashed text on disk (U01-m3).
    if count == 0 && fs::metadata(path)?.len() == 0 {
        return Ok(0);
    }
    codewhale_util::utils::write_atomic(path, b"")?;
    Ok(count)
}

fn pop_stash_from(path: &Path) -> Option<StashedDraft> {
    let mut entries = load_stash_from(path);
    let popped = entries.pop()?;
    write_stash_to(path, &entries);
    Some(popped)
}

fn write_stash_to(path: &Path, entries: &[StashedDraft]) {
    let mut payload = String::new();
    for entry in entries {
        match serde_json::to_string(entry) {
            Ok(line) => {
                payload.push_str(&line);
                payload.push('\n');
            }
            Err(err) => {
                // A draft that round-trips through serde shouldn't
                // fail to serialize, but belt-and-suspenders so a
                // weird codepoint in `text` doesn't blow the file
                // away mid-write.
                tracing::warn!("Skipping stash entry due to serialize failure: {err}");
            }
        }
    }
    if let Err(err) = codewhale_util::utils::write_atomic(path, payload.as_bytes()) {
        tracing::warn!(
            "Failed to persist composer stash at {}: {err}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_stash_path() -> (TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("composer_stash.jsonl");
        (tmp, path)
    }

    #[test]
    fn push_and_load_round_trip() {
        let (_tmp, path) = temp_stash_path();
        push_stash_to(&path, "first draft");
        push_stash_to(&path, "second draft");
        let entries = load_stash_from(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "first draft");
        assert_eq!(entries[1].text, "second draft");
        assert!(!entries[1].ts.is_empty(), "timestamp stamped on push");
    }

    #[test]
    fn pop_returns_lifo_and_rewrites_file() {
        let (_tmp, path) = temp_stash_path();
        push_stash_to(&path, "first");
        push_stash_to(&path, "second");
        let popped = pop_stash_from(&path).expect("non-empty stash");
        assert_eq!(popped.text, "second");
        let remaining = load_stash_from(&path);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].text, "first");
    }

    #[test]
    fn pop_on_empty_stash_returns_none() {
        let (_tmp, path) = temp_stash_path();
        assert!(pop_stash_from(&path).is_none());
    }

    #[test]
    fn empty_text_is_dropped() {
        let (_tmp, path) = temp_stash_path();
        push_stash_to(&path, "");
        push_stash_to(&path, "   \n  ");
        assert!(load_stash_from(&path).is_empty());
    }

    #[test]
    fn multiline_drafts_are_preserved_intact() {
        let (_tmp, path) = temp_stash_path();
        let multiline = "first line\nsecond line\n  third line";
        push_stash_to(&path, multiline);
        let entries = load_stash_from(&path);
        assert_eq!(entries.len(), 1);
        // Multi-line text round-trips because JSON escapes the newlines.
        assert_eq!(entries[0].text, multiline);
    }

    #[test]
    fn malformed_lines_are_skipped_and_valid_lines_survive() {
        let (_tmp, path) = temp_stash_path();
        // Mix of valid JSON, garbage, and partial-write truncation.
        let raw = "\
{\"ts\":\"2026-05-04T01:23:45Z\",\"text\":\"good one\"}
this is not json
{\"text\":\"good two\"}
{\"ts\":\"2026-05-04T01:24:00Z\"
{\"text\":\"\"}
{}
";
        std::fs::write(&path, raw).unwrap();
        let entries = load_stash_from(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "good one");
        assert_eq!(entries[1].text, "good two");
    }

    #[test]
    fn clear_returns_zero_when_file_is_absent() {
        let (_tmp, path) = temp_stash_path();
        // Path doesn't exist yet.
        assert_eq!(clear_stash_at(&path).unwrap(), 0);
    }

    #[test]
    fn clear_returns_zero_when_file_is_empty() {
        let (_tmp, path) = temp_stash_path();
        std::fs::write(&path, "").unwrap();
        assert_eq!(clear_stash_at(&path).unwrap(), 0);
    }

    /// U01-m3: a file holding only unparseable lines still holds stashed
    /// text; `/stash clear` must leave it empty, not report success over it.
    #[test]
    fn clear_empties_a_file_of_malformed_lines() {
        let (_tmp, path) = temp_stash_path();
        std::fs::write(&path, "not json\n{\"ts\":1}\n").unwrap();
        assert_eq!(clear_stash_at(&path).unwrap(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn clear_drops_entries_and_reports_count() {
        let (_tmp, path) = temp_stash_path();
        push_stash_to(&path, "first");
        push_stash_to(&path, "second");
        push_stash_to(&path, "third");
        let dropped = clear_stash_at(&path).expect("clear succeeds");
        assert_eq!(dropped, 3);
        // File still exists but is empty so subsequent loads come back clean.
        assert!(load_stash_from(&path).is_empty());
    }

    #[test]
    fn cap_prunes_oldest_at_push_time() {
        let (_tmp, path) = temp_stash_path();
        for i in 0..(MAX_STASH_ENTRIES + 5) {
            push_stash_to(&path, &format!("draft {i}"));
        }
        let entries = load_stash_from(&path);
        assert_eq!(entries.len(), MAX_STASH_ENTRIES);
        // Oldest survivors are `5..` because the first 5 were pruned.
        assert_eq!(entries[0].text, "draft 5");
        assert_eq!(
            entries[entries.len() - 1].text,
            format!("draft {}", MAX_STASH_ENTRIES + 5 - 1)
        );
    }

    #[test]
    fn diagnostic_stash_honors_an_explicit_home_without_legacy_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let primary = tmp.path().join("isolated-codewhale").join(STASH_FILE_NAME);
        let legacy = tmp.path().join("ambient-deepseek").join(STASH_FILE_NAME);
        std::fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy parent");
        std::fs::write(&legacy, r#"{"text":"ambient draft"}"#).expect("legacy stash");

        let report = diagnostic_stash_report_from_paths(primary.clone(), Some(legacy), true);

        assert_eq!(report.path.as_deref(), Some(primary.as_path()));
        assert!(!report.present);
        assert_eq!(report.count, 0);
        assert!(report.error.is_none());
        assert!(
            !primary.parent().expect("primary parent").exists(),
            "diagnostic lookup must not create an explicit state home"
        );
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_stash_rejects_a_symlink_leaf_without_following_it() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let external = tmp.path().join("external-stash.jsonl");
        let primary = tmp.path().join("composer_stash.jsonl");
        std::fs::write(&external, r#"{"text":"external draft"}"#).expect("external stash");
        symlink(&external, &primary).expect("symlink stash");

        let report = diagnostic_stash_report_from_paths(primary.clone(), None, true);

        assert_eq!(report.path.as_deref(), Some(primary.as_path()));
        assert!(!report.present);
        assert_eq!(report.count, 0);
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|error| error.contains("symlink"))
        );
    }
}

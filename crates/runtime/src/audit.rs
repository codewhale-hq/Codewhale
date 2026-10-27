//! Lightweight audit logging for sensitive operations.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{Value, json};

use crate::private_log::{flush_and_sync, open_append};

/// Append an audit event to `$CODEWHALE_HOME/audit.log` (or the default
/// `~/.codewhale/audit.log` when no explicit CodeWhale home is configured).
///
/// This helper is best-effort by design: callers should not fail critical flows
/// if audit persistence fails.
pub fn log_sensitive_event(event: &str, details: Value) {
    let result = default_audit_path().and_then(|path| append_event(&path, event, details));
    if let Err(err) = result {
        crate::logging::warn(format!("audit log write failed: {err}"));
    }
}

/// Append an audit event to `<codewhale_home>/audit.log` for a caller that
/// already owns an explicit Codewhale home (for example the DSH integration's
/// `DshPaths`), so its audit record lands beside the rest of its state rather
/// than in whatever home the process environment names (#6534).
pub fn log_sensitive_event_in(codewhale_home: &Path, event: &str, details: Value) {
    if let Err(err) = append_event(&codewhale_home.join("audit.log"), event, details) {
        crate::logging::warn(format!("audit log write failed: {err}"));
    }
}

/// Size at which `audit.log` is rolled to `audit.log.1`.
///
/// The log was append-only with no bound at all: a real `~/.codewhale/audit.log`
/// had reached 2.6 MB and was still growing, with nothing in the product that
/// would ever shrink it. Unbounded growth in the user's config directory is not
/// a viable end state, and neither is silently discarding the record — so one
/// previous generation is kept, which bounds the pair at ~2× this value while
/// preserving well over a year of ordinary use.
const AUDIT_LOG_ROTATE_BYTES: u64 = 16 * 1024 * 1024;

/// Roll `audit.log` to `audit.log.1` once it passes [`AUDIT_LOG_ROTATE_BYTES`].
///
/// Exactly one previous generation is kept; the older `.1` is replaced. Rolling
/// is a rename, so no record is ever rewritten in place and an event is never
/// lost to a partially-copied file.
///
/// Best-effort by the same rule as the rest of this module: if the roll fails
/// the event is still appended to the existing file. A too-large audit log is a
/// far better outcome than a dropped audit record.
fn rotate_if_oversized(path: &std::path::Path) {
    let oversized = fs::metadata(path).is_ok_and(|meta| meta.len() >= AUDIT_LOG_ROTATE_BYTES);
    if !oversized {
        return;
    }
    let mut rolled = path.as_os_str().to_owned();
    rolled.push(".1");
    let _ = fs::rename(path, std::path::Path::new(&rolled));
}

fn append_event(path: &Path, event: &str, details: Value) -> anyhow::Result<()> {
    let path = path.to_path_buf();
    let parent = path.parent().map(|p| p.to_path_buf());
    if let Some(ref parent) = parent {
        fs::create_dir_all(parent)?;
    }
    rotate_if_oversized(&path);
    // Open for append with a BufWriter for buffered I/O, then flush + fsync
    // after each event so the record is durably on disk.
    let mut writer = open_append(&path)?;
    let record = json!({
        "ts": Utc::now().to_rfc3339(),
        "event": event,
        "details": details,
    });
    let line = serde_json::to_string(&record)?;
    use std::io::Write;
    writeln!(writer, "{line}")?;
    flush_and_sync(&mut writer)?;
    Ok(())
}

fn default_audit_path() -> anyhow::Result<PathBuf> {
    // A test that has not sealed its own home must never append to the
    // developer's real ~/.codewhale/audit.log (#6534); it gets a per-process
    // scratch log instead. An *ambient* CODEWHALE_HOME is not a seal: it names
    // somebody's real profile just as `~/.codewhale` does.
    #[cfg(any(test, feature = "test-support"))]
    if !crate::test_support::home_is_sealed() {
        return Ok(std::env::temp_dir()
            .join(format!("codewhale-test-audit-{}", std::process::id()))
            .join("audit.log"));
    }
    Ok(codewhale_config::codewhale_home()?.join("audit.log"))
}

/// Where audit events are written, for surfaces that point a person at the
/// full record (for example `/permissions`). `None` when no Codewhale home
/// resolves; callers show a placeholder rather than guessing a path.
#[must_use]
pub fn audit_log_path() -> Option<PathBuf> {
    default_audit_path().ok()
}

#[cfg(test)]
mod tests {
    use super::{AUDIT_LOG_ROTATE_BYTES, rotate_if_oversized};

    /// #6534 guard: with no explicit CODEWHALE_HOME, a test process resolves
    /// the audit log outside the real user home, so no test can append to it.
    #[test]
    fn test_process_never_resolves_the_real_home_audit_log() {
        let _lock = crate::test_support::lock_test_env();
        let _home = crate::test_support::EnvVarGuard::remove("CODEWHALE_HOME");
        let path = super::audit_log_path().expect("audit path");
        let real = codewhale_paths::user_home()
            .expect("user home")
            .join(".codewhale")
            .join("audit.log");
        assert_ne!(path, real);
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
    }

    #[test]
    fn an_explicit_home_receives_its_own_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        super::log_sensitive_event_in(dir.path(), "test.event", serde_json::json!({"k": 1}));
        let log = std::fs::read_to_string(dir.path().join("audit.log")).expect("audit.log");
        assert!(log.contains("\"event\":\"test.event\""), "{log}");
    }

    #[test]
    fn a_small_log_is_left_alone() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("audit.log");
        std::fs::write(&path, b"{}\n").expect("write");
        rotate_if_oversized(&path);
        assert!(path.exists(), "an ordinary log is never rolled");
        assert!(!dir.path().join("audit.log.1").exists());
    }

    #[test]
    fn an_oversized_log_rolls_to_one_previous_generation() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("audit.log");
        let rolled = dir.path().join("audit.log.1");
        std::fs::write(&path, vec![b'x'; AUDIT_LOG_ROTATE_BYTES as usize]).expect("write");

        rotate_if_oversized(&path);

        assert!(
            !path.exists(),
            "the live log is rolled aside, not truncated"
        );
        assert_eq!(
            std::fs::metadata(&rolled).expect("rolled log").len(),
            AUDIT_LOG_ROTATE_BYTES,
            "the previous generation keeps every byte — rolling is a rename"
        );
    }

    #[test]
    fn rolling_twice_keeps_exactly_one_previous_generation() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("audit.log");
        let rolled = dir.path().join("audit.log.1");

        std::fs::write(&path, vec![b'a'; AUDIT_LOG_ROTATE_BYTES as usize]).expect("first");
        rotate_if_oversized(&path);
        std::fs::write(&path, vec![b'b'; AUDIT_LOG_ROTATE_BYTES as usize]).expect("second");
        rotate_if_oversized(&path);

        let kept = std::fs::read(&rolled).expect("rolled log");
        assert_eq!(kept.len(), AUDIT_LOG_ROTATE_BYTES as usize);
        assert_eq!(kept[0], b'b', "the newer generation replaces the older one");
        assert!(
            !dir.path().join("audit.log.2").exists(),
            "generations must not accumulate — that is the bug being fixed"
        );
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        rotate_if_oversized(&dir.path().join("audit.log"));
    }
}

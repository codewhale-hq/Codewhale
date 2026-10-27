//! Boot-time snapshot pruning.
//!
//! Called from `session_manager` once per session start. Failure is
//! never fatal — old snapshots taking disk space is annoying but not
//! correctness-breaking, so we log and move on.

use std::io;
use std::path::Path;
use std::time::Duration;

use super::paths::snapshot_git_dir;
use super::repo::SnapshotRepo;

/// Prune snapshots older than `max_age` for the given workspace.
///
/// If no snapshot repo exists yet (first run) this is a cheap no-op.
/// Returns the number of snapshots removed.
pub fn prune_older_than(workspace: &Path, max_age: Duration) -> io::Result<usize> {
    let git_dir = snapshot_git_dir(workspace)?;
    if !git_dir.exists() {
        return Ok(0);
    }
    let repo = SnapshotRepo::open_or_init(workspace)?;
    let removed = repo.prune_older_than(max_age)?;
    // `git prune --expire=now` walks the whole object store — seconds on a
    // large snapshot repo. Nothing removed means nothing newly unreachable,
    // so skip the walk entirely on the common boot path (#3757).
    if removed > 0 {
        repo.prune_unreachable_objects()?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::test_support::{EnvVarGuard, lock_test_env};
    use tempfile::tempdir;

    #[test]
    fn prune_no_repo_returns_zero() {
        let tmp = tempdir().unwrap();
        let _lock = lock_test_env();
        let _profile = EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("profile"));
        let removed =
            prune_older_than(tmp.path(), codewhale_config::DEFAULT_SNAPSHOT_MAX_AGE).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn prune_with_existing_repo_zero_age_clears_all() {
        let tmp = tempdir().unwrap();
        let _lock = lock_test_env();
        let _profile = EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join("profile"));
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("f.txt"), "x").unwrap();
        repo.snapshot("turn:0").unwrap();

        // Same-second flake guard: see `repo::tests`.
        std::thread::sleep(Duration::from_millis(1100));

        let removed = prune_older_than(&workspace, Duration::from_secs(0)).unwrap();
        assert!(removed >= 1);
    }
}

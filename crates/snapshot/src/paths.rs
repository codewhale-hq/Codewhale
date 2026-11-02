//! Path resolution for the per-workspace snapshot side-repos.
//!
//! Snapshots live under the shared resolved state directory (including an
//! explicit `CODEWHALE_HOME`, or the existing primary/legacy snapshot store) with
//! a two-level hash split so we can snapshot multiple worktrees of the
//! same project independently — `git worktree list` users won't get
//! cross-talk between feature branches.

use std::io;
use std::path::{Path, PathBuf};

/// Compute the snapshot directory for a given workspace path.
///
/// Returns `<snapshot state dir>/<project_hash>/<worktree_hash>/`, using
/// `codewhale_config::resolve_state_dir("snapshots")` for the shared base.
/// This resolves paths without creating directories or migrating state.
///
/// The `project_hash` is derived from the canonicalized workspace path
/// after stripping any `.worktrees/<name>` suffix — multiple worktrees
/// of the same repo share the same `project_hash` so users can browse
/// snapshots cross-worktree if they want, but the `worktree_hash` keeps
/// commits isolated by default.
pub fn snapshot_dir_for(workspace: &Path) -> io::Result<PathBuf> {
    // An explicit profile must never read or create ambient snapshots. The
    // shared resolver also preserves legacy stores and rejects invalid
    // overrides; do not silently fall back to the OS home or working directory.
    let base = snapshot_state_base()?;
    Ok(snapshot_dir_with_base(workspace, &base))
}

/// The shared snapshot store. An unsealed test gets a private one: a snapshot
/// repo opened by a fixture must not land in the developer's real store.
#[cfg(any(test, feature = "test-support"))]
fn snapshot_state_base() -> io::Result<PathBuf> {
    match codewhale_runtime::test_support::unsealed_state_dir("snapshots") {
        Some(base) => Ok(base),
        None => codewhale_config::resolve_state_dir("snapshots").map_err(io::Error::other),
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn snapshot_state_base() -> io::Result<PathBuf> {
    codewhale_config::resolve_state_dir("snapshots").map_err(io::Error::other)
}

fn snapshot_dir_with_base(workspace: &Path, base: &Path) -> PathBuf {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let project_root = strip_worktree_suffix(&canonical);
    let project_hash = stable_hex(&project_root);
    let worktree_hash = stable_hex(&canonical);
    base.join(project_hash).join(worktree_hash)
}

/// Resolve the `.git` directory inside the snapshot dir.
pub fn snapshot_git_dir(workspace: &Path) -> io::Result<PathBuf> {
    Ok(snapshot_dir_for(workspace)?.join(".git"))
}

/// Ensure the snapshot dir exists on disk and return its path.
pub fn ensure_snapshot_dir(workspace: &Path) -> io::Result<PathBuf> {
    let dir = snapshot_dir_for(workspace)?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub use codewhale_paths::{stable_hex, strip_worktree_suffix};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_dir_layout_keeps_two_hash_levels_under_selected_base() {
        let tmp = tempdir().expect("tempdir");
        let base = tmp.path().join("snapshots");
        let dir = snapshot_dir_with_base(tmp.path(), &base);
        let mut iter = dir.strip_prefix(&base).unwrap().components();
        assert!(iter.next().is_some()); // project_hash
        assert!(iter.next().is_some()); // worktree_hash
        assert!(iter.next().is_none());
    }

    #[test]
    fn worktree_suffix_stripped_for_project_hash() {
        let tmp = tempdir().expect("tempdir");
        let main_path = tmp.path().join("repo");
        let wt_path = tmp.path().join("repo").join(".worktrees").join("featX");
        std::fs::create_dir_all(&main_path).unwrap();
        std::fs::create_dir_all(&wt_path).unwrap();

        let base = tmp.path().join("snapshots");
        let main_dir = snapshot_dir_with_base(&main_path, &base);
        let wt_dir = snapshot_dir_with_base(&wt_path, &base);

        // Same project_hash (parent component before the worktree-specific tail).
        let main_components: Vec<_> = main_dir.components().collect();
        let wt_components: Vec<_> = wt_dir.components().collect();
        assert_eq!(
            main_components[main_components.len() - 2],
            wt_components[wt_components.len() - 2],
            "worktrees should share project_hash",
        );
        // But different worktree_hash (the tail).
        assert_ne!(main_components.last(), wt_components.last());
    }

    #[test]
    fn explicit_profile_owns_snapshot_creation_and_lookup() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let profile = tmp.path().join("selected-profile");
        let _profile =
            codewhale_runtime::test_support::EnvVarGuard::set("CODEWHALE_HOME", &profile);
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let expected = snapshot_dir_with_base(&workspace, &profile.join("snapshots"));
        let dir = ensure_snapshot_dir(&workspace).expect("create selected snapshot directory");
        assert_eq!(
            dir, expected,
            "snapshot writes must use the selected profile"
        );
        assert!(dir.exists());
        assert_eq!(
            snapshot_git_dir(&workspace).expect("lookup"),
            dir.join(".git"),
            "snapshot reads must use the same selected profile as writes"
        );
    }

    #[test]
    fn invalid_profile_is_an_error_without_ambient_fallback() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _profile =
            codewhale_runtime::test_support::EnvVarGuard::set("CODEWHALE_HOME", "relative-profile");
        assert!(snapshot_dir_for(tmp.path()).is_err());
        assert!(snapshot_git_dir(tmp.path()).is_err());
        assert!(ensure_snapshot_dir(tmp.path()).is_err());
        assert!(
            tmp.path()
                .read_dir()
                .expect("unchanged workspace")
                .next()
                .is_none()
        );
    }
}

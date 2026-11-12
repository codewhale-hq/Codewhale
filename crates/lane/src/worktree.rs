//! Worktree provisioning owned by Runtime (not Fleet) — #4176 / #4016.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::DateTime;

pub type GitCommandFn = fn(&Path) -> Result<Command>;

static GIT_COMMAND: OnceLock<GitCommandFn> = OnceLock::new();

pub fn set_git_command(constructor: GitCommandFn) {
    let _ = GIT_COMMAND.set(constructor);
}

fn git_command(dir: &Path) -> Result<Command> {
    #[cfg(test)]
    let constructor = GIT_COMMAND.get_or_init(|| tests::plain_git_command);
    #[cfg(not(test))]
    let constructor = GIT_COMMAND
        .get()
        .context("no git command was configured for lane worktrees")?;
    constructor(dir)
}

/// Spec for an isolated worktree + branch for a lane.
#[derive(Debug, Clone)]
pub struct WorktreeProvision {
    /// Git repository root (must contain `.git`).
    pub repo_root: PathBuf,
    /// Branch to create (from `base_ref`).
    pub branch: String,
    /// Directory for the new worktree (created by `git worktree add`).
    pub path: PathBuf,
    /// Base ref to branch from (default `HEAD`).
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProvisionedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Refuse `path` when an existing component between `root` and `path` is a
/// link. A path outside `root` (an explicit `--worktree-path`) is the caller's
/// own choice and is not checked.
pub fn reject_linked_below(root: &Path, path: &Path) -> Result<()> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing to create a worktree through the link {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

/// Create a git worktree + branch for a lane.
pub fn provision_worktree(spec: &WorktreeProvision) -> Result<ProvisionedWorktree> {
    if spec.branch.trim().is_empty() {
        bail!("worktree branch must not be empty");
    }
    if !spec.repo_root.exists() {
        bail!("repo root does not exist: {}", spec.repo_root.display());
    }
    let base = spec.base_ref.as_deref().unwrap_or("HEAD");
    // A leading '-' would be parsed as a `git worktree add` option.
    if spec.branch.starts_with('-') || base.starts_with('-') {
        bail!("worktree branch and base ref must not start with '-'");
    }
    if let Some(parent) = spec.path.parent() {
        // The default location is `<repo>/.codewhale/lanes/<id>`: a link on
        // the way down from the repository would send the checkout elsewhere.
        reject_linked_below(&spec.repo_root, parent)?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }
    // Capture git output instead of inheriting the caller's terminal. Runtime
    // callers include the raw-mode TUI launch screen, where even one inherited
    // progress/error line corrupts the alternate-screen buffer.
    let output = git_command(&spec.repo_root)?
        .args([
            "worktree",
            "add",
            "-b",
            &spec.branch,
            "--",
            &spec.path.to_string_lossy(),
            base,
        ])
        .output()
        .context("git worktree add")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git worktree add failed for branch {} at {}{}{}",
            spec.branch,
            spec.path.display(),
            if detail.is_empty() { "" } else { ": " },
            detail
        );
    }
    Ok(ProvisionedWorktree {
        path: spec.path.clone(),
        branch: spec.branch.clone(),
    })
}

/// Remove a worktree when TTL has expired (or immediately when TTL is 0).
///
/// `stopped_at` is RFC3339. When `ttl_secs` is `None`, no cleanup is performed.
///
/// Removal only ever touches a path that git identifies as a managed worktree
/// of its own repository (#5824); anything else — a stale or malformed record
/// pointing at an unrelated directory — is left untouched.
pub fn remove_worktree_if_expired(
    worktree_path: &Path,
    ttl_secs: Option<u64>,
    stopped_at: Option<&str>,
) -> Result<()> {
    let Some(ttl) = ttl_secs else {
        return Ok(());
    };
    if !worktree_path.exists() {
        return Ok(());
    }
    if ttl > 0 {
        let Some(stopped) = stopped_at else {
            return Ok(());
        };
        let stopped_ts = DateTime::parse_from_rfc3339(stopped)
            .with_context(|| format!("parse stopped_at {stopped}"))?
            .timestamp() as u64;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(stopped_ts) < ttl {
            return Ok(());
        }
    }

    // Ask the worktree what it is before deleting it: once the directory is
    // gone, neither its branch nor its repository is recoverable from the path.
    let Some(details) = worktree_details(worktree_path) else {
        return Ok(());
    };
    // #5824: a stale or malformed record must not turn TTL cleanup into an
    // unbounded recursive delete. Removal proceeds only for a path that git
    // itself identifies as a managed worktree of `details.repo_root`.
    if !is_managed_worktree(worktree_path, &details) {
        tracing::debug!(
            "skipped TTL cleanup of {}: git does not identify it as a managed worktree",
            worktree_path.display()
        );
        return Ok(());
    }

    // Best-effort: git worktree remove --force, then rm -rf.
    let removed = git_command(&details.repo_root)
        .and_then(|mut command| {
            Ok(command
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &worktree_path.to_string_lossy(),
                ])
                .status()?)
        })
        .is_ok_and(|status| status.success());
    if !removed && worktree_path.exists() && is_managed_worktree(worktree_path, &details) {
        // Re-verified immediately before the fallback: the directory may have
        // been swapped for an unrelated one between identification and removal.
        fs::remove_dir_all(worktree_path)
            .with_context(|| format!("remove worktree {}", worktree_path.display()))?;
    }
    if !removed {
        // The directory is gone but git still has it registered, and
        // `git worktree add` refuses a path it already knows about.
        if let Ok(mut command) = git_command(&details.repo_root) {
            let _ = command.args(["worktree", "prune"]).status();
        }
    }
    if let Some(branch) = details.branch.as_deref() {
        delete_lane_branch(&details.repo_root, branch);
    }
    Ok(())
}

/// What a lane worktree is: which repository owns it, which branch it has
/// checked out (`None` when detached), and every worktree that repository
/// lists.
struct WorktreeDetails {
    repo_root: PathBuf,
    branch: Option<String>,
    /// The worktrees the owning repository lists; a candidate path must
    /// resolve to one of these before cleanup may delete anything (#5824).
    worktrees: Vec<PathBuf>,
}

fn worktree_details(worktree_path: &Path) -> Option<WorktreeDetails> {
    let listing = git_command(worktree_path)
        .ok()?
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let worktrees: Vec<PathBuf> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect();
    // The main worktree is listed first, so its path is the repository root.
    let repo_root = worktrees.first().cloned()?;

    let branch = git_command(worktree_path)
        .ok()
        .and_then(|mut command| {
            command
                .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
                .output()
                .ok()
        })
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty());

    Some(WorktreeDetails {
        repo_root,
        branch,
        worktrees,
    })
}

/// Whether git identifies `worktree_path` itself as a managed worktree of the
/// repository in `details` (#5824). Two checks, both required:
///
/// - the candidate resolves to a worktree the owning repository lists, and
/// - the worktree is a *linked* one: its `.git` file names the registration
///   the owning repository keeps beneath `<repo>/.git/worktrees/`.
///
/// Repository roots (whose `.git` is a directory with no registration) and
/// plain subdirectories of a repo (which are not listed as worktrees) fail
/// here and are never candidates for recursive deletion. Paths are
/// canonicalized on both sides so symlinks (macOS `/tmp` -> `/private/tmp`)
/// and relative records cannot smuggle a different directory past the check.
fn is_managed_worktree(worktree_path: &Path, details: &WorktreeDetails) -> bool {
    let Ok(candidate) = fs::canonicalize(worktree_path) else {
        return false;
    };
    let listed = details
        .worktrees
        .iter()
        .any(|path| fs::canonicalize(path).is_ok_and(|resolved| resolved == candidate));
    if !listed {
        return false;
    }
    let Ok(repo_root) = fs::canonicalize(&details.repo_root) else {
        return false;
    };
    let registrations = repo_root.join(".git").join("worktrees");
    let registration = fs::read_to_string(worktree_path.join(".git"))
        .ok()
        .and_then(|dot_git| {
            dot_git
                .lines()
                .find_map(|line| line.strip_prefix("gitdir: "))
                .map(str::trim)
                .filter(|gitdir| !gitdir.is_empty())
                .map(PathBuf::from)
        });
    let Some(registration) = registration else {
        return false;
    };
    let registration = if registration.is_absolute() {
        registration
    } else {
        worktree_path.join(registration)
    };
    fs::canonicalize(registration).is_ok_and(|resolved| resolved.starts_with(registrations))
}

/// Delete the branch a removed lane worktree was on.
///
/// Lane branch names are derived from the user's launch name (`codex/{slug}`),
/// not from a UUID, so leaving the branch behind makes reusing that name fail
/// with "branch already exists" — a worktree directory that no longer exists
/// still blocking a legitimate lane.
///
/// This uses `branch -d`, not `-D`: a lane branch with nothing on it beyond
/// its base is merged and deletes cleanly, which is the case that was broken.
/// A branch carrying unmerged commits is someone's work, and a TTL timer is
/// not a mandate to throw it away — that one is kept, and the name stays taken
/// until a human decides otherwise.
fn delete_lane_branch(repo_root: &Path, branch: &str) {
    let output = git_command(repo_root)
        .and_then(|mut command| Ok(command.args(["branch", "-d", branch]).output()?));
    match output {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            tracing::debug!(
                "kept lane branch {branch} after worktree cleanup: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Err(err) => {
            tracing::debug!("could not delete lane branch {branch}: {err}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    pub(super) fn plain_git_command(dir: &Path) -> Result<Command> {
        let mut command = Command::new("git");
        command.current_dir(dir);
        Ok(command)
    }

    pub(crate) fn init_repo(root: &Path) {
        assert!(
            Command::new("git")
                .args(["init", "-b", "main"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "lane@test"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "lane"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("README"), "lane").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "README"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-m", "init"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn provision_and_ttl_zero_cleanup() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-lane");
        let provisioned = provision_worktree(&WorktreeProvision {
            repo_root: repo,
            branch: "codex/lane-test".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();
        assert!(provisioned.path.is_dir());
        assert!(wt_path.join("README").is_file());

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            !wt_path.exists(),
            "TTL 0 should remove worktree immediately"
        );
    }

    #[cfg(unix)]
    #[test]
    fn provision_refuses_a_linked_lanes_directory_under_the_repo() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".codewhale")).unwrap();
        init_repo(&repo);
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".codewhale").join("lanes")).unwrap();

        let error = provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/lane-linked".into(),
            path: repo.join(".codewhale").join("lanes").join("lane-1"),
            base_ref: Some("main".into()),
        })
        .expect_err("a linked lanes directory must be refused");
        assert!(format!("{error:#}").contains("link"), "{error:#}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn provision_refuses_option_shaped_base_ref() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("nested").join("wt-lane");
        let err = provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/lane-opt".into(),
            path: wt_path.clone(),
            base_ref: Some("--lock".into()),
        })
        .expect_err("option-shaped base ref must be refused");
        assert!(
            err.to_string().contains("must not start with '-'"),
            "{err:#}"
        );
        assert!(!wt_path.exists());
        assert!(!dir.path().join("nested").exists(), "nothing created");
        assert!(!branch_exists(&repo, "codex/lane-opt"));
        let list = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&list.stdout).contains("locked"));
    }

    #[test]
    fn provision_accepts_a_path_that_looks_like_an_option() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        // With `--` before the positionals, git reads this as a path.
        let provisioned = provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/lane-dash-path".into(),
            path: PathBuf::from("--detach"),
            base_ref: Some("main".into()),
        })
        .unwrap();
        assert!(repo.join("--detach").join("README").is_file());
        assert!(branch_exists(&repo, "codex/lane-dash-path"));
        drop(provisioned);
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "--verify", "--quiet", branch])
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn expired_cleanup_deletes_the_branch_so_the_lane_name_is_reusable() {
        // #4731: cleanup removed the worktree directory but left the branch.
        // Lane branches are named from the user's launch name, so reusing that
        // name then failed with "branch already exists" — pointing at a
        // worktree that no longer existed.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_path = dir.path().join("wt-lane");
        let spec = WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/reused-name".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        };
        provision_worktree(&spec).unwrap();
        assert!(branch_exists(&repo, "codex/reused-name"));

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(!wt_path.exists());
        assert!(
            !branch_exists(&repo, "codex/reused-name"),
            "an unused lane branch must not outlive its worktree"
        );

        // The whole point: the same launch name provisions again.
        provision_worktree(&spec).expect("re-provisioning the same lane name must succeed");
        assert!(wt_path.join("README").is_file());
    }

    #[test]
    fn expired_cleanup_keeps_a_branch_with_unmerged_work() {
        // A TTL timer is not a mandate to discard commits. The worktree goes;
        // the branch carrying work stays, and the name stays taken until a
        // human decides otherwise.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let wt_path = dir.path().join("wt-lane");
        provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/has-work".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();

        fs::write(wt_path.join("work.txt"), "unmerged").unwrap();
        for args in [
            vec!["add", "work.txt"],
            vec!["commit", "-m", "lane work worth keeping"],
        ] {
            assert!(
                Command::new("git")
                    .args(&args)
                    .current_dir(&wt_path)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(!wt_path.exists(), "the worktree directory is disposable");
        assert!(
            branch_exists(&repo, "codex/has-work"),
            "a branch with unmerged commits must survive worktree cleanup"
        );
    }

    #[test]
    fn ttl_cleanup_never_deletes_a_plain_directory() {
        // #5824: a stale or malformed record pointing at an unrelated
        // directory must not turn TTL cleanup into an unbounded recursive
        // delete just because the TTL is zero.
        let dir = tempdir().unwrap();
        let precious = dir.path().join("not-a-worktree");
        fs::create_dir_all(precious.join("nested")).unwrap();
        fs::write(precious.join("nested/keep.txt"), "keep").unwrap();

        remove_worktree_if_expired(&precious, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            precious.exists(),
            "git cannot identify this path as a managed worktree, so cleanup must do nothing"
        );
        assert_eq!(
            fs::read_to_string(precious.join("nested/keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn ttl_cleanup_never_deletes_inside_an_unrelated_repository() {
        // Git commands succeed from within a subdirectory of some unrelated
        // repo, but that repo does not list the subdirectory as a worktree.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let precious = repo.join("src");
        fs::create_dir_all(&precious).unwrap();
        fs::write(precious.join("keep.txt"), "keep").unwrap();

        remove_worktree_if_expired(&precious, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            precious.exists(),
            "a subdirectory of an unrelated repo is not a managed worktree"
        );
        assert!(precious.join("keep.txt").exists());
    }

    #[test]
    fn ttl_cleanup_never_deletes_a_repository_root() {
        // The main worktree of a repo is not a linked lane worktree: its
        // `.git` is a directory with no registration beneath
        // `.git/worktrees/`. A record pointing there must not wipe a repo.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        remove_worktree_if_expired(&repo, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            repo.exists(),
            "a repository root must never be recursively deleted by TTL cleanup"
        );
        assert!(repo.join(".git").exists());
    }

    #[test]
    fn ttl_cleanup_leaves_a_path_swapped_after_provisioning_intact() {
        // The record was written when the path was a managed worktree; by the
        // time cleanup runs, the directory has been replaced by an unrelated
        // one. Deletion must see the path as it is now, not as the record
        // claims it was.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-lane");
        provision_worktree(&WorktreeProvision {
            repo_root: repo,
            branch: "codex/swapped".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();

        fs::remove_dir_all(&wt_path).unwrap();
        fs::create_dir_all(&wt_path).unwrap();
        fs::write(wt_path.join("keep.txt"), "keep").unwrap();

        remove_worktree_if_expired(&wt_path, Some(0), Some("2020-01-01T00:00:00Z")).unwrap();
        assert!(
            wt_path.exists(),
            "the replacement directory is not the identified worktree and must survive"
        );
        assert!(wt_path.join("keep.txt").exists());
    }

    #[test]
    fn managed_identity_holds_for_a_real_worktree_and_fails_after_a_swap() {
        // The gate that guards the window between identification and removal:
        // it must accept the worktree git provisioned and refuse the same
        // path once its contents no longer resolve to that worktree.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let wt_path = dir.path().join("wt-lane");
        provision_worktree(&WorktreeProvision {
            repo_root: repo.clone(),
            branch: "codex/identity".into(),
            path: wt_path.clone(),
            base_ref: Some("main".into()),
        })
        .unwrap();

        let details = worktree_details(&wt_path).expect("a provisioned worktree identifies itself");
        assert!(is_managed_worktree(&wt_path, &details));

        fs::remove_dir_all(&wt_path).unwrap();
        fs::create_dir_all(&wt_path).unwrap();
        fs::write(wt_path.join("keep.txt"), "keep").unwrap();
        assert!(
            !is_managed_worktree(&wt_path, &details),
            "a swapped directory no longer resolves to the identified worktree"
        );
    }
}

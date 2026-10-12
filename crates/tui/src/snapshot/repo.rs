//! Side-git repository wrapper for workspace snapshots.
//!
//! `SnapshotRepo` shells out to the system `git` binary (we deliberately
//! avoid `git2` to dodge its LGPL surface). The two paths that matter:
//!
//! - `git_dir`  → `<snapshot state dir>/<project_hash>/<worktree_hash>/.git`
//! - `work_tree` → the user's actual workspace
//!
//! Every git invocation passes both `--git-dir` AND `--work-tree`. That is
//! the single biggest safety mechanism: it guarantees we never accidentally
//! mutate the user's own `.git` directory. If git can't find the side
//! repo, the command fails fast instead of falling back to "current
//! directory".

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wait_timeout::ChildExt as _;

use crate::dependencies::ExternalTool;

use super::paths::{ensure_snapshot_dir, snapshot_git_dir};

/// Identifier for a snapshot — the underlying git commit id.
///
/// The field is private: [`SnapshotId::parse`] is the only way to build one,
/// so every value handed to `git` as a revision is a full SHA-1 or SHA-256
/// hex object id and can never be read as an option or a revision expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Accept exactly a full hex object id: 40 (SHA-1) or 64 (SHA-256)
    /// ASCII hex digits. Anything else is `InvalidInput`.
    pub fn parse(id: &str) -> io::Result<Self> {
        if Self::is_well_formed(id) {
            Ok(Self(id.to_string()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot id must be a full hexadecimal commit id",
            ))
        }
    }

    /// Whether `id` would be accepted by [`SnapshotId::parse`].
    pub fn is_well_formed(id: &str) -> bool {
        matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Take the id string out.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Borrow the SHA as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single snapshot record (one row in `git log`).
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Commit SHA inside the side repo.
    pub id: SnapshotId,
    /// Subject line — the label passed to [`SnapshotRepo::snapshot`].
    pub label: String,
    /// Root tree of the snapshot commit. Unlike [`Self::id`] it survives the
    /// survivor-chain rebuild every prune performs (the rebuild re-commits
    /// the same tree under a new commit id), so it is the durable identity a
    /// recorded restore point is resolved by.
    pub tree: SnapshotId,
    /// Author timestamp (Unix seconds).
    pub timestamp: i64,
    /// Session this snapshot belongs to, when recorded (encoded as a
    /// `[sid=...] ` label prefix). `None` for legacy snapshots taken
    /// before session tagging existed.
    pub session_id: Option<String>,
}

/// One path that differs between two snapshots
/// ([`SnapshotRepo::path_changes_between`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPathChange {
    /// Workspace-relative path, as git names it.
    pub path: String,
    /// git's status letter: `A` added, `D` deleted, `M` modified, `T` type
    /// changed.
    pub status: char,
    /// Lines added and removed; `None` for a binary file.
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// What the work tree holds now that a snapshot did not
/// ([`SnapshotRepo::work_tree_changes_since`]), as `git diff` writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkTreeChanges {
    /// One changed path per line, as `git diff --name-only` writes them.
    pub names: String,
    /// `git diff --stat`.
    pub stat: String,
    /// The unified patch, cut at the caller's bound on a char boundary; empty
    /// when more than [`WORK_TREE_PATCH_MAX_LINES`] lines changed.
    pub patch: String,
    /// Whether the patch was cut or left out.
    pub patch_truncated: bool,
}

/// Changed lines past which [`SnapshotRepo::work_tree_changes_since`] does
/// not read the patch at all: git's whole output is buffered before it can be
/// cut, so a generated multi-gigabyte file must not be diffed into memory.
pub const WORK_TREE_PATCH_MAX_LINES: u64 = 200_000;

/// What a file-scoped restore did to one path, relative to the working tree
/// it was applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRestoreAction {
    /// Both the snapshot and the working tree had the path; its content came
    /// back from the snapshot.
    Modified,
    /// The snapshot had the path and the working tree no longer did, so the
    /// restore recreated the file.
    Recreated,
    /// The working tree had the path and the snapshot did not, so the restore
    /// removed the file.
    Removed,
}

impl PathRestoreAction {
    /// Stable wire name, also used by the runtime API response.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Recreated => "recreated",
            Self::Removed => "removed",
        }
    }
}

/// The safety snapshot a path restore writes first, or one already taken.
enum RestoreBackup<'a> {
    Take(&'a str),
    Existing(&'a SnapshotId),
}

/// Report of what [`SnapshotRepo::restore_paths`] did to one path.
#[derive(Debug, Clone)]
pub struct PathRestoreOutcome {
    /// Workspace-relative path that was restored.
    pub path: PathBuf,
    /// How the working tree changed.
    pub action: PathRestoreAction,
}

/// A snapshot as it was just written: its commit and the root tree it holds.
///
/// The commit id can be rewritten by the prune that follows every snapshot
/// once the repo holds more than [`crate::snapshot::DEFAULT_MAX_SNAPSHOTS`]
/// (see `rebuild_survivor_chain`); the tree id cannot, because a tree is
/// content-addressed and the rebuild re-commits the same tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenSnapshot {
    pub id: SnapshotId,
    pub tree: SnapshotId,
}

/// Wrapper around the per-workspace side-git repo.
pub struct SnapshotRepo {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

const STALE_TMP_PACK_AGE: Duration = Duration::from_secs(60 * 60);

/// Advisory lock file inside the side repo's `.git`. Every session, turn and
/// sub-agent working in one workspace shares one side repo, and its index,
/// HEAD and object store are only consistent when snapshot, restore, prune
/// and gc run one at a time: a gc in one session otherwise deleted another
/// session's new, not yet referenced commit and left HEAD on a missing
/// object.
const SNAPSHOT_LOCK_FILE: &str = "codewhale-snapshot.lock";

/// Longest a snapshot, restore or prune waits for another writer's lock
/// before failing (the turn then shows the snapshot-failure notice).
const SNAPSHOT_LOCK_WAIT: Duration = Duration::from_secs(30);

/// How often a waiting writer retries the lock.
const SNAPSHOT_LOCK_POLL: Duration = Duration::from_millis(25);

thread_local! {
    /// Side repos whose write lock this thread already holds, so a locked
    /// operation that calls another (restore takes a safety snapshot, a
    /// snapshot runs the size prune) does not wait on its own lock.
    static HELD_SNAPSHOT_LOCKS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Maximum total snapshot storage in megabytes before pruning kicks in at
/// snapshot time. Keeps the side repo from blowing up the user's disk during
/// long-running or high-churn sessions (#1112).
const MAX_SNAPSHOT_SIZE_MB: u64 = 500;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Grace margin below `MAX_SNAPSHOT_SIZE_MB` used as the prune target
/// so the repo doesn't hit the limit again one snapshot later.
const PRUNE_TARGET_MB: u64 = 400;

/// Default workspace-size ceiling above which snapshots self-disable
/// on first use (2 GB of non-excluded content). Reports from users with
/// multi-hundred-GB project directories — datasets, model weights,
/// docker image dumps that fall outside the built-in excludes —
/// surfaced that `git add -A` on first init would hang the TUI for
/// minutes-to-hours while indexing the workspace. Snapshots are a
/// rollback safety net, not a backup tool; bailing out on workspaces
/// that big is the right tradeoff. Users with legitimate large
/// monorepos can raise `[snapshots] max_workspace_gb` (or set it to
/// `0` to disable the cap entirely).
pub const DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT: u64 = 2 * 1024 * 1024 * 1024;

/// Hard cap on the number of file entries the bounded size estimator
/// will inspect before declaring the workspace "too large". Protects
/// against a workspace with millions of tiny files (no individual
/// file is large, but `git add -A` would still take forever).
pub const SIZE_WALK_MAX_ENTRIES: usize = 200_000;

/// Which snapshot gate refused a workspace. The recovery differs per gate —
/// raising `[snapshots] max_workspace_gb` lifts only [`WorkspaceGate::TooLarge`]
/// — so callers must not offer one gate's remedy for another's failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceGate {
    /// Snapshot-eligible content exceeds the configured byte cap.
    TooLarge,
    /// The bounded walk hit [`SIZE_WALK_MAX_ENTRIES`]. This bound is
    /// independent of the byte cap: `max_workspace_gb = 0` does not lift it.
    TooManyEntries,
}

/// Leading text of the `io::Error` each gate produces. `core::turn` matches on
/// these to pick the right consequence/recovery notice, so they are one
/// declaration shared by producer and matcher rather than two literals.
pub const GATE_TOO_LARGE_MARKER: &str = "workspace too large for snapshots";
pub const GATE_TOO_MANY_ENTRIES_MARKER: &str = "workspace has too many files for snapshots";
pub const GATE_UNSAFE_LOCATION_MARKER: &str = "workspace snapshots are disabled";

/// Display a workspace path in gate diagnostics. The diagnostic names the
/// path the caller passed in — canonicalization is for filesystem and
/// security logic, not for the message. On Windows `Path::canonicalize`
/// rewrites more than the verbatim (`\\?\`) prefix (case, 8.3 names), so a
/// canonical spelling can never be relied on to match what users name; the
/// verbatim prefix is still stripped when present for readability.
fn display_workspace_for_gate(workspace: &Path) -> String {
    let raw = workspace.display().to_string();
    raw.strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(&raw)
        .to_string()
}

impl WorkspaceGate {
    /// One-line English diagnostic for logs, `/undo`, and the gate matcher.
    /// The user-facing consequence and recovery are localized by the notice
    /// surfaces; this string must not restate them.
    fn describe(self, cap_bytes: u64, workspace: &Path) -> String {
        let workspace = display_workspace_for_gate(workspace);
        match self {
            Self::TooLarge => format!(
                "{GATE_TOO_LARGE_MARKER}: over {} bytes of snapshot-eligible content in {workspace}",
                cap_bytes,
            ),
            Self::TooManyEntries => format!(
                "{GATE_TOO_MANY_ENTRIES_MARKER}: over {SIZE_WALK_MAX_ENTRIES} snapshot-eligible entries in {workspace}"
            ),
        }
    }
}

/// Top-level directory and extension patterns that the snapshot path
/// already excludes via `BUILTIN_EXCLUDES`. The estimator skips these
/// up front so the size walk reflects what would actually land in the
/// snapshot commit. Kept narrow to common build-output dirs — anything
/// else falls back to the `.gitignore` filter.
const SIZE_WALK_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    "vendor",
    ".cargo",
    ".rustup",
    ".npm",
    ".bun",
    ".yarn",
    ".pnpm-store",
    ".cache",
    ".venv",
    "venv",
    ".tox",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".gradle",
    ".m2",
    ".local",
    ".git",
];

const BUILTIN_EXCLUDES: &str = "\
# CodeWhale built-in snapshot exclusions
node_modules/
target/
dist/
build/
.build/
.next/
.nuxt/
.svelte-kit/
.turbo/
.parcel-cache/
vendor/
.cargo/
.rustup/
.npm/
.bun/
.yarn/
.pnpm-store/
.cache/
.venv/
venv/
.tox/
__pycache__/
*.pyc
.mypy_cache/
.pytest_cache/
.ruff_cache/
.gradle/
.m2/
.local/
.DS_Store

# Binary and generated artifacts. Snapshots are source rollback checkpoints,
# not a full binary backup; keeping these out avoids side-repo bloat.
*.exe
*.dll
*.so
*.dylib
*.wasm
*.o
*.obj
*.class
*.pdb
*.dSYM
*.zip
*.tar
*.tar.gz
*.tgz
*.tar.bz2
*.tar.xz
*.7z
*.rar
*.iso
*.dmg
*.bin
*.mp4
*.mov
*.mkv
*.avi
*.webm
*.mp3
*.wav
*.flac
*.aac
";

impl SnapshotRepo {
    /// Open an existing snapshot repo for `workspace` without creating or
    /// initializing anything on disk.
    ///
    /// This is useful for read-only UI surfaces that want to report checkpoint
    /// availability without paying the first-init size walk or surprising the
    /// user by creating a side repo from a view action.
    pub fn open_existing(workspace: &Path) -> io::Result<Option<Self>> {
        let work_tree = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let git_dir = snapshot_git_dir(&work_tree)?;
        if !git_dir.exists() || !git_dir.join("HEAD").exists() {
            return Ok(None);
        }
        Ok(Some(Self { git_dir, work_tree }))
    }

    /// Open or initialize the snapshot repo for `workspace`.
    ///
    /// On first use this:
    /// 1. Creates the `.git` dir under the resolved snapshot store.
    /// 2. Runs `git init --bare=false --quiet`.
    /// 3. Sets a fixed `user.name` / `user.email` so commits don't pick up
    ///    the user's global git identity (we don't want our snapshots to
    ///    look like they came from the user).
    pub fn open_or_init(workspace: &Path) -> io::Result<Self> {
        Self::open_or_init_with_cap(workspace, DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT)
    }

    /// Variant of [`Self::open_or_init`] that accepts an explicit
    /// workspace-size cap. `cap_bytes = 0` disables the cap entirely
    /// (always snapshot, regardless of size).
    ///
    /// When the workspace exceeds the cap and the side repo hasn't
    /// been initialized yet, returns `Err(InvalidInput)` with a
    /// "workspace too large" reason. Subsequent calls (after the user
    /// shrinks the workspace or raises the cap via config) succeed.
    pub fn open_or_init_with_cap(workspace: &Path, cap_bytes: u64) -> io::Result<Self> {
        let work_tree = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        if let Some(reason) = unsafe_workspace_snapshot_reason(
            &work_tree,
            crate::config::effective_home_dir().as_deref(),
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{GATE_UNSAFE_LOCATION_MARKER} for {reason}: {}",
                    display_workspace_for_gate(workspace)
                ),
            ));
        }

        let git_dir = ensure_snapshot_dir(&work_tree)?.join(".git");

        // A `.git` without HEAD is an init that never finished (or only a
        // peer's lock file): initialize it rather than use it half-made.
        let needs_init = !git_dir.join("HEAD").exists();
        if needs_init {
            // First-init size guard. Skipping this on subsequent opens
            // is intentional: paying a workspace walk on every snapshot
            // would defeat the purpose of the cap, and a workspace
            // that fit on first init is allowed to grow within the
            // existing repo's `MAX_SNAPSHOT_SIZE_MB` budget. Users on
            // workspaces that grew past the cap mid-session get the
            // existing aggressive-pruning path in `snapshot()`.
            if let Err(gate) =
                estimate_workspace_size_bounded(&work_tree, cap_bytes, SIZE_WALK_MAX_ENTRIES)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    gate.describe(cap_bytes, workspace),
                ));
            }
            // The lock file lives in `.git`, so create it first; `git init`
            // accepts an existing `.git` directory.
            std::fs::create_dir_all(&git_dir)?;
        }
        let repo = Self { git_dir, work_tree };
        if needs_init {
            // Two sessions opening a new workspace at once must not both run
            // `git init` and the identity config: a config write that loses
            // git's own config lock is ignored, and a snapshot committed
            // before the identity lands would carry the user's.
            repo.with_write_lock(|| {
                if repo.git_dir.join("HEAD").exists() {
                    return Ok(());
                }
                repo.init_side_repo()
            })?;
        }

        write_builtin_excludes(&repo.git_dir)?;
        if let Err(err) = cleanup_stale_pack_temps(&repo.git_dir, STALE_TMP_PACK_AGE) {
            tracing::debug!(
                target: "snapshot",
                "failed to clean stale snapshot tmp_pack files: {err}"
            );
        }
        Ok(repo)
    }

    /// `git init` the side repo and pin its config. Runs under the write lock.
    fn init_side_repo(&self) -> io::Result<()> {
        let (git_dir, work_tree) = (&self.git_dir, &self.work_tree);
        let parent = git_dir.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "snapshot dir has no parent")
        })?;
        // `git init` here uses the parent directory as the work tree
        // and stores metadata in `.git`. We then continue to use
        // explicit `--git-dir` / `--work-tree` flags for every other
        // command so behaviour is invariant of cwd.
        let mut git =
            crate::dependencies::Git::command().ok_or_else(|| io_other("git not found on PATH"))?;
        let init = git.arg("init").arg("--quiet").arg(parent);
        let init = run_bounded_git(init, "init")
            .map_err(|e| io_other(format!("failed to run git init: {e}")))?;
        if !init.status.success() {
            return Err(io_other(format!(
                "git init failed: {}",
                String::from_utf8_lossy(&init.stderr).trim()
            )));
        }

        // Pin a stable identity so snapshot commits are recognisable
        // and don't bleed into the user's git config.
        let _ = run_git(
            git_dir,
            work_tree,
            &["config", "user.name", "deepseek-snapshots"],
        );
        let _ = run_git(
            git_dir,
            work_tree,
            &["config", "user.email", "snapshots@codewhale.local"],
        );
        // Don't auto-gc on every commit; we manage pruning ourselves.
        let _ = run_git(git_dir, work_tree, &["config", "gc.auto", "0"]);
        // Ignore CRLF rewriting — we want byte-for-byte fidelity.
        let _ = run_git(git_dir, work_tree, &["config", "core.autocrlf", "false"]);
        Ok(())
    }

    /// Take a snapshot of the current working tree.
    ///
    /// Internally: `git add -A`, `git write-tree`, `git commit-tree`, then
    /// `git update-ref HEAD <commit>`.
    /// `git add -A` honours the user's workspace ignore rules while staging
    /// into the side repo's index.
    ///
    /// Before committing, checks whether the snapshot directory exceeds
    /// [`MAX_SNAPSHOT_SIZE_MB`] and prunes the oldest snapshots if it does.
    ///
    /// Returns the snapshot's commit SHA.
    #[allow(dead_code)] // convenience entry kept for tests and legacy callers; production writes go through snapshot_with_session
    pub fn snapshot(&self, label: &str) -> io::Result<SnapshotId> {
        self.snapshot_with_session(label, None)
    }

    /// Take a snapshot, tagging it with the owning session id.
    ///
    /// The session id is encoded into the commit message as a `[sid=...] `
    /// label prefix. [`Self::list`] decodes it back into
    /// [`Snapshot::session_id`] and strips the prefix from the visible
    /// label, so existing listing surfaces keep showing the plain label.
    /// Legacy snapshots taken through [`Self::snapshot`] carry no prefix
    /// and decode with `session_id == None`.
    pub fn snapshot_with_session(
        &self,
        label: &str,
        session_id: Option<&str>,
    ) -> io::Result<SnapshotId> {
        self.take_snapshot(label, session_id).map(|taken| taken.id)
    }

    /// [`Self::snapshot_with_session`], also reporting the root tree the
    /// snapshot holds — the identity a recorded restore point keeps.
    pub fn take_snapshot(
        &self,
        label: &str,
        session_id: Option<&str>,
    ) -> io::Result<TakenSnapshot> {
        self.with_write_lock(|| self.take_snapshot_locked(label, session_id))
    }

    fn take_snapshot_locked(
        &self,
        label: &str,
        session_id: Option<&str>,
    ) -> io::Result<TakenSnapshot> {
        // Guard against disk blowup (#1112): if the snapshot directory has
        // grown beyond the limit, prune aggressively before adding more.
        // When the prune actually destroys restore points the user is told
        // once per workspace — losing undo history to a log line is the S5
        // failure mode (2026-08-04 snapshot hunt).
        if let Ok(removed) = self.prune_size_pressure(
            MAX_SNAPSHOT_SIZE_MB * BYTES_PER_MB,
            PRUNE_TARGET_MB * BYTES_PER_MB,
        ) && removed > 0
        {
            notify_snapshot_history_pruned_once(&self.work_tree, removed);
        }
        // A HEAD that names a missing commit would make every `commit-tree
        // -p` below fail ("is not a valid object"), silently ending undo.
        self.repair_broken_head()?;
        let parent = run_git(
            &self.git_dir,
            &self.work_tree,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        )?;
        let parent = parent
            .status
            .success()
            .then(|| String::from_utf8_lossy(&parent.stdout).trim().to_string())
            .filter(|s| !s.is_empty());

        self.stage_work_tree()?;
        let (tree, sha) = match self.commit_staged_tree(parent.as_deref(), label, session_id) {
            Ok(done) => done,
            Err(first) => {
                // An index naming objects that no longer exist (left by an
                // interrupted or racing gc) breaks every later snapshot:
                // `add -A` does not rehash files whose stat data is
                // unchanged, and `write-tree` hands back the index's cached
                // tree id even when that tree is gone, so `commit-tree` then
                // fails. Rebuild the index from the work tree once.
                let reset = run_git(&self.git_dir, &self.work_tree, &["read-tree", "--empty"])?;
                if !reset.status.success() {
                    return Err(first);
                }
                self.stage_work_tree()?;
                self.commit_staged_tree(parent.as_deref(), label, session_id)?
            }
        };

        self.move_head(Some(&sha), parent.as_deref())?;

        let id = SnapshotId::parse(&sha).map_err(|_| {
            io_other(format!(
                "git commit-tree returned a malformed commit id: {sha:?}"
            ))
        })?;
        let tree = SnapshotId::parse(&tree).map_err(|_| {
            io_other(format!(
                "git write-tree returned a malformed tree id: {tree:?}"
            ))
        })?;
        Ok(TakenSnapshot { id, tree })
    }

    /// `write-tree` the staged index and `commit-tree` it onto `parent`,
    /// returning the tree and commit ids.
    fn commit_staged_tree(
        &self,
        parent: Option<&str>,
        label: &str,
        session_id: Option<&str>,
    ) -> io::Result<(String, String)> {
        let tree = run_git(&self.git_dir, &self.work_tree, &["write-tree"])?;
        if !tree.status.success() {
            return Err(io_other(format!(
                "git write-tree failed: {}",
                String::from_utf8_lossy(&tree.stderr).trim()
            )));
        }
        let tree = String::from_utf8_lossy(&tree.stdout).trim().to_string();

        let mut args = vec!["commit-tree".to_string(), tree.clone()];
        if let Some(parent) = parent {
            args.push("-p".to_string());
            args.push(parent.to_string());
        }
        args.push("-m".to_string());
        args.push(Self::encode_session_label(label, session_id));
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        // `commit-tree` creates marker commits even when the tree matches its
        // parent, and it does not run user/global commit hooks.
        let commit = run_git(&self.git_dir, &self.work_tree, &arg_refs)?;
        if !commit.status.success() {
            return Err(io_other(format!(
                "git commit-tree failed: {}",
                String::from_utf8_lossy(&commit.stderr).trim()
            )));
        }
        let sha = String::from_utf8_lossy(&commit.stdout).trim().to_string();
        Ok((tree, sha))
    }

    /// Repair a side repo whose HEAD names a commit that no longer exists
    /// (an interrupted gc or prune, a copied or partially deleted
    /// `~/.codewhale/snapshots` directory). Left alone, every later snapshot
    /// fails on `commit-tree -p <missing>` and /undo is dead without a word.
    ///
    /// The broken ref is deleted so the next snapshot starts a fresh history.
    /// Restore points before the break cannot be recovered; the caller tells
    /// the user. Returns `true` when a repair happened.
    pub fn repair_broken_head(&self) -> io::Result<bool> {
        self.with_write_lock(|| self.repair_broken_head_locked())
    }

    fn repair_broken_head_locked(&self) -> io::Result<bool> {
        let commit = run_git(
            &self.git_dir,
            &self.work_tree,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        )?;
        if commit.status.success() {
            return Ok(false);
        }
        // An unborn branch (fresh repo) names nothing: nothing to repair.
        let named = run_git(
            &self.git_dir,
            &self.work_tree,
            &["rev-parse", "--verify", "--quiet", "HEAD"],
        )?;
        if !named.status.success() {
            return Ok(false);
        }
        let missing = String::from_utf8_lossy(&named.stdout).trim().to_string();
        // A lookup can fail for a moment while another session sharing this
        // repo runs gc or repack. Only a commit that is really absent
        // justifies touching HEAD: deleting it discards every restore point.
        if self.is_commit(&missing)? {
            return Ok(false);
        }
        self.reset_missing_head(&missing)
    }

    /// Move HEAD off `missing`, a commit id it named when this repair looked.
    /// Both moves are compare-and-swap against `missing`: a writer that does
    /// not take the snapshot lock (an older build) may have published a valid
    /// snapshot since, and neither the reflog reset nor the delete may then
    /// overwrite it. A refused reset is reported, never followed by a delete.
    fn reset_missing_head(&self, missing: &str) -> io::Result<bool> {
        // The newest reflog entry that still names a commit keeps the
        // restore points before the break.
        if let Some(recovered) = self.newest_reflog_commit(missing)? {
            self.move_head(Some(&recovered), Some(missing))
                .map_err(|error| {
                    io_other(format!(
                        "snapshot history HEAD pointed at missing commit {missing} and was not reset to {recovered}: {error}"
                    ))
                })?;
            tracing::warn!(
                target: "snapshot",
                "snapshot history HEAD pointed at missing commit {missing}; reset to {recovered} from the reflog"
            );
            return Ok(false);
        }
        self.move_head(None, Some(missing)).map_err(|error| {
            io_other(format!(
                "snapshot history HEAD points at missing commit {missing} and could not be reset: {error}"
            ))
        })?;
        tracing::warn!(
            target: "snapshot",
            "snapshot history HEAD pointed at missing commit {missing}; started a fresh history"
        );
        Ok(true)
    }

    fn is_commit(&self, oid: &str) -> io::Result<bool> {
        let object = format!("{oid}^{{commit}}");
        Ok(
            run_git(&self.git_dir, &self.work_tree, &["cat-file", "-e", &object])?
                .status
                .success(),
        )
    }

    /// The newest commit recorded in HEAD's reflogs (its branch's, then
    /// HEAD's own) that still exists, skipping `missing`.
    fn newest_reflog_commit(&self, missing: &str) -> io::Result<Option<String>> {
        let branch = run_git(&self.git_dir, &self.work_tree, &["symbolic-ref", "HEAD"])?;
        let mut logs = Vec::new();
        if branch.status.success() {
            let name = String::from_utf8_lossy(&branch.stdout).trim().to_string();
            logs.push(self.git_dir.join("logs").join(name));
        }
        logs.push(self.git_dir.join("logs").join("HEAD"));
        for log in logs {
            let Ok(text) = std::fs::read_to_string(&log) else {
                continue;
            };
            // Each line is `<old> <new> <who> <when>\t<message>`.
            for line in text.lines().rev() {
                let mut fields = line.split(' ');
                let (Some(old), Some(new)) = (fields.next(), fields.next()) else {
                    continue;
                };
                for oid in [new, old] {
                    if oid.len() >= 40
                        && oid != missing
                        && oid.bytes().any(|b| b != b'0')
                        && self.is_commit(oid)?
                    {
                        return Ok(Some(oid.to_string()));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Point the side repo's HEAD branch at a commit id that does not exist.
    #[cfg(test)]
    pub(crate) fn point_head_at_missing_commit_for_test(&self) {
        let branch = run_git(&self.git_dir, &self.work_tree, &["symbolic-ref", "HEAD"])
            .expect("symbolic-ref");
        let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
        let path = self.git_dir.join(&branch);
        std::fs::create_dir_all(path.parent().expect("ref parent")).expect("ref dir");
        std::fs::write(path, "1111111111111111111111111111111111111111\n").expect("write ref");
    }

    /// Prefix a snapshot label with its owning session id, if any.
    fn encode_session_label(label: &str, session_id: Option<&str>) -> String {
        match session_id {
            Some(sid) if !sid.is_empty() => format!("[sid={sid}] {label}"),
            _ => label.to_string(),
        }
    }

    /// Split a possibly session-tagged label back into `(session_id, label)`.
    ///
    /// Returns `(None, label)` for untagged labels. The decoded label is
    /// the original one without the `[sid=...] ` prefix, so consumers that
    /// match on `pre-turn:`/`tool:`/`redo:` prefixes keep working unchanged.
    fn decode_session_label(label: &str) -> (Option<String>, String) {
        let Some(rest) = label.strip_prefix("[sid=") else {
            return (None, label.to_string());
        };
        let Some(end) = rest.find("] ") else {
            return (None, label.to_string());
        };
        let sid = &rest[..end];
        let plain = &rest[end + 2..];
        if sid.is_empty() || plain.is_empty() {
            return (None, label.to_string());
        }
        (Some(sid.to_string()), plain.to_string())
    }
    /// Size-pressure prune (#1112): if the side repo exceeds `max_bytes`,
    /// drop the oldest half of the snapshots, repeatedly, until the store is
    /// at or under `target_bytes` or only the protected restore points are
    /// left. Returns the number of snapshots destroyed, so the caller can
    /// tell the user their undo history shrank (S5).
    ///
    /// The prune goes oldest first by count and always keeps the newest
    /// snapshot plus each session's newest `pre-turn:` and `post-turn:`
    /// boundaries, so every session's running turn (and the one before it)
    /// stays restorable. It used to
    /// prune by age starting at one second, which on the first pass dropped
    /// every snapshot older than a second and then wiped the rest: a workspace
    /// whose side repo sat above the cap lost all undo history on every
    /// snapshot.
    fn prune_size_pressure(&self, max_bytes: u64, target_bytes: u64) -> io::Result<usize> {
        self.with_write_lock(|| {
            let current_bytes = dir_size_bytes(&self.git_dir)?;
            if current_bytes <= max_bytes {
                return Ok(0);
            }
            tracing::warn!(
                target: "snapshot",
                current_mb = current_bytes / BYTES_PER_MB,
                limit_mb = max_bytes / BYTES_PER_MB,
                "snapshot storage over limit — pruning the oldest snapshots"
            );
            let mut removed_total: usize = 0;
            loop {
                let snapshots = self.list(usize::MAX)?;
                let mut survivors = size_pressure_survivors(&snapshots, snapshots.len() / 2);
                if survivors.len() >= snapshots.len() {
                    // Halving kept only protected points: cut to those alone.
                    survivors = size_pressure_survivors(&snapshots, 0);
                }
                if survivors.is_empty() || survivors.len() >= snapshots.len() {
                    break;
                }
                self.rebuild_survivor_chain(&survivors, &snapshots[0].id)?;
                self.reclaim_unreachable();
                removed_total = removed_total.saturating_add(snapshots.len() - survivors.len());
                let new_size = dir_size_bytes(&self.git_dir)?;
                if new_size <= target_bytes {
                    tracing::info!(
                        target: "snapshot",
                        new_size_mb = new_size / BYTES_PER_MB,
                        "pruned snapshot storage back under limit"
                    );
                    break;
                }
            }
            Ok(removed_total)
        })
    }

    /// Restore the workspace to the state at `id`.
    ///
    /// Requires a durable safety snapshot before changing files. A failed
    /// restore attempts to put those files back; if that also fails, the
    /// error identifies the retained safety snapshot for recovery. This is
    /// not atomic against an external editor changing the live workspace.
    /// File/directory transitions are refused before checkout because the
    /// replaced directory may contain files excluded from the backup.
    /// We never touch the user's own `.git`.
    pub fn restore(&self, id: &SnapshotId) -> io::Result<()> {
        self.with_write_lock(|| self.restore_locked(id))
    }

    fn restore_locked(&self, id: &SnapshotId) -> io::Result<()> {
        // The backup label is deliberately not an undo/revert-turn candidate.
        let target_short = &id.as_str()[..id.as_str().len().min(12)];
        let backup = self
            .snapshot_with_session(&format!("pre-restore:{target_short}"), None)
            .map_err(|error| {
                io_other(format!(
                    "pre-restore safety snapshot failed; no workspace files were changed: {error}"
                ))
            })?;
        let current_paths = self.tree_paths(backup.as_str())?;
        let target_paths = self.tree_paths(id.as_str())?;
        for rel in &target_paths {
            match std::fs::symlink_metadata(self.work_tree.join(rel)) {
                Ok(metadata) if metadata.is_dir() => {
                    return Err(io_other(format!(
                        "'{}' requires a file/directory transition; nothing was restored",
                        rel.display()
                    )));
                }
                Ok(_) if !current_paths.contains(rel) => {
                    return Err(io_other(format!(
                        "'{}' was excluded from the safety snapshot; nothing was restored",
                        rel.display()
                    )));
                }
                // Unix reports a path under a live file as NotADirectory;
                // Windows reports NotFound, so look for the file itself.
                Err(error)
                    if error.kind() == io::ErrorKind::NotADirectory
                        || (error.kind() == io::ErrorKind::NotFound
                            && ancestor_is_not_a_directory(&self.work_tree, rel)) =>
                {
                    return Err(io_other(format!(
                        "'{}' requires a file/directory transition; nothing was restored",
                        rel.display()
                    )));
                }
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        if let Err(error) = self.restore_tree(id, &current_paths, &target_paths) {
            let recovery = match self.restore_tree(&backup, &target_paths, &current_paths) {
                Ok(()) => "previous snapshot files were restored".to_string(),
                Err(rollback) => format!("rollback also failed: {rollback}"),
            };
            return Err(io_other(format!(
                "restore failed: {error}; {recovery}; safety snapshot {} retains the previous files",
                backup.as_str()
            )));
        }
        Ok(())
    }

    fn restore_tree(
        &self,
        id: &SnapshotId,
        current_paths: &HashSet<PathBuf>,
        target_paths: &HashSet<PathBuf>,
    ) -> io::Result<()> {
        // An empty target (the first snapshot of an empty directory) has no
        // path for `:/` to match, and git refuses the checkout outright; there
        // is nothing to write back, only the later files to remove.
        if !target_paths.is_empty() {
            let checkout = run_git(
                &self.git_dir,
                &self.work_tree,
                &["checkout", id.as_str(), "--", ":/"],
            )?;
            if !checkout.status.success() {
                return Err(io_other(format!(
                    "git checkout failed: {}",
                    String::from_utf8_lossy(&checkout.stderr).trim()
                )));
            }
        }
        self.remove_paths_missing_from_target(current_paths, target_paths)
    }

    /// File restore never traverses symlinks, directories, or Git metadata.
    /// Validate every existing component before reading, backing up or writing.
    pub fn validate_restore_file(&self, rel: &Path) -> io::Result<bool> {
        if !is_safe_relative_path(rel)
            || rel
                .components()
                .any(|part| is_git_metadata_name(part.as_os_str()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "refusing to restore unsafe path '{}': restore requires a regular workspace file",
                    rel.display()
                ),
            ));
        }
        let mut path = self.work_tree.clone();
        for part in rel.components() {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(meta)
                    if meta.file_type().is_symlink()
                        || (path == self.work_tree.join(rel) && !meta.is_file())
                        || (path != self.work_tree.join(rel) && !meta.is_dir()) =>
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "restore refuses directories, symlinks and non-regular files",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn snapshot_contains_regular_file(&self, id: &SnapshotId, rel: &Path) -> io::Result<bool> {
        self.snapshot_file_blob(id, rel).map(|blob| blob.is_some())
    }

    /// The blob id `rel` holds in snapshot (commit or tree) `id`, or `None`
    /// when the snapshot does not contain it. Anything other than a regular
    /// file is `InvalidInput`: file-scoped restores never write symlinks or
    /// directories.
    fn snapshot_file_blob(&self, id: &SnapshotId, rel: &Path) -> io::Result<Option<String>> {
        let entry = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--end-of-options",
                id.as_str(),
                "--",
                rel.to_str()
                    .ok_or_else(|| io_other("restore path must be UTF-8"))?,
            ],
        )?;
        if !entry.status.success() {
            return Err(io_other(format!(
                "Failed to inspect snapshot file: {}",
                String::from_utf8_lossy(&entry.stderr).trim()
            )));
        }
        if entry.stdout.is_empty() {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&entry.stdout);
        let blob = line
            .strip_prefix("100644 blob ")
            .or_else(|| line.strip_prefix("100755 blob "))
            .and_then(|rest| rest.split('\t').next())
            .filter(|sha| SnapshotId::is_well_formed(sha))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "snapshot path is not a regular file",
                )
            })?;
        Ok(Some(blob.to_string()))
    }

    /// Whether the working-tree bytes of `rel` are exactly what snapshot `id`
    /// holds for it (both absent counts as a match).
    ///
    /// Compares content hashes directly instead of `git diff <id>`: the side
    /// repo's index only lists paths present at the last snapshot, and a diff
    /// against a commit reports a path the index lost as deleted even when
    /// the file is on disk.
    pub fn path_matches_snapshot(&self, id: &SnapshotId, rel: &Path) -> io::Result<bool> {
        let in_work = self.validate_restore_file(rel)?;
        match (self.snapshot_file_blob(id, rel)?, in_work) {
            (None, false) => Ok(true),
            (None, true) | (Some(_), false) => Ok(false),
            (Some(blob), true) => {
                let rel_str = rel
                    .to_str()
                    .ok_or_else(|| io_other("restore path must be UTF-8"))?;
                let abs = self.work_tree.join(rel);
                let abs = abs
                    .to_str()
                    .ok_or_else(|| io_other("restore path must be UTF-8"))?;
                // Git for Windows cannot open the `\\?\` verbatim form that
                // `canonicalize` gives the work tree.
                let abs = match abs.strip_prefix(r"\\?\") {
                    Some(rest) => match rest.strip_prefix(r"UNC\") {
                        Some(unc) => format!(r"\\{unc}"),
                        None => rest.to_string(),
                    },
                    None => abs.to_string(),
                };
                let hashed = run_git(
                    &self.git_dir,
                    &self.work_tree,
                    &["hash-object", &format!("--path={rel_str}"), "--", &abs],
                )?;
                if !hashed.status.success() {
                    return Err(io_other(format!(
                        "git hash-object failed: {}",
                        String::from_utf8_lossy(&hashed.stderr).trim()
                    )));
                }
                Ok(String::from_utf8_lossy(&hashed.stdout).trim() == blob)
            }
        }
    }

    /// Paths whose content differs between snapshots `from` and `to` (commit
    /// or tree ids), with renames reported as a delete plus an add so each
    /// side's path is restorable on its own.
    pub fn changed_paths_between(
        &self,
        from: &SnapshotId,
        to: &SnapshotId,
    ) -> io::Result<Vec<PathBuf>> {
        let diff = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "diff",
                "--no-renames",
                "--name-only",
                "-z",
                "--end-of-options",
                from.as_str(),
                to.as_str(),
                "--",
            ],
        )?;
        if !diff.status.success() {
            return Err(io_other(format!(
                "git diff --name-only failed: {}",
                String::from_utf8_lossy(&diff.stderr).trim()
            )));
        }
        let mut paths: Vec<PathBuf> = parse_nul_paths(&diff.stdout).into_iter().collect();
        paths.sort();
        Ok(paths)
    }

    /// Whether `rel` has the same content (or is absent) in both snapshots.
    pub fn path_same_in_snapshots(
        &self,
        a: &SnapshotId,
        b: &SnapshotId,
        rel: &Path,
    ) -> io::Result<bool> {
        Ok(self.snapshot_file_blob(a, rel)? == self.snapshot_file_blob(b, rel)?)
    }

    /// Return whether `rel` differs between snapshot `id` and the current
    /// working tree.
    ///
    /// This is the single-path counterpart of
    /// [`Self::work_tree_matches_snapshot`]: it answers "would restoring just
    /// this file change anything?", which is what file-scoped revert
    /// cursoring needs. A path that exists in neither the snapshot nor the
    /// working tree does not differ.
    pub fn path_differs_from_snapshot(&self, id: &SnapshotId, rel: &Path) -> io::Result<bool> {
        let in_work = self.validate_restore_file(rel)?;
        let in_target = self.snapshot_contains_regular_file(id, rel)?;
        match (in_target, in_work) {
            // Neither side has it: nothing to restore and nothing to remove.
            (false, false) => Ok(false),
            // The snapshot has it and the working tree lost it.
            (true, false) => Ok(true),
            // The path was created after the snapshot.
            (false, true) => Ok(true),
            (true, true) => {
                let rel = rel.to_string_lossy().into_owned();
                let diff = run_git(
                    &self.git_dir,
                    &self.work_tree,
                    &[
                        "--literal-pathspecs",
                        "diff",
                        "--quiet",
                        "--end-of-options",
                        id.as_str(),
                        "--",
                        rel.as_str(),
                    ],
                )?;
                git_diff_matches(diff).map(|matches| !matches)
            }
        }
    }

    /// Restore only `rel_paths` from snapshot `id`.
    ///
    /// This is the file-scoped counterpart of [`Self::restore`]. The
    /// difference that matters: the whole-tree `git checkout <sha> -- :/` is
    /// replaced by a pathspec-limited checkout, so a working-tree path outside
    /// `rel_paths` is never written or deleted. The safety backup reads the workspace.
    ///
    /// A path the snapshot does not track is removed from the working tree
    /// (that is how a file created after the snapshot is reverted), and a path
    /// the snapshot tracks but the working tree lost is recreated. A path that
    /// exists in neither side produces no outcome at all, rather than a
    /// report claiming a change that did not happen.
    #[cfg(test)]
    pub fn restore_paths(
        &self,
        id: &SnapshotId,
        rel_paths: &[PathBuf],
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        self.restore_paths_checked(id, rel_paths, || Ok(()))
    }

    pub fn restore_file_if_unchanged(
        &self,
        id: &SnapshotId,
        rel: &Path,
        expected_hash: &str,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        let verify = || {
            let actual = if self.validate_restore_file(rel)? {
                let bytes = std::fs::read(self.work_tree.join(rel))?;
                format!("sha256:{}", crate::hashing::sha256_hex(bytes))
            } else {
                "absent".to_string()
            };
            if actual != expected_hash {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "The file changed after the selected change record. Refresh and review it before restoring; nothing was changed.",
                ));
            }
            Ok(())
        };
        verify()?;
        self.restore_paths_checked(id, &[rel.to_path_buf()], verify)
    }

    fn restore_paths_checked(
        &self,
        id: &SnapshotId,
        rel_paths: &[PathBuf],
        preflight: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        let target_short = &id.as_str()[..id.as_str().len().min(12)];
        let plan: Vec<(PathBuf, SnapshotId)> = rel_paths
            .iter()
            .map(|rel| (rel.clone(), id.clone()))
            .collect();
        self.restore_path_plan(
            &plan,
            &format!("pre-restore:{target_short}"),
            false,
            preflight,
        )
    }

    /// Restore each `(path, source)` pair of `plan` from its own snapshot
    /// (commit or tree id), and nothing else.
    ///
    /// The whole plan is validated, then one mandatory `backup_label` safety
    /// snapshot is written, then `preflight` runs immediately before the
    /// first mutation. A path its source does not hold is removed; with
    /// `prune_emptied_dirs` the directories that removal leaves empty go too
    /// (a turn-scoped undo removes the directories a dropped turn created),
    /// otherwise they stay (a file-scoped revert names a file, not a tree).
    pub fn restore_path_plan(
        &self,
        plan: &[(PathBuf, SnapshotId)],
        backup_label: &str,
        prune_emptied_dirs: bool,
        preflight: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        self.restore_path_plan_backed_up(
            plan,
            RestoreBackup::Take(backup_label),
            prune_emptied_dirs,
            preflight,
        )
    }

    /// [`Self::restore_path_plan`] with a safety snapshot the caller already
    /// took (`backup`, a commit id) instead of a new one. `preflight` must
    /// prove every planned path is still as `backup` holds it, so the backup
    /// is as good as one taken now; reusing it keeps a second snapshot, and
    /// the prune that comes with it, out of the window between planning and
    /// the first write.
    pub fn restore_path_plan_with_backup(
        &self,
        plan: &[(PathBuf, SnapshotId)],
        backup: &SnapshotId,
        prune_emptied_dirs: bool,
        preflight: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        self.restore_path_plan_backed_up(
            plan,
            RestoreBackup::Existing(backup),
            prune_emptied_dirs,
            preflight,
        )
    }

    fn restore_path_plan_backed_up(
        &self,
        plan: &[(PathBuf, SnapshotId)],
        backup: RestoreBackup<'_>,
        prune_emptied_dirs: bool,
        preflight: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        self.with_write_lock(|| {
            self.restore_path_plan_locked(plan, backup, prune_emptied_dirs, preflight)
        })
    }

    fn restore_path_plan_locked(
        &self,
        plan: &[(PathBuf, SnapshotId)],
        backup: RestoreBackup<'_>,
        prune_emptied_dirs: bool,
        preflight: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Vec<PathRestoreOutcome>> {
        if plan.is_empty() {
            return Ok(Vec::new());
        }
        // Validate the entire request before any mutation or backup. A snapshot
        // directory entry must not turn a file action into recursive checkout.
        let mut pre_state = Vec::with_capacity(plan.len());
        for (rel, id) in plan {
            let in_work = self.validate_restore_file(rel)?;
            let in_target = self.snapshot_contains_regular_file(id, rel)?;
            pre_state.push((rel.clone(), id.clone(), in_target, in_work));
        }

        // A durable backup is required for this destructive API. Ignored
        // files cannot be removed/overwritten if the snapshot cannot retain them.
        let backup = match backup {
            RestoreBackup::Take(label) => self.snapshot_with_session(label, None)?,
            RestoreBackup::Existing(id) => id.clone(),
        };
        for (rel, _, _, in_work) in &pre_state {
            if *in_work && !self.snapshot_contains_regular_file(&backup, rel)? {
                return Err(io_other(
                    "File was excluded from the safety snapshot; nothing was restored",
                ));
            }
            self.validate_restore_file(rel)?;
        }

        // Recheck after the potentially slow safety snapshot, immediately
        // before checkout/removal. New editor work is retained in the backup.
        preflight()?;

        // One pathspec-limited checkout per source snapshot, in plan order.
        let mut sources: Vec<&SnapshotId> = Vec::new();
        for (_, id, in_target, _) in &pre_state {
            if *in_target && !sources.contains(&id) {
                sources.push(id);
            }
        }
        for source in sources {
            let tracked: Vec<String> = pre_state
                .iter()
                .filter(|(_, id, in_target, _)| *in_target && id == source)
                .map(|(rel, _, _, _)| rel.to_string_lossy().into_owned())
                .collect();
            let mut args: Vec<String> = vec![
                "--literal-pathspecs".to_string(),
                "checkout".to_string(),
                source.as_str().to_string(),
                "--".to_string(),
            ];
            args.extend(tracked);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let checkout = run_git(&self.git_dir, &self.work_tree, &arg_refs)?;
            if !checkout.status.success() {
                return Err(io_other(format!(
                    "git checkout failed: {} (safety snapshot {} holds the previous files)",
                    String::from_utf8_lossy(&checkout.stderr).trim(),
                    backup.as_str()
                )));
            }
        }

        let mut outcomes = Vec::new();
        for (rel, _, in_target, was_in_work) in pre_state {
            match (in_target, was_in_work) {
                (true, true) => outcomes.push(PathRestoreOutcome {
                    path: rel,
                    action: PathRestoreAction::Modified,
                }),
                (true, false) => outcomes.push(PathRestoreOutcome {
                    path: rel,
                    action: PathRestoreAction::Recreated,
                }),
                (false, true) => {
                    let path = self.work_tree.join(&rel);
                    self.validate_restore_file(&rel)?;
                    std::fs::remove_file(&path).map_err(|error| {
                        io_other(format!(
                            "removing '{}' failed: {error} (safety snapshot {} holds the previous files)",
                            rel.display(),
                            backup.as_str()
                        ))
                    })?;
                    if prune_emptied_dirs {
                        self.prune_empty_parent_dirs(path.parent());
                    }
                    outcomes.push(PathRestoreOutcome {
                        path: rel,
                        action: PathRestoreAction::Removed,
                    });
                }
                // Already in the snapshot's state.
                (false, false) => {}
            }
        }
        Ok(outcomes)
    }

    /// Return whether the current workspace matches the given snapshot's
    /// tracked file content.
    ///
    /// This is intentionally narrower than a full "workspace identical"
    /// claim: it compares the current working tree against the snapshot's
    /// tracked paths via git's diff machinery. That is sufficient for
    /// `/undo` cursoring — if the diff is empty, restoring this snapshot
    /// again would be a no-op, so the caller should continue scanning
    /// older snapshots.
    pub fn work_tree_matches_snapshot(&self, id: &SnapshotId) -> io::Result<bool> {
        let diff = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "diff",
                "--quiet",
                "--end-of-options",
                id.as_str(),
                "--",
                ":/",
            ],
        )?;
        git_diff_matches(diff)
    }

    /// Everything in the work tree that differs from snapshot `from`: files
    /// changed, deleted, and created since, including ones no snapshot has
    /// recorded yet. New ignored paths stay out, as they stay out of a
    /// snapshot; a path `from` holds is compared even if it is ignored now,
    /// as the next snapshot would still record it.
    ///
    /// Read-only for the user and for the snapshot history: the work tree is
    /// compared through a throwaway index, seeded with `from`'s paths and
    /// then given every other path as "intent to add" (name and stat data, no
    /// content), so the side repo's index is not written, no file content is
    /// stored, and no snapshot is taken. (Git stores the empty blob those
    /// entries name.) Renames are not detected, matching
    /// [`Self::patch_between`]. The patch is cut at `max_patch_bytes`, and
    /// left out past [`WORK_TREE_PATCH_MAX_LINES`] changed lines.
    pub fn work_tree_changes_since(
        &self,
        from: &SnapshotId,
        max_patch_bytes: usize,
    ) -> io::Result<WorkTreeChanges> {
        let scratch = tempfile::tempdir()?;
        let index = scratch.path().join("index");
        let run = |args: &[&str]| -> io::Result<String> {
            let subcommand = args.first().copied().unwrap_or("git");
            let mut cmd = crate::dependencies::Git::command()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git not found on PATH"))?;
            cmd.env("GIT_INDEX_FILE", &index)
                .arg("--git-dir")
                .arg(&self.git_dir)
                .arg("--work-tree")
                .arg(&self.work_tree)
                .args(args);
            let output = run_bounded_git(&mut cmd, subcommand)?;
            if !output.status.success() {
                return Err(io_other(format!(
                    "git {subcommand} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        // Seed with `from` first: on an empty index a file the snapshot holds
        // but a later `.gitignore` covers would be skipped by `add` and read
        // as deleted while it is still on disk.
        run(&["read-tree", from.as_str()])?;
        run(&["add", "--intent-to-add", "--", ":/"])?;
        let diff = |format: &str| {
            run(&[
                "diff",
                "--no-renames",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                format,
                "--end-of-options",
                from.as_str(),
                "--",
                ":/",
            ])
        };
        // `--numstat`: `added\tremoved\tpath` per line, `-` for a binary side.
        let numstat = diff("--numstat")?;
        let mut names = String::new();
        let mut changed_lines = 0_u64;
        for record in numstat.lines() {
            let mut fields = record.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            for count in [added, removed] {
                changed_lines = changed_lines.saturating_add(count.parse().unwrap_or(0));
            }
            names.push_str(path);
            names.push('\n');
        }
        let stat = diff("--stat")?;
        let (patch, patch_truncated) = if changed_lines > WORK_TREE_PATCH_MAX_LINES {
            (String::new(), true)
        } else {
            truncate_at_char_boundary(&diff("--unified=3")?, max_patch_bytes)
        };
        Ok(WorkTreeChanges {
            names,
            stat,
            patch,
            patch_truncated,
        })
    }

    /// Paths that differ between snapshots `from` and `to`, in git's order,
    /// one [`SnapshotPathChange`] each: its `status` is git's `A`/`M`/`D`/`T`
    /// letter and its line counts are `None` for a binary file. Paths come
    /// back as git stores them (`-z`), control characters included, so a
    /// caller that prints one must escape it. Both trees are read from the side repo; neither the
    /// work tree nor the index is touched. At most `limit` paths are
    /// returned; the flag says whether more differed.
    pub fn path_changes_between(
        &self,
        from: &SnapshotId,
        to: &SnapshotId,
        limit: usize,
    ) -> io::Result<(Vec<SnapshotPathChange>, bool)> {
        let run = |format: &str| -> io::Result<String> {
            let output = run_git(
                &self.git_dir,
                &self.work_tree,
                &[
                    "diff",
                    "--no-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                    format,
                    "-z",
                    "--end-of-options",
                    from.as_str(),
                    to.as_str(),
                ],
            )?;
            if !output.status.success() {
                return Err(io_other(format!(
                    "git diff {format} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        // `--numstat -z`: `added\tremoved\tpath\0`, `-` for a binary side.
        let numstat = run("--numstat")?;
        let mut counts: HashMap<String, (Option<u64>, Option<u64>)> = HashMap::new();
        for record in numstat.split('\0').filter(|record| !record.is_empty()) {
            let mut fields = record.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            counts.insert(path.to_string(), (added.parse().ok(), removed.parse().ok()));
        }
        // `--name-status -z`: `status\0path\0` pairs.
        let name_status = run("--name-status")?;
        let mut fields = name_status.split('\0').filter(|field| !field.is_empty());
        let mut changes = Vec::new();
        let mut truncated = false;
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            if changes.len() == limit {
                truncated = true;
                break;
            }
            let (added, removed) = counts.get(path).copied().unwrap_or((None, None));
            changes.push(SnapshotPathChange {
                path: path.to_string(),
                status: status.chars().next().unwrap_or('M'),
                added,
                removed,
            });
        }
        Ok((changes, truncated))
    }

    /// Whether a tree is still in this repo.
    ///
    /// The side repo keeps only the newest snapshots, while the receipt that
    /// names one is durable in the turn record: a restore point can outlive
    /// the object it names. A caller about to diff two trees asks this first,
    /// so "these restore points are gone" is answered with the pruning it is
    /// rather than as a git failure over an object nobody can bring back.
    /// IO and repository failures remain errors rather than evidence of pruning.
    pub fn has_tree(&self, id: &SnapshotId) -> io::Result<bool> {
        let spec = format!("{}^{{tree}}", id.as_str());
        let output = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ],
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(io_other(format!(
                "git tree lookup failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    /// The unified diff of one path between snapshots `from` and `to`, as
    /// `git diff` writes it — the patch behind the [`SnapshotPathChange`]
    /// [`Self::path_changes_between`] counts for the same two trees.
    ///
    /// Both trees are read from the side repo: neither the work tree nor the
    /// index is touched. The path is taken literally, so a name holding glob
    /// characters is one path rather than a pattern. Renames are not
    /// detected, matching `path_changes_between`, so a moved file reads as a
    /// deletion plus an addition.
    ///
    /// The text is cut at `max_bytes` on a char boundary; the flag says
    /// whether anything was dropped. A binary path yields git's own binary
    /// notice, and a path that does not differ yields an empty string: the
    /// caller reports what git wrote rather than inventing a patch.
    pub fn patch_between(
        &self,
        from: &SnapshotId,
        to: &SnapshotId,
        path: &str,
        max_bytes: usize,
    ) -> io::Result<(String, bool)> {
        let output = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "--literal-pathspecs",
                "diff",
                "--no-renames",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--unified=3",
                "--end-of-options",
                from.as_str(),
                to.as_str(),
                "--",
                path,
            ],
        )?;
        if !output.status.success() {
            return Err(io_other(format!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(truncate_at_char_boundary(
            &String::from_utf8_lossy(&output.stdout),
            max_bytes,
        ))
    }

    fn tree_paths(&self, treeish: &str) -> io::Result<HashSet<PathBuf>> {
        let ls = run_git(
            &self.git_dir,
            &self.work_tree,
            &[
                "ls-tree",
                "-r",
                "-z",
                "--name-only",
                "--end-of-options",
                treeish,
            ],
        )?;
        if !ls.status.success() {
            return Err(io_other(format!(
                "git ls-tree failed: {}",
                String::from_utf8_lossy(&ls.stderr).trim()
            )));
        }
        Ok(parse_nul_paths(&ls.stdout))
    }

    fn remove_paths_missing_from_target(
        &self,
        current_paths: &HashSet<PathBuf>,
        target_paths: &HashSet<PathBuf>,
    ) -> io::Result<()> {
        let removals: Vec<&PathBuf> = current_paths
            .difference(target_paths)
            .filter(|rel| is_safe_relative_path(rel))
            .collect();
        // The removal list comes from the side repo, not the live tree. A
        // directory may have been replaced by a symlink since the backup, and
        // `remove_file` would follow it and delete outside the workspace.
        // Refuse the whole removal before deleting anything.
        for rel in &removals {
            self.removal_parent_is_real(rel)?;
        }
        for rel in removals {
            // Checked again next to the delete: the tree is live.
            if !self.removal_parent_is_real(rel)? {
                continue;
            }
            let path = self.work_tree.join(rel);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.file_type().is_dir() {
                // A file-to-directory transition can make this path a
                // required parent of files just restored from the target.
                if target_paths.iter().any(|target| target.starts_with(rel)) {
                    continue;
                }
                std::fs::remove_dir(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            self.prune_empty_parent_dirs(path.parent());
        }
        Ok(())
    }

    /// Whether every directory between the work tree and `rel` is a real
    /// directory. `Ok(false)`: one is missing, so `rel` is gone too. An
    /// ancestor that is a symlink or a file is an `InvalidInput` refusal.
    fn removal_parent_is_real(&self, rel: &Path) -> io::Result<bool> {
        let mut dir = self.work_tree.clone();
        for part in rel.parent().into_iter().flat_map(Path::components) {
            dir.push(part);
            match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.file_type().is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "restore refuses to remove '{}': '{}' is no longer a directory (a symlink could lead outside the workspace)",
                            rel.display(),
                            dir.strip_prefix(&self.work_tree).unwrap_or(&dir).display()
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn prune_empty_parent_dirs(&self, mut dir: Option<&Path>) {
        while let Some(path) = dir {
            if path == self.work_tree {
                break;
            }
            if std::fs::remove_dir(path).is_err() {
                break;
            }
            dir = path.parent();
        }
    }

    /// List up to `limit` most-recent snapshots, newest first.
    pub fn list(&self, limit: usize) -> io::Result<Vec<Snapshot>> {
        // `git log -<n>` is the short form of `--max-count=<n>`; if `limit`
        // is `usize::MAX` (caller asked for "everything") we pass an empty
        // count so git defaults to no upper bound.
        let mut args: Vec<String> = vec!["log".to_string()];
        if limit < usize::MAX {
            args.push(format!("--max-count={limit}"));
        }
        args.push("--pretty=format:%H%x09%T%x09%at%x09%s".to_string());
        args.push("--no-color".to_string());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let log = run_git(&self.git_dir, &self.work_tree, &arg_refs)?;
        if !log.status.success() {
            let head = run_git(
                &self.git_dir,
                &self.work_tree,
                &["symbolic-ref", "-q", "HEAD"],
            )?;
            if head.status.success() {
                let reference = String::from_utf8_lossy(&head.stdout);
                let exists = run_git(
                    &self.git_dir,
                    &self.work_tree,
                    &["show-ref", "--verify", "--quiet", reference.trim()],
                )?;
                if exists.status.code() == Some(1) {
                    return Ok(Vec::new());
                }
            }
            return Err(io_other(format!(
                "git log failed: {}",
                String::from_utf8_lossy(&log.stderr).trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&log.stdout);
        let mut out = Vec::new();
        for line in stdout.lines() {
            let mut parts = line.splitn(4, '\t');
            let sha = parts.next().unwrap_or("").to_string();
            let tree = parts.next().unwrap_or("").to_string();
            let ts = parts
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let subject = parts.next().unwrap_or("").to_string();
            // `git log --pretty=format:%H` only emits full hex ids; skip anything
            // else rather than let it become a revision argument later.
            let (Ok(id), Ok(tree)) = (SnapshotId::parse(&sha), SnapshotId::parse(&tree)) else {
                continue;
            };
            let (session_id, label) = Self::decode_session_label(&subject);
            out.push(Snapshot {
                id,
                tree,
                label,
                timestamp: ts,
                session_id,
            });
        }
        Ok(out)
    }

    /// Drop snapshots older than `max_age`, returning the count removed.
    ///
    /// Strategy: identify keepable commits (younger than the cutoff),
    /// reset HEAD to the oldest survivor, then `git reflog expire` +
    /// `git gc --prune=now` to actually reclaim space. Cheap and avoids
    /// rewriting history when nothing has aged out.
    pub fn prune_older_than(&self, max_age: Duration) -> io::Result<usize> {
        self.with_write_lock(|| self.prune_older_than_locked(max_age))
    }

    fn prune_older_than_locked(&self, max_age: Duration) -> io::Result<usize> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| io_other(format!("clock error: {e}")))?
            .as_secs() as i64;
        let cutoff = now - max_age.as_secs() as i64;

        let snapshots = self.list(usize::MAX)?;
        if snapshots.is_empty() {
            return Ok(0);
        }

        // Snapshots are newest-first. Find the index of the first one
        // at-or-older than the cutoff — every entry from that index
        // onward is a candidate for removal. We use `<=` so a 0-second
        // retention drops same-second commits (otherwise tests calling
        // `prune_older_than(Duration::ZERO)` immediately after creating
        // a snapshot would never prune anything).
        let cut_index = snapshots.iter().position(|s| s.timestamp <= cutoff);
        let Some(cut) = cut_index else {
            return Ok(0);
        };
        let removed = snapshots.len() - cut;
        if removed == 0 {
            return Ok(0);
        }

        if cut == 0 {
            // Every snapshot is older than the cutoff: unset HEAD so the next
            // snapshot starts a fresh history and gc reclaims the old one.
            self.move_head(None, Some(snapshots[0].id.as_str()))?;
        } else {
            // Keep the newest `cut` snapshots (indices [0..cut], newest-first)
            // and drop the older tail. This MUST rebuild the survivors as a
            // fresh orphan chain, not `update-ref HEAD <oldest survivor>`:
            // the snapshots are a parent-linked commit chain with the newest
            // at HEAD, so pointing HEAD at the oldest survivor orphaned every
            // NEWER snapshot (gc then destroyed them) while keeping the very
            // snapshots we meant to remove as its ancestors — the exact
            // inverse of the intent (2026-08-04 review, reproduced).
            self.rebuild_survivor_chain(&snapshots[..cut], &snapshots[0].id)?;
        }

        self.reclaim_unreachable();
        Ok(removed)
    }

    /// Expire the reflog and gc every unreachable object now, reclaiming the
    /// space of the snapshots a prune dropped. Runs only under the snapshot
    /// write lock: an immediate prune is safe because no other writer that
    /// takes the lock can hold a new, not yet referenced object meanwhile.
    fn reclaim_unreachable(&self) {
        let _ = run_git(
            &self.git_dir,
            &self.work_tree,
            &["reflog", "expire", "--expire=now", "--all"],
        );
        let _ = run_git(
            &self.git_dir,
            &self.work_tree,
            &["gc", "--prune=now", "--quiet"],
        );
    }

    /// Run `op` holding the side repo's cross-process write lock (see
    /// [`SNAPSHOT_LOCK_FILE`]). Reentrant on one thread, so a locked
    /// operation may call another.
    fn with_write_lock<T>(&self, op: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.with_write_lock_within(SNAPSHOT_LOCK_WAIT, op)
    }

    /// [`Self::with_write_lock`] with an explicit bound on the wait. The
    /// snapshot runs on the turn and tool path, so a peer's long gc (or a
    /// stopped process holding the lock) must fail this snapshot, not stall
    /// the turn and pin a blocking thread indefinitely.
    fn with_write_lock_within<T>(
        &self,
        wait: Duration,
        op: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let held = HELD_SNAPSHOT_LOCKS.with(|held| held.borrow().contains(&self.git_dir));
        if held {
            return op();
        }
        let file = open_snapshot_lock_file(&self.git_dir.join(SNAPSHOT_LOCK_FILE))?;
        let mut lock = fd_lock::RwLock::new(file);
        let deadline = std::time::Instant::now() + wait;
        let _guard = loop {
            match lock.try_write() {
                Ok(guard) => break guard,
                Err(err) if std::time::Instant::now() >= deadline => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "snapshot side repo is busy: another session held its lock for over {}s ({err})",
                            wait.as_secs()
                        ),
                    ));
                }
                Err(_) => std::thread::sleep(SNAPSHOT_LOCK_POLL),
            }
        };
        // Declared after the guard, so it is dropped (and the thread's claim
        // released) before the file lock is.
        let _held = HeldSnapshotLock::claim(&self.git_dir);
        op()
    }

    /// Stage every tracked and untracked path the workspace exposes.
    /// `--all` means `add` + `update` + `remove`, the set `git status` shows.
    fn stage_work_tree(&self) -> io::Result<()> {
        let add = run_git(&self.git_dir, &self.work_tree, &["add", "-A"])?;
        if !add.status.success() {
            return Err(io_other(format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Rebuild `survivors` (newest-first) as a fresh orphan commit chain and
    /// point HEAD at its tip, so every snapshot NOT in `survivors` becomes
    /// unreachable for gc to reclaim. Each survivor's tree, label, session
    /// id, and author/committer timestamp are preserved, so ages do not lie
    /// after a prune (finding: `prune_keep_last_n` previously reset them to
    /// "now"). Assumes `survivors` is non-empty.
    ///
    /// `listed_head` is the HEAD the survivors were chosen from; if another
    /// writer moved HEAD since, the rebuild fails rather than orphan that
    /// writer's snapshot.
    fn rebuild_survivor_chain(
        &self,
        survivors: &[Snapshot],
        listed_head: &SnapshotId,
    ) -> io::Result<()> {
        let mut prev_sha: Option<String> = None;
        for s in survivors.iter().rev() {
            let tree = run_git(
                &self.git_dir,
                &self.work_tree,
                &["rev-parse", &format!("{}^{{tree}}", s.id.as_str())],
            )?;
            if !tree.status.success() {
                return Err(io_other(format!(
                    "rev-parse {}^{{tree}} failed: {}",
                    s.id.as_str(),
                    String::from_utf8_lossy(&tree.stderr).trim()
                )));
            }
            let tree_hash = String::from_utf8_lossy(&tree.stdout).trim().to_string();

            let mut args = vec![
                "commit-tree".to_string(),
                "-m".to_string(),
                Self::encode_session_label(&s.label, s.session_id.as_deref()),
                tree_hash,
            ];
            if let Some(ref p) = prev_sha {
                args.push("-p".to_string());
                args.push(p.clone());
            }
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let new_sha = self.commit_tree_preserving_date(&arg_refs, s.timestamp)?;
            prev_sha = Some(new_sha);
        }

        if let Some(final_sha) = prev_sha {
            self.move_head(Some(&final_sha), Some(listed_head.as_str()))?;
        }
        Ok(())
    }

    /// Point HEAD at `new` (or delete it when `None`), only if it still names
    /// `expected` (`None`: HEAD must not exist yet). The compare-and-swap
    /// means a writer that does not take the snapshot lock, such as an older
    /// build, is never silently orphaned: the move fails instead.
    fn move_head(&self, new: Option<&str>, expected: Option<&str>) -> io::Result<()> {
        let expected = expected.unwrap_or("");
        let args = match new {
            Some(new) => ["update-ref", "HEAD", new, expected],
            None => ["update-ref", "-d", "HEAD", expected],
        };
        let update = run_git(&self.git_dir, &self.work_tree, &args)?;
        if !update.status.success() {
            return Err(io_other(format!(
                "git update-ref HEAD failed: {}",
                String::from_utf8_lossy(&update.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Run a `commit-tree` invocation with the author/committer dates pinned
    /// to `timestamp` (Unix seconds), so a rebuilt survivor keeps its real
    /// age instead of stamping "now".
    fn commit_tree_preserving_date(&self, args: &[&str], timestamp: i64) -> io::Result<String> {
        let date = format!("{timestamp} +0000");
        let mut command = crate::dependencies::Git::command()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git not found on PATH"))?;
        command
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args);
        let out = run_bounded_git(&mut command, args.first().copied().unwrap_or("git"))?;
        if !out.status.success() {
            return Err(io_other(format!(
                "commit-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Prune by count: keep the newest `max_count` snapshots, plus the newest
    /// `max_count` turn boundaries (`pre-turn:` / `post-turn:`), and drop the
    /// rest.
    ///
    /// Turn boundaries are the restore points turn-scoped undo resolves, and
    /// every other kind (`tool:`, `post-tool:`, `pre-restore:`) can arrive in
    /// bursts: one turn with more file-modifying tool calls than `max_count`,
    /// or several threads sharing the workspace, would otherwise push out the
    /// running turn's own `pre-turn:` snapshot before its `post-turn:` one is
    /// taken. So the boundaries are retained by their own count, and at most
    /// `2 * max_count` snapshots survive.
    ///
    /// The survivors are rebuilt as a fresh orphan chain; each keeps its
    /// tree, label, session id and timestamp, and the dropped ones become
    /// unreachable for gc to reclaim.
    #[cfg(test)]
    pub fn prune_keep_last_n(&self, max_count: usize) -> io::Result<usize> {
        self.with_write_lock(|| self.prune_keep_last_n_locked(max_count, 1))
    }

    /// [`Self::prune_keep_last_n`] for the prune that follows every snapshot:
    /// it waits until half a window of snapshots is due to go and drops them
    /// together. Rebuilding the survivor chain costs two git processes per
    /// survivor, and doing that after every snapshot past the cap put seconds
    /// in front of every later turn's provider request. The store then holds
    /// at most half a window more than [`Self::prune_keep_last_n`] would keep.
    pub fn prune_keep_last_n_batched(&self, max_count: usize) -> io::Result<usize> {
        self.with_write_lock(|| self.prune_keep_last_n_locked(max_count, (max_count / 2).max(1)))
    }

    fn prune_keep_last_n_locked(&self, max_count: usize, min_removed: usize) -> io::Result<usize> {
        let snapshots = self.list(usize::MAX)?;
        if snapshots.len() <= max_count {
            return Ok(0);
        }
        // Newest first: keep the first `max_count` of every kind, and the
        // first `max_count` turn boundaries wherever they sit.
        let mut boundaries_kept = 0usize;
        let survivors: Vec<Snapshot> = snapshots
            .iter()
            .enumerate()
            .filter(|(index, snapshot)| {
                let boundary = is_turn_boundary_label(&snapshot.label);
                let keep = *index < max_count || (boundary && boundaries_kept < max_count);
                if keep && boundary {
                    boundaries_kept += 1;
                }
                keep
            })
            .map(|(_, snapshot)| snapshot.clone())
            .collect();
        let removed = snapshots.len() - survivors.len();
        if removed < min_removed || survivors.is_empty() {
            return Ok(0);
        }
        self.rebuild_survivor_chain(&survivors, &snapshots[0].id)?;
        self.reclaim_unreachable();
        Ok(removed)
    }

    /// Whether a snapshot of the workspace would leave `rel` out: it is
    /// excluded by the workspace's `.gitignore` files or the built-in
    /// snapshot exclusions and not already tracked. Such a path is never in
    /// any snapshot, so no restore can put it back.
    pub fn path_is_excluded(&self, rel: &Path) -> io::Result<bool> {
        let rel_str = rel
            .to_str()
            .ok_or_else(|| io_other("snapshot path must be UTF-8"))?;
        let out = run_git(
            &self.git_dir,
            &self.work_tree,
            &["check-ignore", "--quiet", "--", rel_str],
        )?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(io_other(format!(
                "git check-ignore failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    /// Drop unreachable loose objects left behind by interrupted or
    /// orphaned side-repo operations.
    pub fn prune_unreachable_objects(&self) -> io::Result<()> {
        self.with_write_lock(|| {
            let prune = run_git(&self.git_dir, &self.work_tree, &["prune", "--expire=now"])?;
            if !prune.status.success() {
                return Err(io_other(format!(
                    "git prune failed: {}",
                    String::from_utf8_lossy(&prune.stderr).trim()
                )));
            }
            Ok(())
        })
    }

    /// Return the side-repo's `.git` directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Return the work tree path.
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }
}

/// Whether `label` marks a turn boundary (`pre-turn:` / `post-turn:`), the
/// restore points [`SnapshotRepo::prune_keep_last_n`] retains by their own
/// count.
fn is_turn_boundary_label(label: &str) -> bool {
    label.starts_with("pre-turn:") || label.starts_with("post-turn:")
}

/// Which snapshots a size-pressure prune keeps (newest first): the newest
/// `keep`, plus the newest snapshot and, for every session, its newest
/// `pre-turn:` and `post-turn:` boundaries wherever they sit. Sessions and
/// sub-agents share the side repo, so protecting only the globally newest
/// boundaries let one session's snapshots push out another's running turn;
/// each session's current turn and the one before it stay restorable
/// however hard the prune has to cut.
fn size_pressure_survivors(snapshots: &[Snapshot], keep: usize) -> Vec<Snapshot> {
    let mut boundaries_seen: HashSet<(Option<&str>, bool)> = HashSet::new();
    snapshots
        .iter()
        .enumerate()
        .filter(|(index, snapshot)| {
            let kind = if snapshot.label.starts_with("pre-turn:") {
                Some(true)
            } else if snapshot.label.starts_with("post-turn:") {
                Some(false)
            } else {
                None
            };
            let newest_boundary = kind
                .is_some_and(|pre| boundaries_seen.insert((snapshot.session_id.as_deref(), pre)));
            *index == 0 || *index < keep || newest_boundary
        })
        .map(|(_, snapshot)| snapshot.clone())
        .collect()
}

/// This thread's claim on a side repo's write lock, released on drop.
struct HeldSnapshotLock(PathBuf);

impl HeldSnapshotLock {
    fn claim(git_dir: &Path) -> Self {
        HELD_SNAPSHOT_LOCKS.with(|held| held.borrow_mut().push(git_dir.to_path_buf()));
        Self(git_dir.to_path_buf())
    }
}

impl Drop for HeldSnapshotLock {
    fn drop(&mut self) {
        HELD_SNAPSHOT_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(index) = held.iter().rposition(|path| path == &self.0) {
                held.remove(index);
            }
        });
    }
}

/// Open (creating if needed) the side repo's lock file without following a
/// symlink planted in its place.
fn open_snapshot_lock_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// Keep the side repo's `info/exclude` at [`BUILTIN_EXCLUDES`]. Every open
/// runs this without the write lock, while another session may be inside
/// `git add -A` reading the file, so it is left alone when already current
/// and otherwise replaced by rename: a truncate-then-write let that reader
/// see an empty exclude list and stage `node_modules/`, `target/` and the
/// like into the shared side repo.
fn write_builtin_excludes(git_dir: &Path) -> io::Result<()> {
    let info_dir = git_dir.join("info");
    let exclude = info_dir.join("exclude");
    if std::fs::read(&exclude).is_ok_and(|current| current == BUILTIN_EXCLUDES.as_bytes()) {
        return Ok(());
    }
    std::fs::create_dir_all(&info_dir)?;
    crate::utils::write_atomic(&exclude, BUILTIN_EXCLUDES.as_bytes())
}

/// Recursively compute the total size of a directory in bytes.
fn dir_size_bytes(root: &Path) -> io::Result<u64> {
    fn walk(dir: &Path, total: &mut u64) -> io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(&path, total)?;
            } else if ft.is_file() {
                *total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
        Ok(())
    }
    let mut total: u64 = 0;
    walk(root, &mut total)?;
    Ok(total)
}

/// One prominent notice per workspace per process when the size-pressure
/// prune destroys restore points — silent loss of undo history is the S5
/// failure mode (2026-08-04 snapshot hunt). The stderr print is deliberate:
/// headless/CLI stderr is the user surface for once-per-workspace snapshot
/// warnings, matching `maybe_notify_snapshots_disabled_once` in
/// `core/turn.rs`.
#[allow(clippy::print_stderr)]
fn notify_snapshot_history_pruned_once(workspace: &Path, removed: usize) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NOTIFIED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = workspace.to_string_lossy().into_owned();
    let set = NOTIFIED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut guard) = set.lock() else {
        return;
    };
    if !guard.insert(key) {
        return;
    }
    drop(guard);
    eprint!("{}", snapshot_history_pruned_message(workspace, removed));
}

/// Build the user-visible notice for a size-pressure prune. Kept pure and
/// separate from the emit/dedup shell so the content is unit-testable.
fn snapshot_history_pruned_message(workspace: &Path, removed: usize) -> String {
    format!(
        "warning: snapshot/undo history for {} was pruned to stay under the {} MB snapshot storage cap.
  {} snapshot(s) were removed and can no longer be restored.
  The cap bounds the undo side-repo's disk use; high-churn or large workspaces hit it sooner.
",
        workspace.display(),
        MAX_SNAPSHOT_SIZE_MB,
        removed
    )
}

fn cleanup_stale_pack_temps(git_dir: &Path, stale_age: Duration) -> io::Result<usize> {
    let pack_dir = git_dir.join("objects").join("pack");
    if !pack_dir.exists() {
        return Ok(0);
    }
    cleanup_stale_pack_temps_in(&pack_dir, stale_age, SystemTime::now())
}

fn cleanup_stale_pack_temps_in(
    pack_dir: &Path,
    stale_age: Duration,
    now: SystemTime,
) -> io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(pack_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("tmp_pack_") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }

        let metadata = entry.metadata()?;
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < stale_age {
            continue;
        }

        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(removed)
}

// Generous budget: `git add -A` on a large workspace is legitimately slow,
// but a wedged git (stalled NFS/FUSE, hung hook) must not block the turn
// pipeline forever — every caller treats a snapshot error as
// snapshot-disabled-with-warning and proceeds without the git data. Tests
// use a tighter budget so a regression that deadlocks a child on its own
// output fails in seconds instead of hanging for the full window.
#[cfg(not(test))]
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(test)]
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace granted to the pipe readers after git has exited. A clean git
/// closes its own write ends, so EOF is already waiting; only a grandchild
/// that inherited the pipes (a post-checkout hook, a `git gc` pack worker)
/// can hold them past exit, and it must not hold the turn pipeline either.
const GIT_PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Run a pre-configured git command under [`GIT_COMMAND_TIMEOUT`] with both
/// pipes drained while the child runs (the same concurrent drain
/// `Command::output` performs). Every git invocation in this module goes
/// through here, so a wedged git — stalled NFS/FUSE, hung hook — degrades
/// the snapshot with an error instead of hanging the turn pipeline.
/// (`delta.rs`'s read-view git commands are not routed here yet; that is a
/// known follow-up.)
///
/// The timeout path kills the child and reaps it on a detached thread: on a
/// hard-wedged mount git can sit in uninterruptible kernel I/O where even
/// SIGKILL is deferred, and a blocking `wait()` would hang the pipeline
/// exactly like the wedged git would. Killing without git's own cleanup can
/// leave a fresh `index.lock` behind; later snapshots then fail fast on the
/// lock with an error naming it.
fn run_bounded_git(cmd: &mut std::process::Command, subcommand: &str) -> io::Result<Output> {
    run_bounded_git_with_timeout(cmd, subcommand, GIT_COMMAND_TIMEOUT)
}

fn run_bounded_git_with_timeout(
    cmd: &mut std::process::Command,
    subcommand: &str,
    timeout: Duration,
) -> io::Result<Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    // Drain both pipes while waiting: the restore path's `ls-tree -r` and
    // the diff commands emit output that grows with workspace size, and a
    // child blocked on a full pipe buffer never exits — it would turn every
    // such call into a guaranteed timeout. Readers stream into shared
    // buffers, and the completion channel lets the collect below bound how
    // long a grandchild-held pipe may hold the call after git has exited.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let buf = std::sync::Arc::clone(&stdout_buf);
        let done_tx = done_tx.clone();
        std::thread::spawn(move || {
            if let Some(mut reader) = stdout_pipe {
                read_pipe_to_buffer(&mut reader, &buf);
            }
            let _ = done_tx.send(());
        });
    }
    {
        let buf = std::sync::Arc::clone(&stderr_buf);
        let done_tx = done_tx.clone();
        std::thread::spawn(move || {
            if let Some(mut reader) = stderr_pipe {
                read_pipe_to_buffer(&mut reader, &buf);
            }
            let _ = done_tx.send(());
        });
    }
    drop(done_tx);

    let Some(status) = child.wait_timeout(timeout)? else {
        let _ = child.kill();
        // Reap off the pipeline thread (see the doc comment): the kernel may
        // not deliver the kill until an uninterruptible syscall returns.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("git {subcommand} timed out after {}s", timeout.as_secs()),
        ));
    };
    // Wait for the readers up to the grace, then return what was captured. A
    // clean git already closed its write ends, so both readers deliver
    // immediately; the grace only covers a pipe still held open by an
    // inherited copy (a daemonizing hook). On expiry the captured output is
    // returned with a note on stderr instead of silently truncating; the
    // reader itself ends whenever whatever holds the pipe exits, holding
    // only a pipe read — never the turn pipeline.
    let mut partial = false;
    let mut drained = 0;
    while drained < 2 {
        match done_rx.recv_timeout(GIT_PIPE_DRAIN_GRACE) {
            Ok(()) => drained += 1,
            Err(_) => {
                partial = true;
                break;
            }
        }
    }
    let stdout = stdout_buf.lock().map(|buf| buf.clone()).unwrap_or_default();
    let mut stderr = stderr_buf.lock().map(|buf| buf.clone()).unwrap_or_default();
    if partial {
        if !stderr.is_empty() && stderr.last() != Some(&b'\n') {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(
            b"[codewhale] git output pipes did not close after git exited \
              (kept open by a hook or subprocess?); captured output may be partial\n",
        );
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Read a child's pipe to end into a shared buffer, tolerating interruption.
fn read_pipe_to_buffer(reader: &mut impl io::Read, buf: &std::sync::Mutex<Vec<u8>>) {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                if let Ok(mut buf) = buf.lock() {
                    buf.extend_from_slice(&chunk[..read]);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

fn run_git(git_dir: &Path, work_tree: &Path, args: &[&str]) -> io::Result<Output> {
    let mut cmd = crate::dependencies::Git::command()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git not found on PATH"))?;
    cmd.arg("--git-dir")
        .arg(git_dir)
        .arg("--work-tree")
        .arg(work_tree)
        .args(args);
    run_bounded_git(&mut cmd, args.first().copied().unwrap_or("git"))
}

fn git_diff_matches(output: Output) -> io::Result<bool> {
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(io_other(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

fn io_other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// `text` whole, or the longest prefix that fits in `max_bytes` and ends on a
/// char boundary; the flag says which of the two the caller got.
pub(crate) fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// Walk `workspace` and accumulate file sizes, returning `Ok(total)`
/// when the workspace fits under `cap_bytes` and `Err(gate)` naming the
/// bound that tripped. Honors `.gitignore` — whether or not the
/// workspace is itself a git repo, matching the `git add -A` that the
/// snapshot commit actually runs against this work tree — and the
/// snapshot-specific skip list above, so the measured size reflects
/// what would land in a snapshot commit rather than the raw `du -sh`
/// total.
///
/// The walk is bounded by both `cap_bytes` and `max_entries`, and the
/// two bounds are reported separately because they have different
/// recoveries. A `cap_bytes` of `0` disables the byte cap entirely (so
/// config can opt out) but not the entry bound.
///
/// Production passes [`SIZE_WALK_MAX_ENTRIES`] for `max_entries`; it is
/// a parameter only so the entry bound is reachable in a test without
/// creating 200,000 inodes. It must not be threaded up through
/// [`SnapshotRepo::open_or_init_with_cap`]: [`WorkspaceGate::describe`]
/// interpolates the constant into the user-facing message, so a weaker
/// injected bound would report a number that did not trip.
pub fn estimate_workspace_size_bounded(
    workspace: &Path,
    cap_bytes: u64,
    max_entries: usize,
) -> Result<u64, WorkspaceGate> {
    use ignore::WalkBuilder;
    let mut total: u64 = 0;
    let mut entries: usize = 0;
    let skip: HashSet<&'static str> = SIZE_WALK_SKIP_DIRS.iter().copied().collect();
    let walker = WalkBuilder::new(workspace)
        .hidden(false)
        // `ignore` defaults to `require_git(true)`, which silently disables
        // every gitignore rule when the workspace is not inside a git repo.
        // The snapshot's own `git add -A` honors `.gitignore` regardless, so
        // without this the estimator over-counts a non-git workspace and can
        // refuse it while offering a `.gitignore` remedy that cannot work.
        .require_git(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            // Skip the well-known build-output directories at any depth.
            // The `ignore` crate calls `filter_entry` once per dir/file;
            // returning `false` here prunes the whole subtree.
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !skip.contains(name))
        })
        .build();
    for entry in walker.flatten() {
        entries += 1;
        if entries > max_entries {
            return Err(WorkspaceGate::TooManyEntries);
        }
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            total = total.saturating_add(meta.len());
            if cap_bytes > 0 && total > cap_bytes {
                return Err(WorkspaceGate::TooLarge);
            }
        }
    }
    Ok(total)
}

pub(crate) fn unsafe_workspace_snapshot_reason(
    workspace: &Path,
    home: Option<&Path>,
) -> Option<&'static str> {
    let workspace = normalize_path_for_safety(workspace);
    if is_filesystem_root(&workspace) {
        return Some("filesystem root");
    }

    if is_home_directory(&workspace, home) {
        return Some("home directory");
    }

    let home = home.map(normalize_path_for_safety)?;
    if workspace.parent() == Some(home.as_path()) {
        let name = workspace.file_name().and_then(|name| name.to_str());
        if matches!(
            name,
            Some(
                "Desktop" | "Documents" | "Downloads" | "Library" | "Movies" | "Music" | "Pictures"
            )
        ) {
            return Some("home collection directory");
        }
    }

    None
}

fn normalize_path_for_safety(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn is_home_directory(work_tree: &Path, home: Option<&Path>) -> bool {
    let Some(home) = home else {
        return false;
    };

    let home_canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    work_tree == home_canonical
}

fn parse_nul_paths(bytes: &[u8]) -> HashSet<PathBuf> {
    bytes
        .split(|b| *b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| PathBuf::from(String::from_utf8_lossy(chunk).into_owned()))
        .collect()
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Whether one path component names the repository metadata directory as the
/// filesystem resolves it, not just as spelled: `.git` in any letter case
/// (macOS and Windows default to case-insensitive names) and, on Windows,
/// with the trailing dots/spaces or `:stream` suffix it drops and the `GIT~N`
/// short-name alias. The one `.git` rule for workspace file routes, file
/// restore, displayed workspace paths, the write carve-out and sub-agent
/// deliverables.
pub fn is_git_metadata_name(name: &std::ffi::OsStr) -> bool {
    git_metadata_name(&name.to_string_lossy(), cfg!(windows))
}

fn git_metadata_name(name: &str, windows: bool) -> bool {
    let name = if windows {
        name.split(':')
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', ' '])
    } else {
        name
    };
    name.eq_ignore_ascii_case(".git")
        || (windows
            && name.len() > 4
            && name
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("git~"))
            && name[4..].bytes().all(|byte| byte.is_ascii_digit()))
}

/// Normalize a caller-supplied path into a safe workspace-relative path.
///
/// Accepts either a workspace-relative path or an absolute path inside
/// `workspace`. Returns `None` when the result is not a plain relative path —
/// absolute, empty, containing `..`, or pointing outside the workspace. Every
/// file-scoped restore path passes through here, so a caller never gets to
/// name a path the snapshot repo would resolve outside the work tree.
///
/// The name is literal: leading or trailing spaces, brackets and glob
/// characters are filename bytes, never trimmed and never patterns. Git is
/// invoked with `--literal-pathspecs` for every file-scoped operation.
pub fn workspace_relative_path(workspace: &Path, raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let candidate = Path::new(raw);
    let rel = if candidate.is_absolute() {
        candidate.strip_prefix(workspace).ok()?.to_path_buf()
    } else {
        candidate.to_path_buf()
    };
    is_safe_relative_path(&rel).then_some(rel)
}

/// Whether some existing ancestor of `rel` (within `root`) is not a
/// directory: restoring `rel` would then turn a file into a directory.
fn ancestor_is_not_a_directory(root: &Path, rel: &Path) -> bool {
    rel.ancestors()
        .skip(1)
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .any(|ancestor| {
            std::fs::symlink_metadata(root.join(ancestor)).is_ok_and(|metadata| !metadata.is_dir())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::lock_test_env;
    use std::fs::{File, FileTimes};
    use tempfile::tempdir;

    #[test]
    fn git_metadata_name_covers_case_and_windows_aliases() {
        for windows in [false, true] {
            for name in [".git", ".GIT", ".Git", ".gIt"] {
                assert!(git_metadata_name(name, windows), "{name} windows={windows}");
            }
            for name in [
                ".github",
                ".gitignore",
                "a.git",
                "git",
                "",
                "git~",
                "GIT~1a",
            ] {
                assert!(
                    !git_metadata_name(name, windows),
                    "{name} windows={windows}"
                );
            }
        }
        // Windows drops trailing dots/spaces and `:stream` suffixes, and
        // `GIT~N` is the 8.3 short name of `.git`; elsewhere these are
        // ordinary names.
        for name in [
            ".git.",
            ".git ",
            ".GIT. .",
            ".git::$INDEX_ALLOCATION",
            ".git:stream",
            "GIT~1",
            "git~12",
        ] {
            assert!(git_metadata_name(name, true), "{name}");
            assert!(!git_metadata_name(name, false), "{name}");
        }
    }

    #[test]
    fn snapshot_id_parse_accepts_only_full_hex_object_ids() {
        let sha1 = "0123456789abcdefABCDEF0123456789abcdef01";
        let sha256 = "a".repeat(64);
        assert_eq!(SnapshotId::parse(sha1).expect("sha1").as_str(), sha1);
        assert!(SnapshotId::parse(&sha256).is_ok());
        for bad in [
            "",
            "HEAD",
            "abc123",
            "--output=/tmp/x",
            "-0123456789abcdef0123456789abcdef0123456",
            "0123456789abcdef0123456789abcdef0123456g",
            "0123456789abcdef0123456789abcdef01234567~1",
            "0123456789abcdef0123456789abcdef012345678",
        ] {
            let err = SnapshotId::parse(bad).expect_err(bad);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }

    /// Holds the home directory pinned to a tempdir for the lifetime of a test. Also
    /// owns the process-wide env-var mutex so tests across modules
    /// don't trample each other's home env vars.
    pub(super) struct ScopedHome {
        _vars: Vec<crate::test_support::EnvVarGuard>,
        _guard: crate::test_support::TestEnvLock,
    }
    pub(super) fn scoped_home(home: &Path) -> ScopedHome {
        use crate::test_support::EnvVarGuard;
        let guard = lock_test_env();
        ScopedHome {
            _vars: vec![
                EnvVarGuard::set("HOME", home),
                EnvVarGuard::set("USERPROFILE", home),
                EnvVarGuard::remove("HOMEDRIVE"),
                EnvVarGuard::remove("HOMEPATH"),
                EnvVarGuard::set("CODEWHALE_HOME", home.join(".codewhale")),
            ],
            _guard: guard,
        }
    }

    /// Build a side-repo inside the test's selected profile. Return its
    /// environment guard so reads and writes stay isolated for the whole test.
    fn make_repo(tmp: &Path) -> (SnapshotRepo, ScopedHome) {
        let workspace = tmp.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let guard = scoped_home(tmp);
        let repo = SnapshotRepo::open_or_init(&workspace).expect("open_or_init");
        (repo, guard)
    }

    #[test]
    fn snapshot_creates_commit_in_side_repo_only() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha").unwrap();

        let id = repo.snapshot("pre-turn:1").expect("snapshot");
        assert_eq!(id.as_str().len(), 40);

        let list = repo.list(10).expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "pre-turn:1");

        // The user's workspace must NOT have a real `.git` because we
        // never created one in their workspace — only in the side dir.
        assert!(!repo.work_tree().join(".git").exists());
    }

    /// B2: a side repo whose HEAD names a missing commit made every snapshot
    /// fail on `commit-tree -p` ("is not a valid object"), so /undo died
    /// silently. The broken ref is reset and snapshots resume.
    #[test]
    fn broken_head_is_repaired_and_snapshots_resume() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha").unwrap();
        repo.snapshot("pre-turn:1").expect("first snapshot");
        assert!(!repo.repair_broken_head().expect("healthy head"));

        repo.point_head_at_missing_commit_for_test();
        assert!(
            repo.list(10).is_err() || repo.list(10).unwrap().is_empty(),
            "a broken head cannot list its history"
        );

        // With a reflog, the last commit that still exists is restored and
        // the restore points before the break survive.
        assert!(
            !repo.repair_broken_head().expect("recover"),
            "recovered from the reflog, not restarted"
        );
        let list = repo.list(10).expect("list after recovery");
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].label, "pre-turn:1");

        // Without one, the broken ref is deleted and history restarts.
        repo.point_head_at_missing_commit_for_test();
        std::fs::remove_dir_all(repo.git_dir.join("logs")).expect("drop reflogs");
        assert!(repo.repair_broken_head().expect("repair"), "repaired once");
        assert!(!repo.repair_broken_head().expect("idempotent"));
        std::fs::write(repo.work_tree().join("a.txt"), b"beta").unwrap();
        repo.snapshot("pre-turn:2").expect("snapshots resume");
        let list = repo.list(10).expect("list after repair");
        assert_eq!(list.len(), 1, "history restarts at the repair: {list:?}");
        assert_eq!(list[0].label, "pre-turn:2");

        // The snapshot path repairs on its own too, for callers that never
        // asked.
        repo.point_head_at_missing_commit_for_test();
        repo.snapshot("pre-turn:3")
            .expect("snapshot repairs on its own");
    }

    #[test]
    fn open_existing_is_read_only_and_does_not_initialize() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let _home = scoped_home(tmp.path());

        let before = SnapshotRepo::open_existing(&workspace).expect("open existing");
        assert!(before.is_none());
        assert!(
            !snapshot_git_dir(&workspace)
                .expect("snapshot path")
                .exists(),
            "read-only open must not create the side repo"
        );

        let repo = SnapshotRepo::open_or_init(&workspace).expect("open_or_init");
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha").unwrap();
        repo.snapshot("pre-turn:1").expect("snapshot");

        let after = SnapshotRepo::open_existing(&workspace).expect("open existing");
        assert!(after.is_some());
    }

    #[test]
    fn restore_reverts_workspace_files() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let f = repo.work_tree().join("file.txt");

        std::fs::write(&f, b"original").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::write(&f, b"clobbered").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        repo.restore(&id).expect("restore");
        let after = std::fs::read_to_string(&f).unwrap();
        assert_eq!(after, "original");
    }

    #[test]
    fn restore_removes_files_added_after_target_snapshot() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let original = repo.work_tree().join("original.txt");
        let added = repo.work_tree().join("added.txt");

        std::fs::write(&original, b"original").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::write(&added, b"new file").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        repo.restore(&id).expect("restore");
        assert!(original.exists());
        assert!(!added.exists(), "restore must remove tracked added files");
    }

    /// The first snapshot of an empty project holds the empty tree. Restoring
    /// it used to fail (`pathspec ':/' did not match`) before removing
    /// anything, so every file created since stayed.
    #[test]
    fn restore_to_an_empty_snapshot_removes_the_files_created_since() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let empty = repo.snapshot("pre-turn:1").expect("empty snapshot");
        let created = repo.work_tree().join("src").join("main.rs");
        std::fs::create_dir_all(created.parent().unwrap()).unwrap();
        std::fs::write(&created, b"fn main() {}").unwrap();
        std::fs::write(repo.work_tree().join("notes.txt"), b"notes").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        repo.restore(&empty).expect("restore to the empty tree");
        assert!(!created.exists(), "restore must remove files created since");
        assert!(!repo.work_tree().join("notes.txt").exists());
        assert!(
            !repo.work_tree().join("src").exists(),
            "directories the removal emptied go too"
        );
    }

    #[test]
    fn restore_keeps_current_files_when_safety_snapshot_fails() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let empty = repo.snapshot("pre-turn:1").unwrap();
        let file = repo.work_tree().join("new-work.txt");
        std::fs::write(&file, b"only copy of new work").unwrap();
        repo.snapshot("post-turn:1").unwrap();
        std::fs::write(repo.git_dir().join("index.lock"), b"").unwrap();

        let error = repo.restore(&empty).expect_err("backup is mandatory");
        assert!(
            error.to_string().contains("safety snapshot failed"),
            "{error}"
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"only copy of new work");
    }

    #[test]
    fn restore_rolls_back_files_after_a_partial_checkout_error() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let first = repo.work_tree().join("a.txt");
        let last_dir = repo.work_tree().join("z");
        let last = last_dir.join("last.txt");
        std::fs::create_dir(&last_dir).unwrap();
        std::fs::write(&first, b"old first").unwrap();
        std::fs::write(&last, b"old last").unwrap();
        let target = repo.snapshot("pre-turn:1").unwrap();
        let object = run_git(
            repo.git_dir(),
            repo.work_tree(),
            &["rev-parse", &format!("{}:z/last.txt", target.as_str())],
        )
        .unwrap();
        assert!(object.status.success());
        let object = String::from_utf8(object.stdout).unwrap();
        let object = object.trim();
        std::fs::write(&first, b"new first").unwrap();
        std::fs::remove_file(&last).unwrap();
        let before = repo.snapshot("before-restore-proof").unwrap();
        // A missing target-only blob makes Git fail after it writes a.txt.
        // The backup needs neither that blob nor z/last.txt for recovery.
        std::fs::remove_file(
            repo.git_dir()
                .join("objects")
                .join(&object[..2])
                .join(&object[2..]),
        )
        .unwrap();

        // Prove the failure fixture really permits a partial overwrite.
        let partial = run_git(
            repo.git_dir(),
            repo.work_tree(),
            &["checkout", target.as_str(), "--", ":/"],
        )
        .unwrap();
        assert!(!partial.status.success());
        assert_eq!(std::fs::read(&first).unwrap(), b"old first");
        let reset = run_git(
            repo.git_dir(),
            repo.work_tree(),
            &["checkout", before.as_str(), "--", ":/"],
        )
        .unwrap();
        assert!(reset.status.success());
        assert_eq!(std::fs::read(&first).unwrap(), b"new first");

        let error = repo.restore(&target).expect_err("target blob is missing");
        assert!(error.to_string().contains("unable to read"), "{error}");
        assert!(
            error
                .to_string()
                .contains("previous snapshot files were restored"),
            "{error}"
        );
        assert!(error.to_string().contains("safety snapshot"), "{error}");
        assert_eq!(std::fs::read(&first).unwrap(), b"new first");
        assert!(!last.exists());
    }

    #[test]
    fn restore_refuses_file_directory_transitions_without_changing_files() {
        for target_is_dir in [false, true] {
            let tmp = tempdir().unwrap();
            let (repo, _home) = make_repo(tmp.path());
            let path = repo.work_tree().join("a");
            let child = path.join("child");
            if target_is_dir {
                std::fs::create_dir(&path).unwrap();
                std::fs::write(&child, b"old child").unwrap();
            } else {
                std::fs::write(&path, b"old file").unwrap();
            }
            let target = repo.snapshot("pre-turn:1").unwrap();
            if target_is_dir {
                std::fs::remove_file(&child).unwrap();
                std::fs::remove_dir(&path).unwrap();
                std::fs::write(&path, b"new file").unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
                std::fs::write(&child, b"new child").unwrap();
            }

            let error = repo
                .restore(&target)
                .expect_err("type transition is refused");
            assert!(
                error.to_string().contains("file/directory transition"),
                "{error}"
            );
            if target_is_dir {
                assert_eq!(std::fs::read(&path).unwrap(), b"new file");
            } else {
                assert_eq!(std::fs::read(&child).unwrap(), b"new child");
            }
        }
    }

    #[test]
    fn a_path_under_a_live_file_needs_a_transition() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), b"file").unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        assert!(super::ancestor_is_not_a_directory(
            tmp.path(),
            Path::new("a/child")
        ));
        assert!(!super::ancestor_is_not_a_directory(
            tmp.path(),
            Path::new("d/child")
        ));
        assert!(!super::ancestor_is_not_a_directory(
            tmp.path(),
            Path::new("missing/child")
        ));
        assert!(!super::ancestor_is_not_a_directory(
            tmp.path(),
            Path::new("a")
        ));
    }

    /// A failed safety snapshot refuses before a symlinked parent could be
    /// followed, even when restoring to an empty target skips checkout.
    #[cfg(unix)]
    #[test]
    fn restore_refuses_to_remove_through_a_symlinked_parent() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let empty = repo.snapshot("pre-turn:1").expect("empty snapshot");
        let src = repo.work_tree().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("victim"), b"inside").unwrap();
        repo.snapshot("post-turn:1")
            .expect("snapshot holding src/victim");

        let outside = tempdir().unwrap();
        let sentinel = outside.path().join("victim");
        std::fs::write(&sentinel, b"outside").unwrap();
        std::fs::remove_dir_all(&src).unwrap();
        std::os::unix::fs::symlink(outside.path(), &src).unwrap();
        // A stale index lock makes the mandatory safety snapshot fail.
        std::fs::write(repo.git_dir().join("index.lock"), b"").unwrap();

        let err = repo
            .restore(&empty)
            .expect_err("a removal through a symlinked parent is refused");
        assert!(err.to_string().contains("safety snapshot failed"), "{err}");
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"outside",
            "the file outside the workspace survives"
        );
        assert!(
            std::fs::symlink_metadata(&src)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// Every session, turn and sub-agent in a workspace shares its side repo.
    /// A snapshot must wait while another writer holds the repo's write lock
    /// instead of racing its index, HEAD and gc.
    #[test]
    fn snapshot_waits_for_the_side_repo_write_lock() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("f.txt"), b"v0").unwrap();
        repo.snapshot("pre-turn:1").expect("first snapshot");

        let lock_path = repo.git_dir().join(SNAPSHOT_LOCK_FILE);
        let mut peer = fd_lock::RwLock::new(open_snapshot_lock_file(&lock_path).unwrap());
        let held = peer.write().expect("peer holds the write lock");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let git_dir = repo.git_dir().to_path_buf();
        let work_tree = repo.work_tree().to_path_buf();
        let writer = std::thread::spawn(move || {
            let repo = SnapshotRepo { git_dir, work_tree };
            std::fs::write(repo.work_tree().join("f.txt"), b"v1").unwrap();
            let taken = repo.snapshot("post-turn:1");
            let _ = done_tx.send(());
            taken
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "a snapshot must not run while another writer holds the lock"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("snapshot proceeds once the lock is released");
        writer
            .join()
            .expect("writer thread")
            .expect("snapshot after release");
        assert_eq!(repo.list(usize::MAX).unwrap().len(), 2);
    }

    /// Two writers sharing one side repo, each snapshotting and pruning, must
    /// all succeed and leave a history whose every snapshot still restores.
    #[test]
    fn concurrent_snapshot_and_prune_keep_the_side_repo_consistent() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("seed.txt"), b"seed").unwrap();
        repo.snapshot("pre-turn:0").expect("seed snapshot");
        let git_dir = repo.git_dir().to_path_buf();
        let work_tree = repo.work_tree().to_path_buf();
        let writers: Vec<_> = (0..2)
            .map(|writer| {
                let git_dir = git_dir.clone();
                let work_tree = work_tree.clone();
                std::thread::spawn(move || -> io::Result<()> {
                    let repo = SnapshotRepo { git_dir, work_tree };
                    for turn in 0..6 {
                        std::fs::write(
                            repo.work_tree().join(format!("w{writer}.txt")),
                            format!("{writer}-{turn}"),
                        )?;
                        repo.snapshot(&format!("post-turn:{writer}-{turn}"))?;
                        repo.prune_keep_last_n(2)?;
                    }
                    Ok(())
                })
            })
            .collect();
        for writer in writers {
            writer
                .join()
                .expect("writer thread")
                .expect("writer ran clean");
        }
        assert!(!repo.repair_broken_head().expect("head check"));
        let history = repo.list(usize::MAX).expect("history lists");
        assert!(!history.is_empty());
        for snapshot in &history {
            assert!(
                repo.is_commit(snapshot.id.as_str()).unwrap(),
                "listed snapshot {} must exist",
                snapshot.id.as_str()
            );
        }
    }

    /// A peer stuck holding the lock (a long gc, a stopped process) used to
    /// block the snapshot, and the turn waiting on it, with no end. The wait
    /// is bounded and ends in an error the turn reports.
    #[test]
    fn side_repo_lock_wait_is_bounded() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let lock_path = repo.git_dir().join(SNAPSHOT_LOCK_FILE);
        let mut peer = fd_lock::RwLock::new(open_snapshot_lock_file(&lock_path).unwrap());
        let _held = peer.write().expect("peer holds the write lock");

        let started = std::time::Instant::now();
        let err = repo
            .with_write_lock_within(Duration::from_millis(200), || Ok(()))
            .expect_err("a held lock times out");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(err.to_string().contains("busy"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the wait ends near its bound"
        );
    }

    /// First-time init (git init plus the pinned identity) runs under the
    /// write lock, so two sessions opening a new workspace do not race it; a
    /// `.git` holding only a peer's lock file is initialized, not trusted.
    #[test]
    fn first_init_waits_for_the_side_repo_write_lock() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let git_dir = repo.git_dir().to_path_buf();
        let workspace = repo.work_tree().to_path_buf();
        drop(repo);
        std::fs::remove_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        let mut peer = fd_lock::RwLock::new(
            open_snapshot_lock_file(&git_dir.join(SNAPSHOT_LOCK_FILE)).unwrap(),
        );
        let held = peer.write().expect("peer holds the write lock");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let env_ticket = crate::test_support::env_scope_ticket();
        let opener = std::thread::spawn(move || {
            let _membership = crate::test_support::join_env_scope(env_ticket);
            let opened = SnapshotRepo::open_or_init(&workspace);
            let _ = done_tx.send(());
            opened
        });
        assert!(
            done_rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "init must not run while another writer holds the lock"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("init proceeds once the lock is released");
        let repo = opener.join().expect("opener thread").expect("open_or_init");
        assert!(
            repo.git_dir().join("HEAD").exists(),
            "the repo was initialized"
        );
        std::fs::write(repo.work_tree().join("f.txt"), b"v").unwrap();
        repo.snapshot("pre-turn:1").expect("the new repo snapshots");
    }

    /// Opening runs on every snapshot without the write lock while a peer may
    /// be staging. It used to truncate and rewrite `info/exclude` each time,
    /// so the peer's `git add -A` could read an empty exclude list.
    #[test]
    fn opening_leaves_a_current_exclude_file_untouched() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let exclude = repo.git_dir().join("info").join("exclude");
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        File::options()
            .write(true)
            .open(&exclude)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();

        SnapshotRepo::open_or_init(repo.work_tree()).expect("reopen");
        assert_eq!(
            std::fs::metadata(&exclude).unwrap().modified().unwrap(),
            old,
            "a current exclude file is not rewritten"
        );

        std::fs::write(&exclude, b"stale\n").unwrap();
        SnapshotRepo::open_or_init(repo.work_tree()).expect("reopen");
        assert_eq!(
            std::fs::read_to_string(&exclude).unwrap(),
            BUILTIN_EXCLUDES,
            "a stale exclude file is replaced"
        );
    }

    /// HEAD moves only from the value the writer read. A writer that does not
    /// take the lock (an older build) must not be orphaned by a later move.
    #[test]
    fn head_moves_only_from_the_expected_commit() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("f.txt"), b"v1").unwrap();
        let first = repo.snapshot("pre-turn:1").expect("snapshot 1");
        std::fs::write(repo.work_tree().join("f.txt"), b"v2").unwrap();
        let second = repo.snapshot("post-turn:1").expect("snapshot 2");

        repo.move_head(Some(first.as_str()), Some(first.as_str()))
            .expect_err("HEAD no longer names the expected commit");
        repo.move_head(Some(first.as_str()), None)
            .expect_err("HEAD exists, so an unborn expectation fails");
        repo.move_head(None, Some(first.as_str()))
            .expect_err("deleting HEAD also checks the expected commit");
        assert_eq!(repo.list(1).unwrap()[0].id, second, "HEAD is unchanged");

        repo.move_head(Some(first.as_str()), Some(second.as_str()))
            .expect("the expected commit moves");
        assert_eq!(repo.list(1).unwrap()[0].id, first);
    }

    /// HEAD repair looks up a missing commit, then moves HEAD off it. A peer
    /// that published a snapshot in between used to be overwritten by the
    /// reflog reset or deleted by the fresh-history fallback; both moves now
    /// require HEAD to still name the missing commit.
    #[test]
    fn head_repair_leaves_a_peers_new_snapshot_alone() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("f.txt"), b"v1").unwrap();
        repo.snapshot("pre-turn:1").expect("snapshot 1");
        std::fs::write(repo.work_tree().join("f.txt"), b"v2").unwrap();
        let peer = repo.snapshot("post-turn:peer").expect("peer snapshot");
        let missing = "1111111111111111111111111111111111111111";

        repo.reset_missing_head(missing)
            .expect_err("the reflog reset checks HEAD still names the missing commit");
        assert_eq!(repo.list(1).unwrap()[0].id, peer, "HEAD is unchanged");

        std::fs::remove_dir_all(repo.git_dir().join("logs")).unwrap();
        repo.reset_missing_head(missing)
            .expect_err("the fresh-history delete checks it too");
        assert_eq!(
            repo.list(1).unwrap()[0].id,
            peer,
            "the peer's snapshot stays"
        );
    }

    /// A prune rebuilds the survivors it listed. If another writer committed
    /// after that listing, the rebuild used to overwrite HEAD and orphan the
    /// new snapshot for gc; now it fails and the snapshot stays.
    #[test]
    fn survivor_rebuild_refuses_when_another_writer_moved_head() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        for i in 0..3 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot(&format!("tool:{i}")).expect("snapshot");
        }
        let listed = repo.list(usize::MAX).unwrap();
        std::fs::write(repo.work_tree().join("f.txt"), b"peer").unwrap();
        let peer = repo.snapshot("post-turn:peer").expect("peer snapshot");

        repo.rebuild_survivor_chain(&listed[..1], &listed[0].id)
            .expect_err("HEAD moved since the listing");
        let history = repo.list(usize::MAX).unwrap();
        assert_eq!(history.len(), 4, "nothing was dropped");
        assert_eq!(history[0].id, peer, "the peer's snapshot is still HEAD");
    }

    /// An index naming objects that no longer exist (an interrupted or racing
    /// gc) failed every later `write-tree`, because `add -A` does not rehash
    /// files whose stat data is unchanged. The index is rebuilt once instead.
    #[test]
    fn snapshot_rebuilds_an_index_naming_missing_objects() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let file = repo.work_tree().join("f.txt");
        std::fs::write(&file, b"content").unwrap();
        // An mtime well before the index keeps git from treating the entry as
        // racily clean and rehashing it, which would hide the broken index.
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3600)))
            .unwrap();
        let taken = repo
            .take_snapshot("pre-turn:1", None)
            .expect("first snapshot");
        let blob = git_output(&repo, &["rev-parse", "HEAD:f.txt"]);
        for oid in [blob.as_str(), taken.tree.as_str()] {
            let object = repo
                .git_dir()
                .join("objects")
                .join(&oid[..2])
                .join(&oid[2..]);
            std::fs::remove_file(&object).expect("loose object removed");
        }

        let again = repo
            .take_snapshot("post-turn:1", None)
            .expect("the snapshot rebuilds the index and succeeds");
        assert_eq!(
            git_output(
                &repo,
                &["cat-file", "-p", &format!("{}:f.txt", again.id.as_str())]
            ),
            "content"
        );
    }

    fn git_output(repo: &SnapshotRepo, args: &[&str]) -> String {
        let out = run_git(repo.git_dir(), repo.work_tree(), args).unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn restore_paths_leaves_unrelated_files_alone() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let wanted = repo.work_tree().join("wanted.txt");
        let unrelated = repo.work_tree().join("unrelated.txt");

        std::fs::write(&wanted, b"original").unwrap();
        std::fs::write(&unrelated, b"original").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::write(&wanted, b"clobbered").unwrap();
        std::fs::write(&unrelated, b"also clobbered").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        let outcomes = repo
            .restore_paths(&id, &[PathBuf::from("wanted.txt")])
            .expect("scoped restore");

        assert_eq!(std::fs::read_to_string(&wanted).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "also clobbered",
            "a file-scoped restore must not touch a path it was not given"
        );
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].path, PathBuf::from("wanted.txt"));
        assert_eq!(outcomes[0].action, PathRestoreAction::Modified);
    }

    #[test]
    fn only_restore_paths_is_safe_for_a_single_file_action() {
        // Characterizes the difference the per-file Revert control depends on.
        // `restore()` is what the TUI's `patch_undo()` and the runtime's
        // `patch-undo` endpoint both call. It is scoped in *snapshot selection*
        // (it picks a recent `tool:` snapshot) but not in *effect*: it checks
        // out the whole tree, so it also rolls back a working-tree path that no
        // tool touched. That is the data loss #2 removed the control over, and
        // it is why a per-file action cannot be built on top of it.
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let touched = repo.work_tree().join("touched.txt");
        let unrelated = repo.work_tree().join("unrelated.txt");

        std::fs::write(&touched, b"v1").unwrap();
        std::fs::write(&unrelated, b"snapshot-time").unwrap();
        let id = repo.snapshot("tool:call-1").expect("snapshot");

        // The tool edits one file; something else — the user, another editor —
        // changes the other one after the snapshot was taken.
        std::fs::write(&touched, b"v2").unwrap();
        std::fs::write(&unrelated, b"user-work-in-progress").unwrap();

        repo.restore(&id).expect("whole-tree restore");
        assert_eq!(std::fs::read_to_string(&touched).unwrap(), "v1");
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "snapshot-time",
            "whole-tree restore rolls back a file no tool touched"
        );

        // Same situation again, but through the file-scoped path the
        // `file-revert` endpoint uses.
        std::fs::write(&touched, b"v1").unwrap();
        std::fs::write(&unrelated, b"snapshot-time").unwrap();
        let id2 = repo.snapshot("tool:call-2").expect("snapshot 2");
        std::fs::write(&touched, b"v2").unwrap();
        std::fs::write(&unrelated, b"user-work-in-progress").unwrap();

        repo.restore_paths(&id2, &[PathBuf::from("touched.txt")])
            .expect("scoped restore");
        assert_eq!(std::fs::read_to_string(&touched).unwrap(), "v1");
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "user-work-in-progress",
            "the scoped restore must leave the unrelated edit alone"
        );
    }

    #[test]
    fn restore_paths_removes_a_file_created_after_the_snapshot() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let kept = repo.work_tree().join("kept.txt");
        let created = repo.work_tree().join("created.txt");

        std::fs::write(&kept, b"kept").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::write(&created, b"new file").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        let outcomes = repo
            .restore_paths(&id, &[PathBuf::from("created.txt")])
            .expect("scoped restore");

        assert!(
            !created.exists(),
            "a created file must be removed by revert"
        );
        assert!(kept.exists(), "the untouched file must survive");
        assert_eq!(outcomes[0].action, PathRestoreAction::Removed);
    }

    #[test]
    fn restore_paths_recreates_a_file_deleted_after_the_snapshot() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let deleted = repo.work_tree().join("deleted.txt");

        std::fs::write(&deleted, b"content").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::remove_file(&deleted).unwrap();
        repo.snapshot("post-turn:1").expect("snapshot 2");

        let outcomes = repo
            .restore_paths(&id, &[PathBuf::from("deleted.txt")])
            .expect("scoped restore");

        assert_eq!(std::fs::read_to_string(&deleted).unwrap(), "content");
        assert_eq!(outcomes[0].action, PathRestoreAction::Recreated);
    }

    #[test]
    fn restore_paths_rejects_parent_traversal() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"a").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        let err = repo
            .restore_paths(&id, &[PathBuf::from("../escape.txt")])
            .expect_err("traversal must be refused");
        assert!(err.to_string().contains("unsafe path"), "got: {err}");
    }

    #[test]
    fn path_differs_from_snapshot_is_scoped_to_the_named_path() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let touched = repo.work_tree().join("touched.txt");
        let untouched = repo.work_tree().join("untouched.txt");

        std::fs::write(&touched, b"v1").unwrap();
        std::fs::write(&untouched, b"v1").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        std::fs::write(&touched, b"v2").unwrap();

        assert!(
            repo.path_differs_from_snapshot(&id, Path::new("touched.txt"))
                .expect("differs")
        );
        assert!(
            !repo
                .path_differs_from_snapshot(&id, Path::new("untouched.txt"))
                .expect("differs")
        );
    }

    #[test]
    fn workspace_relative_path_accepts_inside_paths_and_refuses_outside_ones() {
        // A real absolute temp path so the fixture is absolute on Windows too
        // (`/tmp/ws` is a relative path with a root-dir component there).
        let temp = std::env::temp_dir();
        let workspace = temp.join("ws");
        let other = temp.join("other");

        assert_eq!(
            workspace_relative_path(&workspace, "src/lib.rs"),
            Some(PathBuf::from("src/lib.rs"))
        );
        let inside = workspace.join("src").join("lib.rs");
        assert_eq!(
            workspace_relative_path(&workspace, &inside.to_string_lossy()),
            Some(PathBuf::from("src").join("lib.rs"))
        );
        let outside = other.join("lib.rs");
        assert_eq!(
            workspace_relative_path(&workspace, &outside.to_string_lossy()),
            None
        );
        assert_eq!(workspace_relative_path(&workspace, "../escape"), None);
        assert_eq!(
            workspace_relative_path(&workspace, "src/../../escape"),
            None
        );
        assert_eq!(workspace_relative_path(&workspace, ""), None);
        // Whitespace and glob characters are literal filename bytes.
        assert_eq!(
            workspace_relative_path(&workspace, " padded.txt "),
            Some(PathBuf::from(" padded.txt "))
        );
        assert_eq!(
            workspace_relative_path(&workspace, "file[12].txt"),
            Some(PathBuf::from("file[12].txt"))
        );
    }

    fn sha256_hash(path: &Path) -> String {
        format!(
            "sha256:{}",
            crate::hashing::sha256_hex(std::fs::read(path).unwrap())
        )
    }

    /// The Git primitive treats `[12]` as a pattern even after `--`; the
    /// file-scoped restore must not. `file[12].txt` and `file1.txt` both exist
    /// in the snapshot, so only literal pathspecs keep the second one intact.
    #[test]
    fn restore_file_if_unchanged_treats_glob_characters_literally() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let literal = repo.work_tree().join("file[12].txt");
        let sibling = repo.work_tree().join("file1.txt");
        std::fs::write(&literal, b"literal-before").unwrap();
        std::fs::write(&sibling, b"sibling-before").unwrap();
        let id = repo.snapshot("tool:call-1").expect("snapshot");
        std::fs::write(&literal, b"literal-after").unwrap();
        std::fs::write(&sibling, b"sibling-after").unwrap();

        assert!(
            repo.path_differs_from_snapshot(&id, Path::new("file[12].txt"))
                .unwrap()
        );
        let outcomes = repo
            .restore_file_if_unchanged(&id, Path::new("file[12].txt"), &sha256_hash(&literal))
            .expect("literal restore");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].action, PathRestoreAction::Modified);
        assert_eq!(std::fs::read_to_string(&literal).unwrap(), "literal-before");
        assert_eq!(
            std::fs::read_to_string(&sibling).unwrap(),
            "sibling-after",
            "a bracketed filename must never restore its glob siblings"
        );
    }

    #[test]
    fn restore_file_if_unchanged_refuses_when_the_reviewed_bytes_changed() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let file = repo.work_tree().join("a.txt");
        std::fs::write(&file, b"v1").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");
        std::fs::write(&file, b"v2").unwrap();
        let reviewed = sha256_hash(&file);
        // The user edits again after the client captured its change record.
        std::fs::write(&file, b"v3-user-edit").unwrap();

        let err = repo
            .restore_file_if_unchanged(&id, Path::new("a.txt"), &reviewed)
            .expect_err("stale hash must refuse");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v3-user-edit");
        // `absent` is only valid for a file the client saw as deleted.
        let err = repo
            .restore_file_if_unchanged(&id, Path::new("a.txt"), "absent")
            .expect_err("absent must not match an existing file");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        // The exact current bytes restore.
        let outcomes = repo
            .restore_file_if_unchanged(&id, Path::new("a.txt"), &sha256_hash(&file))
            .expect("current hash restores");
        assert_eq!(outcomes[0].action, PathRestoreAction::Modified);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
    }

    #[test]
    fn restore_file_if_unchanged_handles_deleted_and_created_files() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let deleted = repo.work_tree().join("deleted.txt");
        std::fs::write(&deleted, b"content").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");
        std::fs::remove_file(&deleted).unwrap();
        let created = repo.work_tree().join("created.txt");
        std::fs::write(&created, b"new").unwrap();

        let outcomes = repo
            .restore_file_if_unchanged(&id, Path::new("deleted.txt"), "absent")
            .expect("recreate");
        assert_eq!(outcomes[0].action, PathRestoreAction::Recreated);
        assert_eq!(std::fs::read_to_string(&deleted).unwrap(), "content");

        let outcomes = repo
            .restore_file_if_unchanged(&id, Path::new("created.txt"), &sha256_hash(&created))
            .expect("remove");
        assert_eq!(outcomes[0].action, PathRestoreAction::Removed);
        assert!(!created.exists());
        // A created file inside a new directory is removed alone; the
        // directory the user made stays.
        let nested_dir = repo.work_tree().join("newdir");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let nested = nested_dir.join("only.txt");
        std::fs::write(&nested, b"n").unwrap();
        let outcomes = repo
            .restore_file_if_unchanged(&id, Path::new("newdir/only.txt"), &sha256_hash(&nested))
            .expect("remove nested");
        assert_eq!(outcomes[0].action, PathRestoreAction::Removed);
        assert!(!nested.exists());
        assert!(nested_dir.is_dir(), "the parent directory is not pruned");
        // A path missing on both sides is not a change and reports nothing.
        assert!(
            !repo
                .path_differs_from_snapshot(&id, Path::new("never.txt"))
                .unwrap()
        );
    }

    #[test]
    fn restore_file_if_unchanged_refuses_directories_git_metadata_and_ignored_files() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let dir = repo.work_tree().join("src");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.rs"), b"fn a() {}").unwrap();
        std::fs::write(
            repo.work_tree().join(".gitignore"),
            "ignored.txt
",
        )
        .unwrap();
        std::fs::write(repo.work_tree().join("ignored.txt"), b"secret").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");
        std::fs::write(dir.join("lib.rs"), b"fn b() {}").unwrap();

        let mut refused = vec!["src", ".git/config", "src/.GIT/x", ".git"];
        if cfg!(windows) {
            refused.extend([".git./config", ".git /config", "GIT~1/config"]);
        }
        for rel in refused {
            let err = repo.validate_restore_file(Path::new(rel)).expect_err(rel);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{rel}");
        }
        let err = repo
            .restore_file_if_unchanged(&id, Path::new("src"), "absent")
            .expect_err("directories are refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
            "fn b() {}"
        );

        // A gitignored file is excluded from the safety backup, so removing
        // it would be unrecoverable: refuse and leave it in place.
        let ignored = repo.work_tree().join("ignored.txt");
        let err = repo
            .restore_file_if_unchanged(&id, Path::new("ignored.txt"), &sha256_hash(&ignored))
            .expect_err("ignored files are refused");
        assert!(err.to_string().contains("safety snapshot"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&ignored).unwrap(), "secret");
    }

    #[cfg(unix)]
    #[test]
    fn restore_file_if_unchanged_refuses_symlinks_anywhere_in_the_path() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("target.txt"), b"outside").unwrap();
        std::fs::write(repo.work_tree().join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink(&outside, repo.work_tree().join("linkdir")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("target.txt"),
            repo.work_tree().join("link.txt"),
        )
        .unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        for rel in ["link.txt", "linkdir/target.txt"] {
            let err = repo
                .restore_file_if_unchanged(&id, Path::new(rel), "absent")
                .expect_err(rel);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{rel}");
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("target.txt")).unwrap(),
            "outside"
        );
        // The snapshot side is checked too: a symlink entry in the tree is
        // not a regular file even when the work tree copy is gone.
        std::fs::remove_file(repo.work_tree().join("link.txt")).unwrap();
        let err = repo
            .restore_file_if_unchanged(&id, Path::new("link.txt"), "absent")
            .expect_err("snapshot symlink entry");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!repo.work_tree().join("link.txt").exists());
    }

    #[test]
    fn list_distinguishes_an_unborn_head_from_broken_history() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        assert!(repo.list(10).expect("unborn HEAD lists nothing").is_empty());

        std::fs::write(repo.work_tree().join("a.txt"), b"a").unwrap();
        repo.snapshot("pre-turn:1").expect("snapshot");
        assert_eq!(repo.list(10).unwrap().len(), 1);

        // Point the branch at an object that does not exist: the history is
        // now broken, which must surface as an error rather than "no
        // snapshots" (an empty list would let patch-undo drop a turn).
        let head = String::from_utf8(
            run_git(repo.git_dir(), repo.work_tree(), &["symbolic-ref", "HEAD"])
                .unwrap()
                .stdout,
        )
        .unwrap();
        std::fs::write(
            repo.git_dir().join(head.trim()),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        let err = repo.list(10).expect_err("broken history must error");
        assert!(err.to_string().contains("git log failed"), "got: {err}");
    }

    #[test]
    fn restore_takes_a_pre_restore_safety_snapshot_that_round_trips() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let f = repo.work_tree().join("file.txt");

        std::fs::write(&f, b"v1").unwrap();
        let id1 = repo.snapshot("pre-turn:1").expect("snapshot v1");

        std::fs::write(&f, b"v2").unwrap();
        repo.snapshot("post-turn:1").expect("snapshot v2");

        repo.restore(&id1).expect("restore to v1");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "v1");

        // The restore must have captured the pre-restore state (v2) under a
        // `pre-restore:` label naming its target, so the destructive op is
        // itself reversible (2026-08-04 snapshot hunt).
        let snapshots = repo.list(usize::MAX).expect("list");
        let safety = snapshots
            .iter()
            .find(|s| s.label.starts_with("pre-restore:"))
            .expect("a pre-restore safety snapshot must exist");
        assert!(
            safety.label.ends_with(&id1.as_str()[..12]),
            "safety label should name the restore target: {}",
            safety.label
        );

        repo.restore(&safety.id)
            .expect("restore the safety snapshot");
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "v2",
            "the safety snapshot must bring back the pre-restore state"
        );
    }

    #[test]
    fn snapshot_and_restore_do_not_move_user_git_head() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        crate::dependencies::Git::command()
            .expect("git not found")
            .arg("-C")
            .arg(&workspace)
            .arg("init")
            .arg("--quiet")
            .status()
            .unwrap();
        std::fs::write(workspace.join("tracked.txt"), b"committed").unwrap();
        crate::dependencies::Git::command()
            .expect("git not found")
            .arg("-C")
            .arg(&workspace)
            .arg("add")
            .arg("tracked.txt")
            .status()
            .unwrap();
        crate::dependencies::Git::command()
            .expect("git not found")
            .arg("-C")
            .arg(&workspace)
            .arg("-c")
            .arg("user.name=user")
            .arg("-c")
            .arg("user.email=user@example.test")
            .arg("commit")
            .arg("--quiet")
            .arg("-m")
            .arg("init")
            .status()
            .unwrap();
        let user_head_before = crate::dependencies::Git::command()
            .expect("git not found")
            .arg("-C")
            .arg(&workspace)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;

        let _home = scoped_home(tmp.path());
        let repo = SnapshotRepo::open_or_init(&workspace).unwrap();
        std::fs::write(workspace.join("tracked.txt"), b"dirty-before").unwrap();
        let id = repo.snapshot("pre-turn:1").unwrap();
        std::fs::write(workspace.join("tracked.txt"), b"dirty-after").unwrap();
        repo.snapshot("post-turn:1").unwrap();
        repo.restore(&id).unwrap();

        let user_head_after = crate::dependencies::Git::command()
            .expect("git not found")
            .arg("-C")
            .arg(&workspace)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(user_head_after, user_head_before);
        assert_eq!(
            std::fs::read_to_string(workspace.join("tracked.txt")).unwrap(),
            "dirty-before"
        );
    }

    #[test]
    fn list_respects_limit() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        for i in 0..5 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot(&format!("turn:{i}")).unwrap();
        }
        let three = repo.list(3).unwrap();
        assert_eq!(three.len(), 3);
        // Newest first.
        assert_eq!(three[0].label, "turn:4");
    }

    #[test]
    fn prune_drops_snapshots_older_than_threshold() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("f.txt"), "v0").unwrap();
        repo.snapshot("turn:0").unwrap();

        // Wait one second so the snapshot's commit timestamp is strictly
        // in the past relative to the prune call's "now" — otherwise
        // same-second comparisons make the assertion flaky.
        std::thread::sleep(Duration::from_millis(1100));

        let removed = repo.prune_older_than(Duration::from_secs(0)).unwrap();
        assert!(removed >= 1, "expected at least 1 pruned, got {removed}");

        // After pruning everything, the next snapshot should start a
        // fresh history.
        std::fs::write(repo.work_tree().join("f.txt"), "v1").unwrap();
        repo.snapshot("turn:1").unwrap();
        let list = repo.list(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "turn:1");
    }

    /// The 2026-08-04 regression: with a cut in the MIDDLE of history,
    /// `prune_older_than` used to `update-ref HEAD <oldest survivor>`, which
    /// orphaned (and gc destroyed) the NEWEST snapshots while keeping the
    /// old ones as ancestors — the inverse of the intent, firing on every
    /// boot. This pins the correct partial-cut behavior.
    #[test]
    fn prune_older_than_keeps_the_newest_and_drops_only_the_old_tail() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());

        // Two "old" snapshots, then a pause, then two "new" ones.
        for i in 0..2 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("old{i}")).unwrap();
            repo.snapshot(&format!("old:{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
        // A wide gap so git's whole-second commit timestamps land the cut
        // unambiguously between the old and new pairs. The margins are
        // deliberately generous: this test runs under full-suite parallelism
        // where a sleep can overrun, and the cut is wall-clock. At prune time
        // the newest pair is ~0-1.2s old against a 6s cutoff, and the old
        // pair is ~9s old — ~5s of slack in both directions.
        std::thread::sleep(Duration::from_secs(8));
        for i in 0..2 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("new{i}")).unwrap();
            repo.snapshot(&format!("new:{i}")).unwrap();
            if i == 0 {
                std::thread::sleep(Duration::from_millis(1100));
            }
        }
        let before = repo.list(usize::MAX).unwrap();
        assert_eq!(before.len(), 4);
        // Derive the cut from the timestamps actually recorded rather than a
        // fixed 6s. A fixed cut assumes `repo.snapshot()` is fast: `new:0` is
        // only ~1.2s plus one git subprocess older than prune time, so on a
        // loaded Windows runner that subprocess alone pushed it past 6s and
        // three snapshots were pruned instead of two. (The old fixture guard
        // could not catch it either — it checked `before[0]` and `before[2]`,
        // and `before[1]` is the entry that drifts.)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Newest-first: [new:1, new:0, old:1, old:0]. The cut must land
        // strictly between the pairs, so aim at the midpoint of the 8s gap —
        // that leaves ~4s of slack against clock drift and a slow runner in
        // both directions.
        let survivor = before[1].timestamp;
        let victim = before[2].timestamp;
        assert!(
            survivor - victim >= 8,
            "fixture needs an 8s gap between the pairs (survivor {survivor}, victim {victim})"
        );
        let midpoint = victim + (survivor - victim) / 2;
        assert!(
            now > midpoint,
            "fixture cutoff is not before the current time"
        );
        let max_age = Duration::from_secs((now - midpoint) as u64);

        // The two old snapshots drop, the two new ones survive.
        let removed = repo.prune_older_than(max_age).unwrap();
        assert_eq!(removed, 2, "only the old tail should be removed");

        let remaining = repo.list(usize::MAX).unwrap();
        assert_eq!(remaining.len(), 2, "the two newest must survive");
        assert_eq!(
            remaining[0].label, "new:1",
            "newest survives (was destroyed before)"
        );
        assert_eq!(remaining[1].label, "new:0");
        assert!(
            !remaining.iter().any(|s| s.label.starts_with("old:")),
            "old snapshots must be gone, not kept as ancestors: {:?}",
            remaining.iter().map(|s| &s.label).collect::<Vec<_>>()
        );

        // The survivors' contents are intact and restorable.
        repo.restore(&remaining[0].id).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.work_tree().join("f.txt")).unwrap(),
            "new1"
        );
    }

    #[test]
    fn prune_keep_last_n_keeps_latest_and_gc_reclaims_rest() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());

        for i in 0..3 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot(&format!("turn:{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }

        assert_eq!(repo.list(usize::MAX).unwrap().len(), 3);

        let removed = repo.prune_keep_last_n(1).unwrap();
        assert_eq!(removed, 2);

        let remaining = repo.list(usize::MAX).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].label, "turn:2");

        // New snapshot starts a clean chain (not appending to old).
        std::fs::write(repo.work_tree().join("f.txt"), "fresh").unwrap();
        repo.snapshot("turn:new").unwrap();
        assert_eq!(repo.list(usize::MAX).unwrap().len(), 2);
    }

    /// The per-snapshot prune drops half a window at once instead of
    /// rebuilding the chain for every snapshot past the cap.
    #[test]
    fn batched_prune_waits_for_half_a_window_then_drops_it_together() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let max = 4;
        for n in 0..max + 1 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("{n}")).unwrap();
            repo.snapshot(&format!("tool:{n}")).unwrap();
        }
        // One over the cap: the plain prune would rebuild now, the batched
        // one waits.
        assert_eq!(repo.prune_keep_last_n_batched(max).unwrap(), 0);
        assert_eq!(repo.list(usize::MAX).unwrap().len(), max + 1);

        std::fs::write(repo.work_tree().join("f.txt"), "last").unwrap();
        repo.snapshot("tool:last").unwrap();
        assert_eq!(repo.prune_keep_last_n_batched(max).unwrap(), 2);
        let kept = repo.list(usize::MAX).unwrap();
        assert_eq!(kept.len(), max);
        assert_eq!(kept[0].label, "tool:last", "the newest snapshots survive");
    }

    #[test]
    fn prune_keep_last_n_preserves_multiple_snapshots_in_order() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());

        for i in 0..4 {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot(&format!("turn:{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }

        assert_eq!(repo.list(usize::MAX).unwrap().len(), 4);

        let removed = repo.prune_keep_last_n(2).unwrap();
        assert_eq!(removed, 2);

        let remaining = repo.list(usize::MAX).unwrap();
        assert_eq!(remaining.len(), 2);
        // Should be newest-first: turn:3 (newest), turn:2 (second newest)
        assert_eq!(remaining[0].label, "turn:3");
        assert_eq!(remaining[1].label, "turn:2");

        // New snapshot continues the chain.
        std::fs::write(repo.work_tree().join("f.txt"), "fresh").unwrap();
        repo.snapshot("turn:new").unwrap();
        let after = repo.list(usize::MAX).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].label, "turn:new");
    }

    #[test]
    fn open_or_init_removes_stale_tmp_pack_files_only() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let workspace = repo.work_tree().to_path_buf();
        let pack_dir = repo.git_dir().join("objects").join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();

        let stale = pack_dir.join("tmp_pack_stale");
        let fresh = pack_dir.join("tmp_pack_fresh");
        let ordinary_pack = pack_dir.join("pack-kept.pack");
        std::fs::write(&stale, b"stale").unwrap();
        std::fs::write(&fresh, b"fresh").unwrap();
        std::fs::write(&ordinary_pack, b"pack").unwrap();

        let old_time = SystemTime::now() - STALE_TMP_PACK_AGE - Duration::from_secs(60);
        {
            let file = File::options().write(true).open(&stale).unwrap();
            file.set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }

        SnapshotRepo::open_or_init(&workspace).unwrap();

        assert!(!stale.exists(), "stale tmp_pack file should be removed");
        assert!(fresh.exists(), "fresh tmp_pack file should be kept");
        assert!(ordinary_pack.exists(), "non-temp pack file should be kept");
    }

    #[test]
    fn snapshot_respects_workspace_gitignore() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.work_tree().join("ignored.txt"), b"secret").unwrap();
        std::fs::write(repo.work_tree().join("kept.txt"), b"public").unwrap();

        let id = repo.snapshot("pre-turn:1").expect("snapshot");

        // `git ls-tree` against the snapshot's commit shouldn't list ignored.txt.
        let ls = run_git(
            repo.git_dir(),
            repo.work_tree(),
            &["ls-tree", "-r", "--name-only", id.as_str()],
        )
        .expect("ls-tree");
        let names = String::from_utf8_lossy(&ls.stdout);
        assert!(names.contains("kept.txt"), "kept.txt missing: {names}");
        assert!(
            !names.contains("ignored.txt"),
            "ignored.txt should not be in snapshot: {names}",
        );
    }

    #[test]
    fn unsafe_workspace_rejects_home_directory_workspace() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();

        assert_eq!(
            unsafe_workspace_snapshot_reason(home, Some(home)),
            Some("home directory")
        );
    }

    #[test]
    fn unsafe_workspace_rejects_home_collection_directories() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let desktop = tmp.path().join("Desktop");
        std::fs::create_dir_all(&desktop).unwrap();

        assert_eq!(
            unsafe_workspace_snapshot_reason(&desktop, Some(home)),
            Some("home collection directory")
        );
    }

    #[test]
    fn unsafe_workspace_allows_project_directories_under_home() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let workspace = tmp.path().join("code").join("project");
        std::fs::create_dir_all(&workspace).unwrap();

        assert_eq!(
            unsafe_workspace_snapshot_reason(&workspace, Some(home)),
            None
        );
    }

    #[test]
    fn snapshot_respects_builtin_excludes() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::create_dir_all(repo.work_tree().join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(repo.work_tree().join(".next/cache")).unwrap();
        std::fs::create_dir_all(repo.work_tree().join("src")).unwrap();
        std::fs::write(
            repo.work_tree().join("node_modules/pkg/index.js"),
            b"generated",
        )
        .unwrap();
        std::fs::write(repo.work_tree().join(".next/cache/chunk.bin"), b"generated").unwrap();
        std::fs::write(repo.work_tree().join("debug.wasm"), b"binary").unwrap();
        std::fs::write(repo.work_tree().join("src/main.rs"), b"fn main() {}").unwrap();

        let excludes = std::fs::read_to_string(repo.git_dir().join("info/exclude")).unwrap();
        assert!(excludes.contains("node_modules/"));
        assert!(excludes.contains(".next/"));
        assert!(excludes.contains("*.wasm"));

        let id = repo.snapshot("pre-turn:1").expect("snapshot");
        let ls = run_git(
            repo.git_dir(),
            repo.work_tree(),
            &["ls-tree", "-r", "--name-only", id.as_str()],
        )
        .expect("ls-tree");
        let names = String::from_utf8_lossy(&ls.stdout);
        assert!(
            names.contains("src/main.rs"),
            "src/main.rs missing: {names}"
        );
        assert!(
            !names.contains("node_modules"),
            "node_modules should not be in snapshot: {names}",
        );
        assert!(
            !names.contains(".next"),
            ".next should not be in snapshot: {names}",
        );
        assert!(
            !names.contains("debug.wasm"),
            "binary artifacts should not be in snapshot: {names}",
        );
    }

    #[test]
    fn open_or_init_is_idempotent() {
        let tmp = tempdir().unwrap();
        let (_r, _h) = make_repo(tmp.path());
        // Second open should not panic and should reuse the existing
        // `.git`. We re-open via the public API rather than make_repo to
        // avoid double-acquiring HOME (the guard would deadlock).
        drop((_r, _h));
        let (_r2, _h2) = make_repo(tmp.path());
    }

    #[test]
    fn home_directory_guard_matches_canonical_paths() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let home_canonical = home.canonicalize().unwrap();
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace_canonical = workspace.canonicalize().unwrap();

        assert!(is_home_directory(&home_canonical, Some(home)));
        assert!(!is_home_directory(&workspace_canonical, Some(home)));
        assert!(!is_home_directory(&home_canonical, None));
    }

    #[test]
    fn dir_size_bytes_measures_directory_bytes() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("sizedir");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        // 3 bytes per file.
        std::fs::write(dir.join("a.txt"), b"abc").unwrap();
        std::fs::write(dir.join("sub/b.txt"), b"xyz").unwrap();

        let size = dir_size_bytes(&dir).expect("dir_size_bytes");
        assert_eq!(size, 6, "two 3-byte files should measure 6 bytes");

        // Write 2 MB of data.
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; 2 * 1024 * 1024]).unwrap();
        let size = dir_size_bytes(&dir).expect("dir_size_bytes after big write");
        assert_eq!(
            size,
            2 * 1024 * 1024 + 6,
            "expected 2 MB + 6 bytes after writing a 2 MB file"
        );
    }

    /// Regression: snapshot size cap (#1112). When the snapshot dir grows,
    /// `snapshot()` must prune old snapshots to stay under the limit.
    /// This test uses the real size constants, which are 500/400 MB —
    /// we can't easily blow up a temp dir to 500 MB in a unit test.
    /// Instead we verify the guard logic doesn't panic or error on a
    /// small repo (well under the cap), and that `snapshot()` still works.
    #[test]
    fn snapshot_succeeds_when_under_size_cap() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        // The side repo is tiny — well under 500 MB. Snapshot should work.
        std::fs::write(repo.work_tree().join("f.txt"), b"hello").unwrap();
        let id = repo.snapshot("pre-turn:1").expect("snapshot under cap");
        assert_eq!(id.as_str().len(), 40);
    }

    /// Sessions and sub-agents share one side repo. The size-pressure prune
    /// used to protect only the globally newest turn boundaries, so another
    /// session's snapshots pushed a running turn's `pre-turn:` out and its
    /// undo had nothing to restore.
    #[test]
    fn prune_size_pressure_keeps_every_sessions_turn_boundaries() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        for (i, (label, sid)) in [
            ("pre-turn:5", "A"),
            ("tool:a", "A"),
            ("pre-turn:1", "B"),
            ("tool:b", "B"),
            ("post-turn:1", "B"),
            ("pre-turn:2", "B"),
            ("tool:c", "B"),
        ]
        .into_iter()
        .enumerate()
        {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot_with_session(label, Some(sid))
                .expect("snapshot");
        }
        repo.prune_size_pressure(0, 0).expect("prune_size_pressure");
        let kept: Vec<(String, Option<String>)> = repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .map(|s| (s.label, s.session_id))
            .collect();
        let kept_a_pre = kept
            .iter()
            .any(|(label, sid)| label == "pre-turn:5" && sid.as_deref() == Some("A"));
        assert!(
            kept_a_pre,
            "session A's running turn stays restorable: {kept:?}"
        );
        assert_eq!(
            kept.iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["tool:c", "pre-turn:2", "post-turn:1", "pre-turn:5"],
            "only boundaries and the newest snapshot survive a full cut"
        );
    }

    /// The size-pressure prune drops the oldest snapshots first and keeps the
    /// newest one plus the newest turn boundaries. It used to prune by age
    /// from one second down, which wiped every restore point, the running
    /// turn's own `pre-turn:` included, on each snapshot of a side repo over
    /// the cap.
    #[test]
    fn prune_size_pressure_drops_oldest_first_and_keeps_turn_boundaries() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        for (i, label) in [
            "pre-turn:1",
            "tool:a",
            "post-turn:1",
            "pre-turn:2",
            "tool:b",
            "tool:c",
        ]
        .into_iter()
        .enumerate()
        {
            std::fs::write(repo.work_tree().join("f.txt"), format!("v{i}")).unwrap();
            repo.snapshot(label).expect("snapshot");
        }
        // A zero byte limit makes any non-empty side repo "over limit", so the
        // prune cuts as far as it may and reports exactly what it destroyed;
        // the count is what the user-visible notice is built from.
        let removed = repo.prune_size_pressure(0, 0).expect("prune_size_pressure");
        let labels: Vec<String> = repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(
            labels,
            ["tool:c", "pre-turn:2", "post-turn:1"],
            "the newest snapshot and the newest turn boundaries survive"
        );
        assert_eq!(removed, 3, "every dropped snapshot is reported");
        // The survivors still restore: the running turn can be undone.
        let pre = repo.list(usize::MAX).unwrap()[1].id.clone();
        repo.restore(&pre)
            .expect("restore the running turn's boundary");
        assert_eq!(
            std::fs::read_to_string(repo.work_tree().join("f.txt")).unwrap(),
            "v3"
        );
    }

    #[test]
    fn prune_size_pressure_is_a_noop_under_the_limit() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("f.txt"), b"v0").unwrap();
        repo.snapshot("pre-turn:0").expect("snapshot");
        let removed = repo
            .prune_size_pressure(u64::MAX, u64::MAX)
            .expect("prune_size_pressure");
        assert_eq!(removed, 0, "under the limit nothing may be removed");
        assert_eq!(repo.list(usize::MAX).unwrap().len(), 1);
    }

    #[test]
    fn snapshot_history_pruned_message_names_workspace_count_and_cap() {
        let msg = snapshot_history_pruned_message(Path::new("/tmp/ws"), 7);
        assert!(msg.contains("/tmp/ws"), "message must name the workspace");
        assert!(msg.contains("7"), "message must state the removed count");
        assert!(
            msg.contains(&MAX_SNAPSHOT_SIZE_MB.to_string()),
            "message must state the storage cap"
        );
    }

    #[test]
    fn estimate_workspace_size_bounded_returns_total_when_under_cap() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), vec![b'a'; 100]).unwrap();
        std::fs::write(workspace.join("b.txt"), vec![b'b'; 50]).unwrap();
        let total = estimate_workspace_size_bounded(&workspace, 10_000, SIZE_WALK_MAX_ENTRIES)
            .expect("under-cap walk must return a total");
        assert!(
            total >= 150,
            "total ({total}) must include both files (≥150 bytes)"
        );
    }

    #[test]
    fn estimate_workspace_size_bounded_reports_the_size_gate_when_over_cap() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        // Two 1 KB files, cap at 1 KB — second file should trip the cap.
        std::fs::write(workspace.join("a.bin"), vec![b'a'; 1024]).unwrap();
        std::fs::write(workspace.join("b.bin"), vec![b'b'; 1024]).unwrap();
        assert_eq!(
            estimate_workspace_size_bounded(&workspace, 1024, SIZE_WALK_MAX_ENTRIES),
            Err(WorkspaceGate::TooLarge),
            "over-cap walk must name the size gate for early bailout"
        );
    }

    #[test]
    fn oversize_gate_message_states_the_byte_cap_without_a_remedy() {
        // The remedy is localized by the notice surfaces; repeating it here is
        // what produced the doubled warning.
        let message =
            WorkspaceGate::TooLarge.describe(2 * 1024 * 1024 * 1024, Path::new("/tmp/ws"));
        assert!(message.starts_with(GATE_TOO_LARGE_MARKER));
        assert!(message.contains("/tmp/ws"));
        assert!(!message.contains("max_workspace_gb"));
        assert_eq!(message.lines().count(), 1, "the gate message is one line");
    }

    #[test]
    fn entry_gate_message_is_distinct_and_never_blames_the_size_cap() {
        let message = WorkspaceGate::TooManyEntries.describe(0, Path::new("/tmp/ws"));
        assert!(message.starts_with(GATE_TOO_MANY_ENTRIES_MARKER));
        assert!(
            !message.contains(GATE_TOO_LARGE_MARKER),
            "the entry gate must not be reported as a size trip"
        );
        assert!(message.contains(&SIZE_WALK_MAX_ENTRIES.to_string()));
        assert!(!message.contains("max_workspace_gb"));
    }

    #[test]
    fn estimate_workspace_size_bounded_skips_builtin_excluded_dirs() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("node_modules")).unwrap();
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        // 2 MB of "build output" in excluded dirs — must not count toward
        // the cap.
        std::fs::write(workspace.join("node_modules/big.bin"), vec![0u8; 1_000_000]).unwrap();
        std::fs::write(workspace.join("target/big.bin"), vec![0u8; 1_000_000]).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), b"// real source").unwrap();
        let total = estimate_workspace_size_bounded(&workspace, 500_000, SIZE_WALK_MAX_ENTRIES)
            .expect("walk must succeed since real source is tiny");
        assert!(
            total < 1_000,
            "total ({total}) must reflect only src/, not node_modules/ or target/"
        );
    }

    #[test]
    fn estimate_workspace_size_bounded_cap_zero_disables_cap() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        // 10 KB file — would trip a 1 KB cap, but cap=0 means no cap.
        std::fs::write(workspace.join("big.bin"), vec![0u8; 10 * 1024]).unwrap();
        let total = estimate_workspace_size_bounded(&workspace, 0, SIZE_WALK_MAX_ENTRIES)
            .expect("cap=0 must always return a total");
        assert!(
            total >= 10 * 1024,
            "total ({total}) must include the 10 KB file when cap is disabled"
        );
    }

    /// The entry ceiling is the bound that no test could reach before
    /// `max_entries` became a parameter: 200,000 inodes per run is not a
    /// price a unit test should pay. A byte-cheap workspace must still be
    /// refused, and refused as the *entry* gate — reporting `TooLarge` here
    /// would offer `max_workspace_gb` as a remedy that cannot lift it.
    #[test]
    fn entry_ceiling_refuses_a_byte_cheap_workspace_with_too_many_entries() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        for i in 0..10 {
            std::fs::write(workspace.join(format!("f{i}.txt")), b"x").unwrap();
        }
        assert_eq!(
            estimate_workspace_size_bounded(&workspace, 10_000_000, 3),
            Err(WorkspaceGate::TooManyEntries),
            "ten tiny files under a 10 MB cap must trip the entry bound, not the byte cap"
        );
    }

    /// The invariant documented on `WorkspaceGate::TooManyEntries` and on the
    /// estimator: `max_workspace_gb = 0` opts out of the byte cap only. A
    /// future "if `cap_bytes == 0`, skip the walk" shortcut would satisfy
    /// every other test here and silently delete the ceiling that exists to
    /// stop a multi-minute `git add -A`.
    #[test]
    fn cap_zero_does_not_lift_the_entry_ceiling() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        for i in 0..10 {
            std::fs::write(workspace.join(format!("f{i}.txt")), b"x").unwrap();
        }
        assert_eq!(
            estimate_workspace_size_bounded(&workspace, 0, 3),
            Err(WorkspaceGate::TooManyEntries),
            "cap_bytes = 0 disables the byte cap, never the entry ceiling"
        );
    }

    /// `ignore` disables gitignore matching entirely when no ancestor holds a
    /// `.git` (`require_git` defaults to true), but the snapshot's own
    /// `git add -A --work-tree <workspace>` reads `.gitignore` either way. A
    /// non-git workspace was therefore measured on content that would never
    /// be staged — and then told to fix it by editing `.gitignore`.
    ///
    /// Deliberately creates no `.git`: the point is the non-repo case.
    #[test]
    fn gitignored_content_is_excluded_outside_a_git_repo() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join(".gitignore"), "big.bin\n").unwrap();
        std::fs::write(workspace.join("big.bin"), vec![0u8; 1_000_000]).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), b"// real source").unwrap();
        assert!(
            !workspace.join(".git").exists(),
            "this test is only meaningful outside a git repo"
        );
        let total = estimate_workspace_size_bounded(&workspace, 500_000, SIZE_WALK_MAX_ENTRIES)
            .expect("the only large file is gitignored, so the walk must fit under the cap");
        assert!(
            total < 1_000,
            "total ({total}) must exclude the gitignored 1 MB file"
        );
    }

    #[test]
    fn open_or_init_with_cap_rejects_oversized_workspace() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let _home = scoped_home(tmp.path());
        // Drop a 4 KB file under a 1 KB cap.
        std::fs::write(workspace.join("big.bin"), vec![0u8; 4096]).unwrap();
        let outcome = SnapshotRepo::open_or_init_with_cap(&workspace, 1024);
        let err = match outcome {
            Ok(_) => panic!("oversized workspace must fail open_or_init_with_cap"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(GATE_TOO_LARGE_MARKER),
            "error must call out the size cap; got: {msg}"
        );
        let named_owned = workspace.display().to_string();
        let named = named_owned
            .strip_prefix(r"\\?\")
            .or_else(|| named_owned.strip_prefix("//?/"))
            .unwrap_or(named_owned.as_str());
        assert!(
            msg.contains(named),
            "error must name the workspace it refused; got: {msg}"
        );
        // The remedy belongs to the localized notice. Repeating it here is
        // what produced the doubled, three-line warning users saw.
        assert!(
            !msg.contains("max_workspace_gb"),
            "gate error must not carry its own remedy copy; got: {msg}"
        );
    }

    #[test]
    fn open_or_init_with_cap_zero_disables_size_check() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let _home = scoped_home(tmp.path());
        // 4 KB file but cap=0 → should still succeed.
        std::fs::write(workspace.join("big.bin"), vec![0u8; 4096]).unwrap();
        let repo = SnapshotRepo::open_or_init_with_cap(&workspace, 0)
            .expect("cap=0 must skip the size check");
        let id = repo
            .snapshot("pre-turn:1")
            .expect("snapshot under disabled cap");
        assert_eq!(id.as_str().len(), 40);
    }

    #[test]
    fn session_tagged_snapshot_round_trips_through_list() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"x").unwrap();

        repo.snapshot_with_session("pre-turn:1", Some("sess-42"))
            .expect("snapshot with session");

        let list = repo.list(10).expect("list");
        assert_eq!(list.len(), 1);
        // The visible label stays clean; the session id is decoded separately.
        assert_eq!(list[0].label, "pre-turn:1");
        assert_eq!(list[0].session_id.as_deref(), Some("sess-42"));
    }

    #[test]
    fn untagged_snapshot_decodes_without_session() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"x").unwrap();

        repo.snapshot("pre-turn:1").expect("snapshot");

        let list = repo.list(10).expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "pre-turn:1");
        assert_eq!(list[0].session_id, None);
    }

    /// A burst of per-tool snapshots larger than the count cap never pushes
    /// out the turn boundaries: the running turn's `pre-turn:` restore point
    /// survives its own 50-write turn and another thread's burst.
    #[test]
    fn prune_keep_last_n_retains_turn_boundaries_through_a_tool_burst() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let file = repo.work_tree().join("a.txt");
        std::fs::write(&file, "v0").unwrap();
        let older_post = repo
            .take_snapshot("post-turn:0", Some("thr_a"))
            .expect("snapshot");
        let pre = repo
            .take_snapshot("pre-turn:1", Some("thr_a"))
            .expect("snapshot");
        for i in 0..6 {
            std::fs::write(&file, format!("v{}", i + 1)).unwrap();
            repo.take_snapshot(&format!("tool:call-{i}"), Some("thr_a"))
                .expect("snapshot");
            repo.take_snapshot(&format!("post-tool:call-{i}"), Some("thr_a"))
                .expect("snapshot");
        }
        // 14 snapshots, cap 3: the newest three plus the (two) boundaries.
        let removed = repo.prune_keep_last_n(3).expect("prune");
        assert_eq!(removed, 9);
        let labels: Vec<String> = repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .map(|snapshot| snapshot.label)
            .collect();
        assert_eq!(
            labels,
            [
                "post-tool:call-5",
                "tool:call-5",
                "post-tool:call-4",
                "pre-turn:1",
                "post-turn:0",
            ]
        );
        let trees: Vec<SnapshotId> = repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .map(|snapshot| snapshot.tree)
            .collect();
        assert!(trees.contains(&pre.tree) && trees.contains(&older_post.tree));

        // Boundaries are themselves capped at the same count.
        for i in 2..6 {
            repo.take_snapshot(&format!("pre-turn:{i}"), Some("thr_a"))
                .expect("snapshot");
        }
        repo.prune_keep_last_n(3).expect("prune");
        let boundaries = repo
            .list(usize::MAX)
            .unwrap()
            .into_iter()
            .filter(|snapshot| is_turn_boundary_label(&snapshot.label))
            .count();
        assert_eq!(boundaries, 3);
    }

    #[test]
    fn prune_keep_last_n_preserves_session_tags() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let file = repo.work_tree().join("a.txt");

        // More snapshots than DEFAULT_MAX_SNAPSHOTS (50) so the survivor
        // chain is rebuilt as orphan commits — the path that previously
        // dropped the [sid=...] label prefix and turned every surviving
        // snapshot into a "legacy" (untagged) one.
        for i in 0..55 {
            std::fs::write(&file, format!("v{i}")).unwrap();
            repo.snapshot_with_session(&format!("pre-turn:{i}"), Some("sess-p"))
                .expect("tagged snapshot");
        }

        let removed = repo.prune_keep_last_n(50).expect("prune");
        assert!(removed > 0, "expected prune to drop older snapshots");

        let list = repo.list(usize::MAX).expect("list");
        assert_eq!(list.len(), 50);
        assert!(
            list.iter()
                .all(|s| s.session_id.as_deref() == Some("sess-p")),
            "prune must preserve [sid=...] prefixes; got untagged survivors"
        );
    }

    #[test]
    fn tagged_and_untagged_snapshots_coexist_in_one_chain() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"v1").unwrap();

        // Legacy untagged snapshot, then a session-tagged one.
        repo.snapshot("pre-turn:1").expect("legacy snapshot");
        std::fs::write(repo.work_tree().join("a.txt"), b"v2").unwrap();
        repo.snapshot_with_session("pre-turn:1", Some("sess-a"))
            .expect("tagged snapshot");

        let list = repo.list(10).expect("list");
        assert_eq!(list.len(), 2);
        // Newest first.
        assert_eq!(list[0].session_id.as_deref(), Some("sess-a"));
        assert_eq!(list[1].session_id, None);
        assert_eq!(list[1].label, "pre-turn:1");
    }

    /// The patch a per-call change record shows comes from the side repo's
    /// own two trees, names one path, and names it literally: a bracketed
    /// filename must not drag its glob sibling into the diff.
    #[test]
    fn patch_between_diffs_one_literal_path_between_two_snapshots() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha\n").unwrap();
        std::fs::write(repo.work_tree().join("b.txt"), b"keep\n").unwrap();
        std::fs::write(repo.work_tree().join("file[12].txt"), b"literal-before\n").unwrap();
        std::fs::write(repo.work_tree().join("file1.txt"), b"sibling-before\n").unwrap();
        let before = repo.snapshot("tool:call-1").expect("snapshot");
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha\nbeta\n").unwrap();
        std::fs::write(repo.work_tree().join("b.txt"), b"changed\n").unwrap();
        std::fs::write(repo.work_tree().join("file[12].txt"), b"literal-after\n").unwrap();
        let after = repo.snapshot("post-tool:call-1").expect("snapshot");

        let (patch, truncated) = repo
            .patch_between(&before, &after, "a.txt", 1 << 20)
            .expect("patch");
        assert!(!truncated);
        assert!(patch.contains("+beta"), "{patch}");
        assert!(
            !patch.contains("b.txt"),
            "only the named path belongs in this patch: {patch}"
        );

        let (literal, _) = repo
            .patch_between(&before, &after, "file[12].txt", 1 << 20)
            .expect("patch");
        assert!(literal.contains("+literal-after"), "{literal}");
        assert!(
            !literal.contains("file1.txt"),
            "a bracketed filename must not diff its glob sibling: {literal}"
        );

        // A path that does not differ writes nothing rather than an empty
        // hunk the caller would have to interpret.
        let unchanged = repo
            .patch_between(&before, &before, "a.txt", 1 << 20)
            .expect("patch");
        assert!(unchanged.0.is_empty(), "{unchanged:?}");
        assert!(!unchanged.1);
    }

    /// The work tree is compared as it is now, not as the newest snapshot
    /// left it: a file created since, which no snapshot or index holds, is in
    /// the diff with its content, and reading it moves neither the repo's
    /// HEAD nor its index.
    #[test]
    fn work_tree_changes_since_sees_edits_deletions_and_unrecorded_new_files() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        let ws = repo.work_tree().to_path_buf();
        std::fs::write(ws.join(".gitignore"), b"ignored/\n").unwrap();
        std::fs::write(ws.join("keep.txt"), b"same\n").unwrap();
        std::fs::write(ws.join("edit.txt"), b"one\ntwo\n").unwrap();
        std::fs::write(ws.join("gone.txt"), b"bye\n").unwrap();
        std::fs::write(ws.join("late.txt"), b"still here\n").unwrap();
        let before = repo.snapshot("pre-turn:1").expect("snapshot");
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let index = git_output(&repo, &["ls-files", "--stage"]);

        std::fs::write(ws.join("edit.txt"), b"one\nTWO\n").unwrap();
        std::fs::remove_file(ws.join("gone.txt")).unwrap();
        std::fs::write(ws.join("new file.txt"), b"fresh\n").unwrap();
        std::fs::create_dir_all(ws.join("ignored")).unwrap();
        std::fs::write(ws.join("ignored/skip.txt"), b"skip\n").unwrap();
        // Ignored only after the snapshot recorded it, and untouched: it is
        // not a deletion.
        std::fs::write(ws.join(".gitignore"), b"ignored/\nlate.txt\n").unwrap();

        let changes = repo
            .work_tree_changes_since(&before, 1 << 20)
            .expect("changes");
        let mut names: Vec<&str> = changes.names.lines().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [".gitignore", "edit.txt", "gone.txt", "new file.txt"]
        );
        assert!(!changes.patch.contains("still here"), "{}", changes.patch);
        assert!(changes.stat.contains("edit.txt"), "{}", changes.stat);
        assert!(!changes.patch_truncated);
        for expected in ["-two", "+TWO", "-bye", "+fresh"] {
            assert!(changes.patch.contains(expected), "{}", changes.patch);
        }
        assert!(!changes.patch.contains("skip"), "{}", changes.patch);
        assert_eq!(git_output(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(git_output(&repo, &["ls-files", "--stage"]), index);

        let cut = repo.work_tree_changes_since(&before, 40).expect("changes");
        assert!(cut.patch_truncated);
        assert!(cut.patch.len() <= 40);
        assert_eq!(cut.names, changes.names);

        // Back at the snapshot: nothing differs, and nothing is invented.
        std::fs::write(ws.join(".gitignore"), b"ignored/\n").unwrap();
        std::fs::write(ws.join("edit.txt"), b"one\ntwo\n").unwrap();
        std::fs::write(ws.join("gone.txt"), b"bye\n").unwrap();
        std::fs::remove_file(ws.join("new file.txt")).unwrap();
        let none = repo
            .work_tree_changes_since(&before, 1 << 20)
            .expect("changes");
        assert_eq!(none.names, "");
        assert_eq!(none.patch, "");
    }

    /// A tree the repo no longer holds — the receipt outlived the object —
    /// is reported as absent rather than as a diff failure, so a caller can
    /// tell pruning from a broken repo without reading git's stderr.
    #[test]
    fn has_tree_separates_a_pruned_object_from_a_present_one() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha").unwrap();
        let taken = repo.take_snapshot("tool:call-1", None).expect("snapshot");

        assert!(
            repo.has_tree(&taken.tree).expect("tree lookup"),
            "the snapshot's own tree resolves"
        );
        // Valid hex, never written: exactly what a pruned receipt names.
        let pruned = SnapshotId::parse(&"deadbeef".repeat(5)).unwrap();
        assert!(!repo.has_tree(&pruned).expect("missing tree lookup"));
    }

    #[test]
    fn has_tree_reports_broken_repository_metadata_as_an_error() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("a.txt"), b"alpha").unwrap();
        let taken = repo.take_snapshot("tool:call-1", None).expect("snapshot");
        std::fs::write(
            repo.git_dir().join("HEAD"),
            b"invalid snapshot repository metadata\n",
        )
        .unwrap();
        let error = repo
            .has_tree(&taken.tree)
            .expect_err("broken repo is not pruning");
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    /// A patch larger than the caller's bound is cut on a char boundary and
    /// reported as cut, so a client never receives half a character.
    #[test]
    fn patch_between_cuts_an_over_long_patch_on_a_char_boundary() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        std::fs::write(repo.work_tree().join("d.txt"), "start\n").unwrap();
        let before = repo.snapshot("tool:call-1").expect("snapshot");
        let long = format!("{}\n", "汉字宽字符行".repeat(400));
        std::fs::write(repo.work_tree().join("d.txt"), long).unwrap();
        let after = repo.snapshot("post-tool:call-1").expect("snapshot");

        let (patch, truncated) = repo
            .patch_between(&before, &after, "d.txt", 64)
            .expect("patch");
        assert!(truncated, "a 64-byte bound must cut this patch");
        assert!(patch.len() <= 64, "{} bytes kept", patch.len());
        assert!(patch.is_char_boundary(patch.len()));

        let (whole, not_truncated) = repo
            .patch_between(&before, &after, "d.txt", 1 << 20)
            .expect("patch");
        assert!(!not_truncated);
        assert!(whole.contains("汉字宽字符行"), "{whole}");
    }

    #[test]
    fn run_git_drains_output_larger_than_the_pipe_buffer() {
        let tmp = tempdir().unwrap();
        let (repo, _home) = make_repo(tmp.path());
        // ~5000 paths is well past the 64 KiB OS pipe buffer: output of this
        // size is only reachable when the pipes are drained while the child
        // runs. A bounded read that only starts after the child exits would
        // deadlock the child on its own output and die at the command
        // timeout instead of returning the tree listing.
        for i in 0..5000 {
            std::fs::write(repo.work_tree().join(format!("file_{i:05}.txt")), b"x").unwrap();
        }
        let id = repo.snapshot("large-output").expect("snapshot");
        let paths = repo
            .tree_paths(id.as_str())
            .expect("tree_paths must drain output instead of timing out");
        assert!(
            paths.len() >= 5000,
            "expected every file in the tree listing, got {}",
            paths.len()
        );
    }
}

/// The bounded-git tests drive the core with real children, so they need a
/// POSIX shell; the timeout pin also depends on wall-clock behavior.
#[cfg(all(test, unix))]
mod bounded_git_tests {
    use super::*;

    #[test]
    fn bounded_git_times_out_a_wedged_child_and_reports_timed_out() {
        // A child that never exits stands in for a wedged git (stalled
        // mount, hung hook): the call must come back with a TimedOut error
        // promptly instead of blocking the turn pipeline forever.
        let started = std::time::Instant::now();
        let mut wedged = std::process::Command::new("sh");
        wedged.arg("-c").arg("sleep 30");
        let err = run_bounded_git_with_timeout(&mut wedged, "sleep", Duration::from_millis(250))
            .expect_err("a wedged child must hit the bound");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            err.to_string().contains("timed out after"),
            "the error must report the bound; got {err}"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(15),
            "the bound must be enforced promptly; took {elapsed:?}"
        );
    }

    #[test]
    fn bounded_git_returns_promptly_when_a_grandchild_holds_the_pipes() {
        // The direct child (sh) exits after the echo; the backgrounded
        // sleep inherits both pipes and holds them for 30s. The call must
        // still return promptly with the output captured before the grace,
        // annotated on stderr — not wait out the grandchild.
        let started = std::time::Instant::now();
        let mut sh = std::process::Command::new("sh");
        sh.arg("-c").arg("echo bounded-git-grandchild; sleep 30 &");
        let output = run_bounded_git(&mut sh, "sh").expect("sh must succeed");
        let elapsed = started.elapsed();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("bounded-git-grandchild"),
            "output captured before the grace must survive: {:?}",
            output.stdout
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("git output pipes did not close after git exited"),
            "the partial-output note must explain the early return: {:?}",
            output.stderr
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "a pipe-holding grandchild must not hold the call past the grace; took {elapsed:?}"
        );
    }
}

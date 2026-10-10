//! Native git status / worktree surface for the TUI chrome.
//!
//! Cached and non-blocking: probes run off the render path on a background
//! thread and the renderer only ever reads [`cached_status`].
//!
//! A probe shells out to the real `git` binary. One
//! `status --porcelain=v2 --branch -z` carries the branch, its upstream,
//! ahead/behind and every changed path, replacing the separate
//! `symbolic-ref`, `rev-list` and `status --porcelain` calls it used to make
//! (#6565). Around it: `rev-parse --show-toplevel`, one
//! `rev-parse --git-dir --git-common-dir` (repository name and linked
//! worktree), `log -5` for recent commits, `worktree list --porcelain`, and
//! `remote get-url origin` by way of
//! [`observed_git_repo`]. Git older than 2.11 has no
//! porcelain v2; the probe then falls back to the old three calls. There is
//! no `gix` dependency and no per-invocation timeout. All of these run with
//! `GIT_OPTIONAL_LOCKS=0` so a read never contends for `.git/index.lock` in
//! the user's repository.
//!
//! The same status call and parser back the composer's "branch | status"
//! badge ([`context_line`]) and the engine's per-turn git line
//! ([`probe_workspace_status`]), so the chrome, the Git view and the model
//! read one parser instead of three.
//!
//! This module owns capability and state outside the renderer so
//! `widgets/mod.rs` / `ui.rs` stay projection-only.

use crate::dependencies::{ExternalTool, Git};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Snapshot of repository status for chrome / worktree manager.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatusSnapshot {
    pub root: Option<PathBuf>,
    pub repository_name: Option<String>,
    pub branch: Option<String>,
    /// `owner/name` when `origin` resolves to a recognised forge, from the
    /// one normalizer that owns that judgement
    /// ([`normalize_observed_git_repo`]): paths,
    /// credentials, and unknown hosts are dropped rather than displayed.
    /// Cached here so chrome can name the repository without probing on the
    /// render path.
    pub remote_slug: Option<String>,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Whether the branch tracks an upstream; `ahead`/`behind` mean nothing
    /// without one.
    pub has_upstream: bool,
    /// `branch` holds a short commit id because HEAD is detached.
    pub detached: bool,
    pub changes: ChangeCounts,
    /// Total changed paths before the display list is capped.
    pub changed_path_count: usize,
    /// The first [`MAX_CHANGED_PATHS`] changed paths, in git's order.
    pub changed_paths: Vec<ChangedPath>,
    pub recent_commits: Vec<RecentCommit>,
    /// This checkout is a linked worktree, not the main one.
    pub is_linked_worktree: bool,
    pub worktrees: Vec<WorktreeEntry>,
    pub fetched_at: Option<Instant>,
    pub error: Option<String>,
    /// The workspace this snapshot was probed *from*, which is not the same
    /// as [`Self::root`]: launching in a subdirectory gives a `root` of the
    /// repository top level while the workspace stays the subdirectory.
    /// Staleness must compare the probe's own input, not its result.
    pub probed_workspace: Option<PathBuf>,
}

/// Changed paths by kind, classified exactly as the composer badge always
/// did: a path can be both staged and modified; `?` is untracked; `U` is a
/// conflict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub conflicts: usize,
}

impl ChangeCounts {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }

    /// `2 staged, 1 modified`, or `clean`.
    #[must_use]
    pub fn summary(&self) -> String {
        let parts = [
            (self.staged, "staged"),
            (self.modified, "modified"),
            (self.untracked, "untracked"),
            (self.conflicts, "conflicts"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>();
        if parts.is_empty() {
            "clean".to_string()
        } else {
            parts.join(", ")
        }
    }

    fn record(&mut self, x: char, y: char, unmerged: bool) {
        if x == '?' && y == '?' {
            self.untracked = self.untracked.saturating_add(1);
            return;
        }
        // An unmerged path is a conflict and nothing else: its two letters
        // name the merge sides, not a staged and a worktree change (U08-m2).
        if unmerged {
            self.conflicts = self.conflicts.saturating_add(1);
            return;
        }
        if x != ' ' && x != '?' {
            self.staged = self.staged.saturating_add(1);
        }
        if y != ' ' && y != '?' {
            self.modified = self.modified.saturating_add(1);
        }
    }
}

/// One changed path with its two-letter status (`M `, ` M`, `??`, `UU`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    pub code: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentCommit {
    pub hash: String,
    pub subject: String,
    /// Relative commit time as git words it (`3 hours ago`).
    pub when: String,
}

/// Changed paths kept for the Git view.
pub const MAX_CHANGED_PATHS: usize = 20;
/// Recent commits kept for the Git view.
const RECENT_COMMITS: &str = "-5";

/// What one `git status --porcelain=v2 --branch -z` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorcelainStatus {
    /// Branch name, or `None` for a detached HEAD.
    pub head: Option<String>,
    /// Commit id; `None` on an unborn branch.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: ChangeCounts,
    pub changed_path_count: usize,
    pub changed_paths: Vec<ChangedPath>,
}

impl PorcelainStatus {
    /// The ref the badge shows: the branch, or `detached:<short id>`.
    #[must_use]
    pub fn branch_label(&self) -> Option<String> {
        match (&self.head, &self.oid) {
            (Some(head), _) => Some(head.clone()),
            (None, Some(oid)) => Some(format!("detached:{}", short_oid(oid))),
            (None, None) => None,
        }
    }
}

fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

/// Parse `git status --porcelain=v2 --branch -z`. `None` when the output has
/// no `# branch.` header (a git too old for porcelain v2), so the caller can
/// fall back.
#[must_use]
pub fn parse_porcelain_v2(raw: &str) -> Option<PorcelainStatus> {
    let mut status = PorcelainStatus::default();
    let mut saw_branch_header = false;
    let mut records = raw.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# branch.") {
            saw_branch_header = true;
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "oid" if value != "(initial)" => status.oid = Some(value.to_string()),
                "head" if value != "(detached)" => status.head = Some(value.to_string()),
                "upstream" => status.upstream = Some(value.to_string()),
                "ab" => {
                    for part in value.split_whitespace() {
                        if let Some(ahead) = part.strip_prefix('+') {
                            status.ahead = ahead.parse().unwrap_or(0);
                        } else if let Some(behind) = part.strip_prefix('-') {
                            status.behind = behind.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let (kind, rest) = record.split_at(record.len().min(2));
        let (code, path) = match kind {
            // `1 XY sub mH mI mW hH hI path`
            "1 " => (rest.get(..2), rest.splitn(8, ' ').nth(7)),
            // `2 XY sub mH mI mW hH hI Xscore path`, then the original path.
            "2 " => {
                let _original = records.next();
                (rest.get(..2), rest.splitn(9, ' ').nth(8))
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
            "u " => (rest.get(..2), rest.splitn(10, ' ').nth(9)),
            "? " => (Some("??"), Some(rest)),
            _ => continue,
        };
        let (Some(code), Some(path)) = (code, path) else {
            continue;
        };
        let mut letters = code.chars().map(|c| if c == '.' { ' ' } else { c });
        let x = letters.next().unwrap_or(' ');
        let y = letters.next().unwrap_or(' ');
        status.changes.record(x, y, kind == "u ");
        status.changed_path_count += 1;
        if status.changed_paths.len() < MAX_CHANGED_PATHS {
            status.changed_paths.push(ChangedPath {
                code: format!("{x}{y}"),
                path: path.to_string(),
            });
        }
    }
    saw_branch_header.then_some(status)
}

/// Porcelain v1 (`git status --porcelain`) for git older than 2.11: counts
/// and paths only; the branch comes from separate calls.
fn parse_porcelain_v1(raw: &str) -> (ChangeCounts, Vec<ChangedPath>, usize) {
    let mut counts = ChangeCounts::default();
    let mut paths = Vec::new();
    let mut path_count = 0;
    for line in raw.lines() {
        let mut chars = line.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };
        if x == ' ' && y == ' ' {
            continue;
        }
        counts.record(
            x,
            y,
            x == 'U' || y == 'U' || matches!((x, y), ('A', 'A') | ('D', 'D')),
        );
        path_count += 1;
        if paths.len() < MAX_CHANGED_PATHS {
            paths.push(ChangedPath {
                code: format!("{x}{y}"),
                path: line.get(3..).unwrap_or_default().to_string(),
            });
        }
    }
    (counts, paths, path_count)
}

/// Branch, upstream and changes for `workspace` from one git call (two on a
/// git without porcelain v2). Errors outside a repository or when status fails.
/// This is the whole cost of the engine's per-turn git line.
///
/// # Errors
/// Returns the Git diagnostic if status cannot be read.
pub fn probe_workspace_status(workspace: &Path) -> Result<PorcelainStatus, String> {
    crate::project_context::find_git_root(workspace).ok_or("not a git repository")?;
    let raw = git_output(
        workspace,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ],
    )
    .ok();
    if let Some(status) = raw.as_deref().and_then(parse_porcelain_v2) {
        return Ok(status);
    }
    legacy_workspace_status(workspace)
}

/// The pre-2.11 path: the three calls porcelain v2 replaced.
fn legacy_workspace_status(workspace: &Path) -> Result<PorcelainStatus, String> {
    let raw = git_output(
        workspace,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ],
    )?;
    let (changes, changed_paths, changed_path_count) = parse_porcelain_v1(&raw);
    let head = git_output(workspace, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let oid = git_output(workspace, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut status = PorcelainStatus {
        head,
        oid,
        changes,
        changed_paths,
        changed_path_count,
        ..PorcelainStatus::default()
    };
    if let Ok(counts) = git_output(
        workspace,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    ) {
        let mut parts = counts.split_whitespace();
        if let (Some(behind), Some(ahead)) = (parts.next(), parts.next()) {
            status.behind = behind.parse().unwrap_or(0);
            status.ahead = ahead.parse().unwrap_or(0);
            status.upstream = Some("@{upstream}".to_string());
        }
    }
    Ok(status)
}

/// `branch | 2 staged, 1 modified` — the composer badge and the engine's
/// per-turn git line, from a status probe.
#[must_use]
pub fn status_line(status: &PorcelainStatus) -> Option<String> {
    Some(format!(
        "{} | {}",
        status.branch_label()?,
        status.changes.summary()
    ))
}

/// [`status_line`] from a cached snapshot. `None` when the snapshot has not
/// found a repository.
#[must_use]
pub fn context_line(snap: &GitStatusSnapshot) -> Option<String> {
    snap.root.as_ref()?;
    if let Some(error) = &snap.error {
        return Some(error.clone());
    }
    let branch = snap.branch.as_deref()?;
    let branch = if snap.detached {
        format!("detached:{branch}")
    } else {
        branch.to_string()
    };
    Some(format!("{branch} | {}", snap.changes.summary()))
}

fn parse_recent_commits(raw: &str) -> Vec<RecentCommit> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\u{1f}');
            Some(RecentCommit {
                hash: parts.next()?.trim().to_string(),
                subject: parts.next()?.trim().to_string(),
                when: parts.next().unwrap_or_default().trim().to_string(),
            })
        })
        .filter(|commit| !commit.hash.is_empty())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
}

const CACHE_TTL: Duration = Duration::from_secs(2);

/// Probe cadence while the session is in use. One probe is about a dozen
/// `git` processes, none of them counted in the TUI's own CPU time.
pub(crate) const ACTIVE_PROBE_INTERVAL: Duration = CACHE_TTL;

/// How long a session must see no input and no engine event before the probe
/// backs off to [`QUIET_PROBE_INTERVAL`] (#6728).
pub(crate) const QUIET_PROBE_AFTER: Duration = Duration::from_secs(30);

/// Probe cadence of a session nobody is touching. Input or any engine event
/// (a tool finishing, a turn ending) ends the quiet and the next tick probes.
pub(crate) const QUIET_PROBE_INTERVAL: Duration = Duration::from_secs(15);

/// Cache lifetime while backed off. Just under [`QUIET_PROBE_INTERVAL`], so
/// the 15 s workspace-context refresh finds the loop's probe fresh instead of
/// adding a second one of its own.
const QUIET_CACHE_TTL: Duration = Duration::from_secs(14);

static PROBE_BACKED_OFF: AtomicBool = AtomicBool::new(false);

/// Whether a session quiet for `quiet_for` is on the slow probe schedule.
#[must_use]
pub(crate) fn probe_is_backed_off(quiet_for: Duration) -> bool {
    quiet_for >= QUIET_PROBE_AFTER
}

/// The gap between probes for a session quiet for `quiet_for`.
#[must_use]
pub(crate) fn probe_interval(quiet_for: Duration) -> Duration {
    if probe_is_backed_off(quiet_for) {
        QUIET_PROBE_INTERVAL
    } else {
        ACTIVE_PROBE_INTERVAL
    }
}

/// Whether a probe is due, `last_probe_age` after the previous one (`None`:
/// never probed). Fresh input shortens the interval, so a probe that is
/// older than the fast cadence fires on the very next tick.
#[must_use]
pub(crate) fn probe_due(last_probe_age: Option<Duration>, quiet_for: Duration) -> bool {
    last_probe_age.is_none_or(|age| age >= probe_interval(quiet_for))
}

/// Tell the cache which schedule the loop is on, so [`refresh_if_stale`]
/// callers agree with it about what "fresh" means.
pub(crate) fn set_probe_backoff(backed_off: bool) {
    PROBE_BACKED_OFF.store(backed_off, Ordering::Relaxed);
}

fn cache_ttl() -> Duration {
    if PROBE_BACKED_OFF.load(Ordering::Relaxed) {
        QUIET_CACHE_TTL
    } else {
        CACHE_TTL
    }
}

static CACHE: OnceLock<Mutex<GitStatusSnapshot>> = OnceLock::new();

fn cache() -> &'static Mutex<GitStatusSnapshot> {
    CACHE.get_or_init(|| Mutex::new(GitStatusSnapshot::default()))
}

/// Return the last known snapshot without blocking.
#[must_use]
pub fn cached_status() -> GitStatusSnapshot {
    cache().lock().map(|g| g.clone()).unwrap_or_default()
}

/// Refresh status if the cache is stale. Safe to call from a background
/// worker; the render path should only read [`cached_status`].
/// Whether `snap` must be re-probed for `workspace`.
///
/// Split out and pure so the cache contract is testable without spawning
/// git. The workspace comparison uses [`GitStatusSnapshot::probed_workspace`]
/// deliberately: comparing `root` instead meant that any session launched
/// below the repository top level saw `root != workspace` forever, so this
/// returned `true` on every call and `CACHE_TTL` never applied. That turned
/// the two-second chrome tick into an unconditional six-command probe —
/// including the `git status` that contends for `.git/index.lock` (#5617).
fn snapshot_is_stale(snap: &GitStatusSnapshot, workspace: &Path) -> bool {
    snap.fetched_at.is_none_or(|t| t.elapsed() > cache_ttl())
        || snap.probed_workspace.as_deref() != Some(workspace)
}

/// Returns the snapshot for `workspace`: the cached one while fresh, else a
/// new probe (which also becomes the cache).
pub fn refresh_if_stale(workspace: &Path) -> GitStatusSnapshot {
    let cached = cache().lock().ok().map(|g| g.clone());
    if let Some(snap) = cached.as_ref().filter(|g| !snapshot_is_stale(g, workspace)) {
        return snap.clone();
    }
    // Backed off, with a healthy snapshot of this workspace in hand: re-read
    // the status only (#6728).
    if PROBE_BACKED_OFF.load(Ordering::Relaxed)
        && let Some(prev) = cached.filter(|prev| can_probe_status_only(prev, workspace))
    {
        let snap = probe_status_only(&prev);
        if let Ok(mut guard) = cache().lock() {
            *guard = snap.clone();
        }
        return snap;
    }
    force_refresh(workspace)
}

/// Force a refresh (e.g. after checkout / worktree create).
pub fn force_refresh(workspace: &Path) -> GitStatusSnapshot {
    let snap = probe_status(workspace);
    if let Ok(mut guard) = cache().lock() {
        *guard = snap.clone();
    }
    snap
}

pub(crate) fn probe_status(workspace: &Path) -> GitStatusSnapshot {
    let mut snap = GitStatusSnapshot {
        fetched_at: Some(Instant::now()),
        probed_workspace: Some(workspace.to_path_buf()),
        ..GitStatusSnapshot::default()
    };

    // Fast-fail outside a repository. Without this a non-git workspace
    // spawns a doomed `git` process on every tick forever. `find_git_root`
    // walks parents and understands the `gitdir:` pointer file, so linked
    // worktrees and submodules are still recognised — a bare `.git`
    // directory test would not be (#5617). The `rev-parse` below still runs
    // for the cases this cannot see, such as bare repositories.
    if crate::project_context::find_git_root(workspace).is_none() {
        snap.error = Some("not a git repository".into());
        return snap;
    }

    // Resolve git root.
    let root = match git_output(workspace, &["rev-parse", "--show-toplevel"]) {
        Ok(root) => PathBuf::from(root.trim()),
        Err(error) => {
            snap.error = Some(if error.contains("not a git repository") {
                "not a git repository".into()
            } else {
                format!("git unavailable: {}", error.trim())
            });
            return snap;
        }
    };
    snap.root = Some(root.clone());
    let (repository_name, is_linked_worktree) = repository_identity(&root);
    snap.repository_name = repository_name;
    snap.is_linked_worktree = is_linked_worktree;

    // Branch, upstream, ahead/behind and every changed path: one call.
    match probe_workspace_status(&root) {
        Ok(status) => apply_porcelain(&mut snap, status),
        Err(error) => {
            snap.error = Some(format!("git status failed: {}", error.trim()));
            return snap;
        }
    }

    // The forge slug (`owner/name`). Rides this cached probe so the topbar
    // never shells out per frame.
    snap.remote_slug = observed_git_repo(&root);

    if let Ok(log) = git_output(&root, &["log", RECENT_COMMITS, "--format=%h%x1f%s%x1f%cr"]) {
        snap.recent_commits = parse_recent_commits(&log);
    }

    // Worktrees.
    if let Ok(list) = git_output(&root, &["worktree", "list", "--porcelain"]) {
        snap.worktrees = parse_worktree_list(&list);
    }

    snap
}

/// Fold one `git status` reading into `snap`: branch, upstream, ahead/behind
/// and every changed path.
fn apply_porcelain(snap: &mut GitStatusSnapshot, status: PorcelainStatus) {
    snap.detached = status.head.is_none();
    snap.branch = status
        .head
        .clone()
        .or_else(|| status.oid.as_deref().map(|oid| short_oid(oid).to_string()));
    snap.has_upstream = status.upstream.is_some();
    snap.ahead = status.ahead;
    snap.behind = status.behind;
    snap.dirty = !status.changes.is_clean();
    snap.changes = status.changes;
    snap.changed_paths = status.changed_paths;
    snap.changed_path_count = status.changed_path_count;
}

/// Whether a backed-off refresh may re-read only `git status` on top of
/// `prev`. It needs a healthy earlier probe of the same workspace: the rest
/// (repository identity, forge slug, recent commits, worktrees) only changes
/// through something the person or the agent does, and that is activity,
/// which ends the back-off and brings the full probe back.
fn can_probe_status_only(prev: &GitStatusSnapshot, workspace: &Path) -> bool {
    prev.probed_workspace.as_deref() == Some(workspace)
        && prev.error.is_none()
        && prev.root.is_some()
}

/// The back-off probe: one `git status` (two processes) instead of the dozen
/// the full probe starts, spliced into `prev`. Branch, dirty state and
/// ahead/behind still follow whatever happens in another terminal; the
/// commit list and worktree list wait for the next full probe (#6728).
///
/// Known limits: while backed off, a commit or worktree change made in another
/// terminal shows in the branch and dirty state within [`QUIET_PROBE_INTERVAL`]
/// but not in the recent-commit and worktree lists, and the relative ages in
/// those lists stand still, until the next input or engine event brings the
/// full probe back.
fn probe_status_only(prev: &GitStatusSnapshot) -> GitStatusSnapshot {
    let mut snap = prev.clone();
    snap.fetched_at = Some(Instant::now());
    let Some(root) = prev.root.as_deref() else {
        return snap;
    };
    match probe_workspace_status(root) {
        Ok(status) => apply_porcelain(&mut snap, status),
        Err(error) => snap.error = Some(format!("git status failed: {}", error.trim())),
    }
    snap
}

fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
                locked: false,
            });
        } else if let Some(entry) = current.as_mut() {
            if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                entry.branch = Some(branch.to_string());
            } else if line == "bare" {
                entry.bare = true;
            } else if line.starts_with("locked") {
                entry.locked = true;
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    // Shared read policy disables repository-selected helpers and lazy
    // fetch while keeping the sanitized environment and optional locks off.
    let output = Git::review_command(cwd)
        .map_err(|e| format!("{e:#}"))?
        .args(["-c", "log.showSignature=false"])
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    finish_git_output(output)
}

// Explicit writes keep the user's own configured behavior. The shared Git
// command still scrubs parent credentials and never opens a hidden prompt.
fn git_write(cwd: &Path, args: &[&str]) -> Result<String, String> {
    finish_git_output(Git::output(args, cwd).map_err(|e| e.to_string())?)
}

fn finish_git_output(output: std::process::Output) -> Result<String, String> {
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The repository's name (from the common git directory, so a linked
/// worktree names its repository) and whether this checkout is a linked
/// worktree, from one `rev-parse`.
fn repository_identity(worktree_root: &Path) -> (Option<String>, bool) {
    let Ok(paths) = git_output(
        worktree_root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    ) else {
        return (None, false);
    };
    let mut lines = paths.lines().map(str::trim);
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return (None, false);
    };
    (
        repository_name_from_common_dir(worktree_root, Path::new(common_dir)),
        git_dir != common_dir,
    )
}

fn repository_name_from_common_dir(worktree_root: &Path, common_dir: &Path) -> Option<String> {
    let common_dir = if common_dir.is_absolute() {
        common_dir.to_path_buf()
    } else {
        worktree_root.join(common_dir)
    };
    common_dir
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
}

/// Compact chrome label: `CodeWhale · main* ↑2` or
/// `CodeWhale/feature · feature*` for a linked worktree.
///
/// Omits the segment when Git has not named a location or ref. A known
/// location without a branch still renders — the header must not invent a
/// ref to fill the slot.
#[cfg(test)]
#[must_use]
pub fn chrome_label(snap: &GitStatusSnapshot) -> Option<String> {
    let worktree_name = snap
        .root
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy());
    let location = match (snap.repository_name.as_deref(), worktree_name.as_deref()) {
        (Some(repository), Some(worktree)) if repository != worktree => {
            Some(format!("{repository}/{worktree}"))
        }
        (Some(repository), _) => Some(repository.to_string()),
        (None, Some(worktree)) => Some(worktree.to_string()),
        (None, None) => None,
    };
    let mut label = match (location, snap.branch.as_deref()) {
        (Some(location), Some(branch)) => format!("{location} · {branch}"),
        (Some(location), None) => location,
        (None, Some(branch)) => branch.to_string(),
        (None, None) => return None,
    };
    if snap.dirty {
        label.push('*');
    }
    if snap.ahead > 0 {
        label.push_str(&format!(" ↑{}", snap.ahead));
    }
    if snap.behind > 0 {
        label.push_str(&format!(" ↓{}", snap.behind));
    }
    Some(label)
}

/// Status-bar ink for repository chrome. Location is metadata, not a
/// failure — dirtiness is the `*` on the same gray string.
#[cfg(test)]
#[must_use]
pub fn chrome_ink() -> codewhale_palette::ChromeInk {
    codewhale_palette::ChromeInk::Metadata
}

/// Create a new worktree at `path` tracking `branch` (or a new branch name).
pub fn create_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    new_branch: bool,
) -> Result<(), String> {
    let mut args = vec!["worktree", "add"];
    if new_branch {
        args.push("-b");
        args.push(branch);
        args.push(path.to_str().ok_or("invalid path")?);
    } else {
        args.push(path.to_str().ok_or("invalid path")?);
        args.push(branch);
    }
    git_write(repo, &args).map(|_| ())?;
    force_refresh(repo);
    Ok(())
}

/// Build the human-readable workspace context string ("branch | status")
/// from one `git status --porcelain=v2 --branch` call, through the same
/// parser and formatter the Git view uses. Returns `None` if the workspace is
/// not a git repository or git itself is unavailable. The engine's per-turn
/// git line reads this.
pub(crate) fn collect(workspace: &Path) -> Option<String> {
    probe_workspace_status(workspace)
        .ok()
        .as_ref()
        .and_then(status_line)
}

/// Collapse a git remote to `owner/name`. Paths, credentials, and unknown
/// hosts are dropped so the control plane never receives a folder identity.
pub(crate) fn normalize_observed_git_repo(input: &str) -> Option<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }
    let allowed_host = |host: &str| {
        matches!(
            host.to_ascii_lowercase().as_str(),
            "github.com" | "www.github.com" | "gitee.com" | "cnb.cool"
        )
    };
    let path = if let Some((authority, path)) = raw.split_once(':')
        && !raw.contains("://")
        && authority.starts_with("git@")
        && allowed_host(authority.trim_start_matches("git@"))
    {
        path.to_string()
    } else {
        let url = reqwest::Url::parse(raw).ok()?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !allowed_host(url.host_str()?)
        {
            return None;
        }
        url.path().trim_start_matches('/').to_string()
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if owner.len() > 80 || name.len() > 80 {
        return None;
    }
    if !owner
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return None;
    }
    if matches!(owner, "." | "..") || matches!(name, "." | "..") {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

pub(crate) fn observed_git_repo(workspace: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_observed_git_repo(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probed(workspace: &Path, root: &Path) -> GitStatusSnapshot {
        GitStatusSnapshot {
            root: Some(root.to_path_buf()),
            probed_workspace: Some(workspace.to_path_buf()),
            fetched_at: Some(Instant::now()),
            ..GitStatusSnapshot::default()
        }
    }

    /// The cache TTL must actually apply when the session was launched below
    /// the repository top level. Comparing `root` to the workspace made this
    /// permanently stale, so the two-second chrome probe ran unconditionally
    /// and `git status` contended for the user's index lock (#5617).
    #[test]
    fn fresh_snapshot_from_a_subdirectory_is_not_stale() {
        let root = PathBuf::from("/repo");
        let workspace = PathBuf::from("/repo/crates/tui");
        let snap = probed(&workspace, &root);
        assert_ne!(snap.root.as_deref(), Some(workspace.as_path()));
        assert!(
            !snapshot_is_stale(&snap, &workspace),
            "a fresh probe from a subdirectory must satisfy the TTL"
        );
    }

    /// The probe schedule of #6728: two seconds while the session is in use,
    /// fifteen once nothing has touched it for thirty, and the fast cadence
    /// back on the very next tick after any activity.
    #[test]
    fn the_probe_backs_off_only_after_thirty_quiet_seconds() {
        let secs = Duration::from_secs;
        assert_eq!(probe_interval(secs(0)), secs(2));
        assert_eq!(probe_interval(secs(29)), secs(2));
        assert!(!probe_is_backed_off(secs(29)));
        assert_eq!(probe_interval(secs(30)), secs(15));
        assert!(probe_is_backed_off(secs(30)));
        assert_eq!(probe_interval(secs(3_600)), secs(15));
    }

    #[test]
    fn a_probe_is_due_on_its_interval_and_at_once_after_activity() {
        let secs = Duration::from_secs;
        // Never probed: always due.
        assert!(probe_due(None, secs(0)));
        assert!(probe_due(None, secs(600)));
        // In use: the 2 s cadence.
        assert!(!probe_due(Some(secs(1)), secs(5)));
        assert!(probe_due(Some(secs(2)), secs(5)));
        // Quiet: nothing until 15 s.
        assert!(!probe_due(Some(secs(2)), secs(60)));
        assert!(!probe_due(Some(secs(14)), secs(60)));
        assert!(probe_due(Some(secs(15)), secs(60)));
        // The next input resets the quiet clock; a probe 9 s old is now due.
        assert!(!probe_due(Some(secs(9)), secs(60)));
        assert!(probe_due(Some(secs(9)), secs(0)));
    }

    #[test]
    fn a_status_only_refresh_needs_a_healthy_snapshot_of_the_same_workspace() {
        let workspace = PathBuf::from("/repo/crates/tui");
        let healthy = probed(&workspace, Path::new("/repo"));
        assert!(can_probe_status_only(&healthy, &workspace));
        assert!(
            !can_probe_status_only(&healthy, Path::new("/other")),
            "another workspace gets the full probe"
        );
        let mut errored = healthy.clone();
        errored.error = Some("git status failed: boom".into());
        assert!(!can_probe_status_only(&errored, &workspace));
        let mut rootless = healthy;
        rootless.root = None;
        assert!(!can_probe_status_only(&rootless, &workspace));
        assert!(!can_probe_status_only(
            &GitStatusSnapshot::default(),
            &workspace
        ));
    }

    /// The back-off probe runs one `git status` and leaves everything else
    /// the full probe learned in place.
    #[test]
    fn a_status_only_refresh_keeps_identity_commits_and_worktrees() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "first"]);

        let full = probe_status(dir.path());
        assert!(full.error.is_none(), "{:?}", full.error);
        assert!(!full.dirty);
        assert_eq!(full.recent_commits.len(), 1);
        assert!(can_probe_status_only(&full, dir.path()));

        std::fs::write(dir.path().join("b.txt"), "b\n").expect("write");
        let light = probe_status_only(&full);
        assert!(light.error.is_none(), "{:?}", light.error);
        assert!(light.dirty, "the new untracked file shows up");
        assert_eq!(light.changes.untracked, 1);
        assert_eq!(light.branch.as_deref(), Some("main"));
        assert_eq!(light.recent_commits, full.recent_commits);
        assert_eq!(light.worktrees, full.worktrees);
        assert_eq!(light.repository_name, full.repository_name);
        assert_eq!(light.root, full.root);
        assert_eq!(light.probed_workspace, full.probed_workspace);
        assert!(light.fetched_at >= full.fetched_at);
    }

    #[test]
    fn the_quiet_cache_lifetime_stays_below_the_quiet_probe_interval() {
        // Otherwise the loop's own probe would find its previous result
        // fresh and skip, and the cadence would silently double.
        assert!(QUIET_CACHE_TTL < QUIET_PROBE_INTERVAL);
        assert!(QUIET_CACHE_TTL > CACHE_TTL);
    }

    #[test]
    fn a_different_workspace_is_always_stale() {
        let snap = probed(Path::new("/repo/crates/tui"), Path::new("/repo"));
        assert!(snapshot_is_stale(&snap, Path::new("/other")));
    }

    #[test]
    fn an_unprobed_snapshot_is_stale() {
        assert!(snapshot_is_stale(
            &GitStatusSnapshot::default(),
            Path::new("/repo")
        ));
    }

    /// A workspace outside any repository must resolve without spawning git,
    /// and must record its own input so the TTL suppresses the next tick.
    #[test]
    fn non_git_workspace_fast_fails_and_caches_its_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snap = probe_status(dir.path());
        assert_eq!(snap.error.as_deref(), Some("not a git repository"));
        assert_eq!(snap.root, None);
        assert_eq!(snap.probed_workspace.as_deref(), Some(dir.path()));
        assert!(
            !snapshot_is_stale(&snap, dir.path()),
            "the negative result must be cached, not re-probed every tick"
        );
    }

    #[test]
    fn parse_worktree_porcelain() {
        let raw = "\
worktree /repo
HEAD abc
branch refs/heads/main

worktree /repo/.cw-worktrees/feat
HEAD def
branch refs/heads/feat
locked
";
        let entries = parse_worktree_list(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(entries[1].locked);
        assert_eq!(entries[1].branch.as_deref(), Some("feat"));
    }

    #[test]
    fn chrome_label_marks_dirty_and_divergence() {
        let snap = GitStatusSnapshot {
            root: Some("/repo".into()),
            repository_name: Some("repo".into()),
            branch: Some("main".into()),
            dirty: true,
            ahead: 2,
            behind: 1,
            ..GitStatusSnapshot::default()
        };
        assert_eq!(chrome_label(&snap).as_deref(), Some("repo · main* ↑2 ↓1"));
    }

    #[test]
    fn chrome_label_identifies_a_linked_worktree() {
        let snap = GitStatusSnapshot {
            root: Some("/repo/.cw-worktrees/feature".into()),
            repository_name: Some("repo".into()),
            branch: Some("feature".into()),
            dirty: true,
            ..GitStatusSnapshot::default()
        };

        assert_eq!(
            chrome_label(&snap).as_deref(),
            Some("repo/feature · feature*")
        );
    }

    #[test]
    fn chrome_label_omits_dirty_marker_when_clean() {
        let snap = GitStatusSnapshot {
            root: Some("/repo".into()),
            repository_name: Some("repo".into()),
            branch: Some("main".into()),
            dirty: false,
            ..GitStatusSnapshot::default()
        };
        assert_eq!(chrome_label(&snap).as_deref(), Some("repo · main"));
    }

    #[test]
    fn chrome_label_keeps_location_when_the_ref_is_unknown() {
        let snap = GitStatusSnapshot {
            root: Some("/repo/.cw-worktrees/feature".into()),
            repository_name: Some("repo".into()),
            branch: None,
            dirty: true,
            ..GitStatusSnapshot::default()
        };
        assert_eq!(chrome_label(&snap).as_deref(), Some("repo/feature*"));
    }

    #[test]
    fn chrome_label_is_absent_without_a_repo_or_ref() {
        assert_eq!(
            chrome_label(&GitStatusSnapshot {
                error: Some("not a git repository".into()),
                ..GitStatusSnapshot::default()
            }),
            None
        );
    }

    #[test]
    fn chrome_ink_is_metadata_not_failure() {
        assert_eq!(chrome_ink(), codewhale_palette::ChromeInk::Metadata);
        assert_eq!(
            chrome_ink().family(),
            codewhale_palette::SemanticFamily::Neutral
        );
    }

    #[test]
    fn repository_name_uses_the_common_git_directory_for_worktrees() {
        assert_eq!(
            repository_name_from_common_dir(
                Path::new("/repo/.cw-worktrees/feature"),
                Path::new("/repo/.git")
            )
            .as_deref(),
            Some("repo")
        );
        assert_eq!(
            repository_name_from_common_dir(Path::new("/repo"), Path::new(".git")).as_deref(),
            Some("repo")
        );
    }

    #[test]
    fn porcelain_v2_carries_branch_upstream_divergence_and_every_change() {
        let raw = [
            "# branch.oid 1234567890abcdef1234567890abcdef12345678",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 M. N... 100644 100644 100644 aaa bbb src/lib.rs",
            "1 .M N... 100644 100644 100644 aaa bbb docs/a file.md",
            "2 R. N... 100644 100644 100644 aaa bbb R100 new.rs",
            "old.rs",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs",
            "? scratch.txt",
            "! target",
            "",
        ]
        .join("\0");
        let status = parse_porcelain_v2(&raw).expect("porcelain v2");
        assert_eq!(status.head.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(
            status.changes,
            ChangeCounts {
                // The UU record is a conflict only, not also staged and
                // modified (U08-m2).
                staged: 2,
                modified: 1,
                untracked: 1,
                conflicts: 1,
            }
        );
        let paths = status
            .changed_paths
            .iter()
            .map(|path| (path.code.as_str(), path.path.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                ("M ", "src/lib.rs"),
                (" M", "docs/a file.md"),
                ("R ", "new.rs"),
                ("UU", "conflict.rs"),
                ("??", "scratch.txt"),
            ]
        );
        assert_eq!(
            status_line(&status).as_deref(),
            Some("main | 2 staged, 1 modified, 1 untracked, 1 conflicts")
        );
    }

    #[test]
    fn all_unmerged_records_are_conflicts() {
        for code in ["AA", "DD", "AU", "UA", "DU", "UD", "UU"] {
            let raw = format!(
                "# branch.head main\0u {code} N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs\0"
            );
            let status = parse_porcelain_v2(&raw).unwrap();
            assert_eq!(status.changes.conflicts, 1, "{code}");
            assert_eq!(
                (status.changes.staged, status.changes.modified),
                (0, 0),
                "{code} is not also staged or modified"
            );
            assert!(status_line(&status).unwrap().contains("1 conflicts"));
            let (legacy, _, _) = parse_porcelain_v1(&format!("{code} conflict.rs\n"));
            assert_eq!(legacy.conflicts, 1, "legacy {code}");
            assert_eq!((legacy.staged, legacy.modified), (0, 0), "legacy {code}");
        }
    }

    #[test]
    fn changed_path_count_survives_the_display_cap() {
        for count in [1, MAX_CHANGED_PATHS, MAX_CHANGED_PATHS + 1] {
            let mut raw = "# branch.head main\0".to_string();
            let mut legacy = String::new();
            for index in 0..count {
                raw.push_str(&format!(
                    "1 MM N... 100644 100644 100644 aaa bbb file-{index}\0"
                ));
                legacy.push_str(&format!("MM file-{index}\n"));
            }
            let status = parse_porcelain_v2(&raw).unwrap();
            assert_eq!(status.changed_path_count, count);
            assert_eq!(status.changed_paths.len(), count.min(MAX_CHANGED_PATHS));
            let (_, paths, path_count) = parse_porcelain_v1(&legacy);
            assert_eq!(path_count, count);
            assert_eq!(paths.len(), count.min(MAX_CHANGED_PATHS));
        }
    }

    #[test]
    fn porcelain_v2_detached_and_unborn_heads() {
        let detached =
            parse_porcelain_v2("# branch.oid 1234567890abcdef\0# branch.head (detached)\0")
                .expect("v2");
        assert_eq!(detached.head, None);
        assert_eq!(
            status_line(&detached).as_deref(),
            Some("detached:1234567 | clean")
        );
        let unborn =
            parse_porcelain_v2("# branch.oid (initial)\0# branch.head main\0").expect("v2");
        assert_eq!(unborn.oid, None);
        assert_eq!(status_line(&unborn).as_deref(), Some("main | clean"));
        // Porcelain v1 (a git too old for v2) has no branch header: the
        // caller falls back to the old calls.
        assert_eq!(parse_porcelain_v2(" M src/lib.rs\n?? new.rs\n"), None);
        let (counts, paths, _) = parse_porcelain_v1(" M src/lib.rs\n?? new.rs\n");
        assert_eq!((counts.modified, counts.untracked), (1, 1));
        assert_eq!(paths[1].path, "new.rs");
    }

    #[test]
    fn recent_commits_parse_the_unit_separated_log() {
        let commits = parse_recent_commits("abc1234\u{1f}fix: a thing\u{1f}3 hours ago\nbad\n");
        assert_eq!(
            commits,
            [RecentCommit {
                hash: "abc1234".to_string(),
                subject: "fix: a thing".to_string(),
                when: "3 hours ago".to_string(),
            }]
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let mut all = vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend_from_slice(args);
        git_write(dir, &all).expect("git");
    }

    /// Even status may run repository-selected fsmonitor or clean-filter
    /// code. A background read must neither run it nor expose parent env.
    #[cfg(unix)]
    #[test]
    fn automatic_git_status_does_not_execute_repository_helpers() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::test_support::lock_test_env();
        let _sentinel = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_GIT_POLL_SECRET",
            "git-poll-sentinel",
        );
        let dir = tempfile::tempdir().expect("repo");
        let hooks = tempfile::tempdir().expect("private hooks");
        let repo = dir.path();
        git(repo, &["init", "--initial-branch=main"]);
        std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=fixture\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", "first commit"]);
        let fsmonitor_seen = hooks.path().join("fsmonitor-seen");
        let filter_seen = hooks.path().join("filter-seen");
        for (name, marker, ending) in [
            ("fsmonitor.sh", &fsmonitor_seen, "exit 1"),
            ("clean.sh", &filter_seen, "cat"),
        ] {
            let script = hooks.path().join(name);
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf 'leak=%s\\n' \"${{CODEWHALE_TEST_GIT_POLL_SECRET-unset}}\" >> '{}'\n{ending}\n",
                    marker.display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(
            repo,
            &[
                "config",
                "core.fsmonitor",
                &hooks.path().join("fsmonitor.sh").to_string_lossy(),
            ],
        );
        git(
            repo,
            &[
                "config",
                "filter.fixture.clean",
                &hooks.path().join("clean.sh").to_string_lossy(),
            ],
        );
        git(repo, &["config", "filter.fixture.required", "true"]);
        // Same length forces content comparison instead of the cheap size check.
        std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();
        let snapshot = probe_status(repo);
        assert_eq!(snapshot.error, None);
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert!(
            snapshot.dirty && snapshot.changes.modified >= 1,
            "{snapshot:?}"
        );
        for marker in [&fsmonitor_seen, &filter_seen] {
            assert!(
                !marker.exists(),
                "automatic read ran a repository helper: {}",
                std::fs::read_to_string(marker).unwrap_or_default(),
            );
        }
    }

    /// log.showSignature can run the repository's gpg.program even with
    /// captured stdout. Recent-commit polling never requests verification.
    #[cfg(unix)]
    #[test]
    fn automatic_git_log_does_not_execute_a_signature_helper() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;
        let _lock = crate::test_support::lock_test_env();
        let _sentinel = crate::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_GIT_POLL_SECRET",
            "git-poll-sentinel",
        );
        let dir = tempfile::tempdir().expect("repo");
        let hooks = tempfile::tempdir().expect("private hooks");
        let repo = dir.path();
        git(repo, &["init", "--initial-branch=main"]);
        std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", "first commit"]);
        let unsigned = git_write(repo, &["cat-file", "commit", "HEAD"]).unwrap();
        let (headers, message) = unsigned.split_once("\n\n").unwrap();
        let signed = format!(
            "{headers}\ngpgsig -----BEGIN PGP SIGNATURE-----\n fixture\n -----END PGP SIGNATURE-----\n\n{message}"
        );
        let mut child = Git::command()
            .expect("git available")
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(signed.as_bytes())
            .unwrap();
        let object = child.wait_with_output().unwrap();
        assert!(object.status.success());
        let oid = String::from_utf8(object.stdout).unwrap();
        git(repo, &["update-ref", "HEAD", oid.trim()]);
        let marker = hooks.path().join("signature-seen");
        let script = hooks.path().join("signature.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'leak=%s\\n' \"${{CODEWHALE_TEST_GIT_POLL_SECRET-unset}}\" >> '{}'\nexit 1\n",
                marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(repo, &["config", "gpg.program", &script.to_string_lossy()]);
        git(
            repo,
            &["config", "gpg.openpgp.program", &script.to_string_lossy()],
        );
        git(repo, &["config", "log.showSignature", "true"]);
        let log = git_output(repo, &["log", "-1", "--format=%s"]).unwrap();
        assert!(log.contains("first commit"), "{log}");
        assert!(
            !marker.exists(),
            "automatic log ran a signature helper: {}",
            std::fs::read_to_string(marker).unwrap_or_default(),
        );
    }

    /// One probe of a real repository: branch, changes, commits, and the
    /// badge string, through the single porcelain v2 status call.
    #[test]
    fn a_probe_of_a_real_repository_fills_the_git_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        git(repo, &["init", "--initial-branch=main"]);
        std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
        git(repo, &["add", "tracked.txt"]);
        git(repo, &["commit", "-m", "first commit"]);
        std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();

        let snap = probe_status(repo);
        assert_eq!(snap.error, None);
        assert_eq!(snap.branch.as_deref(), Some("main"));
        assert!(!snap.detached && !snap.has_upstream && !snap.is_linked_worktree);
        assert_eq!((snap.changes.modified, snap.changes.untracked), (1, 1));
        assert!(snap.dirty);
        assert_eq!(snap.changed_path_count, 2);
        assert_eq!(snap.recent_commits.len(), 1);
        assert_eq!(snap.recent_commits[0].subject, "first commit");
        assert_eq!(
            context_line(&snap).as_deref(),
            Some("main | 1 modified, 1 untracked")
        );
        // The engine's per-turn line reads the same parser and formatter.
        assert_eq!(
            crate::git_status::collect(repo).as_deref(),
            Some("main | 1 modified, 1 untracked")
        );
    }
}

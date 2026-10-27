//! Workspace git surface for native clients (APPS-106).
//!
//! One authority: these routes run `git` against the server's configured
//! workspace through the same hardened primitives the agent tools use —
//! diffs and token reads go through [`Git::review_command`] (filters,
//! fsmonitor, hooks, lazy fetches and replace-objects neutralized), writes run through
//! [`Git::tokio_command`] with interactive prompts disabled so a credential
//! or host-key prompt can never hang an HTTP request. There is no second
//! index, cache, or diff store here; every response is computed live from
//! the repository. Status uses normal Git filters and untracked settings,
//! matching the workspace counts and operator writes.
//!
//! Routes (workspace-scoped, matching the client contract):
//!   GET  /v1/git           — branch/head/ahead-behind, per-file porcelain
//!                            entries, local branches and remotes
//!   GET  /v1/changes       — the file-change inventory only (status rows)
//!   GET  /v1/diff?path=    — unified diff for one workspace-relative file
//!   GET  /v1/workspace/diff — bounded whole-tree diff + per-file numstat
//!   GET  /v1/git/graph     — recent commit graph rows (bounded)
//!   POST /v1/git/stage     — `{ "paths": [...] }` or `{ "all": true }`
//!   POST /v1/git/unstage   — `{ "paths": [...] }` or `{ "all": true }`
//!   POST /v1/git/discard   — `{ "paths": [...] }` (tracked only; no `all`)
//!   POST /v1/git/commit    — `{ "message": "…", "all": false }`
//!   POST /v1/git/push      — `{ "remote"?, "set_upstream"?: bool }`
//!   POST /v1/git/branch    — `{ "name": "…", "create"?: bool }`
//!
//! Mutations answer with the command output tail plus a refreshed workspace
//! status and the full `current` detail so the client re-renders in one
//! round trip. The caller holds the operator token; the repository's own
//! hooks run for `commit` exactly as they would for the user's terminal.
//!
//! Preconditions (#6647): the detail read carries `head_oid`, `index_token`,
//! a whole-tree `revision` and a per-row `files[].rev`. Stage, unstage,
//! discard and commit accept an optional `expect` built from those values;
//! when the repository no longer matches, the route answers 409
//! `git_state_changed` with the current detail and writes nothing. Every
//! path in this module — request paths, `files[].path`, `expect.files`
//! keys — is workspace-relative; the one translation from git's
//! repository-root frame is [`workspace_frame_path`]. Status is best-effort:
//! unreadable rows have no token, and incomplete reads have no whole-tree
//! revision. These guards do not authorize content the read could not hash.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use codewhale_dependencies::{ExternalTool as _, Git};

use super::workspace::{
    FILE_SERVE_MAX_BYTES, canonical_workspace, collect_workspace_status, content_revision,
    relative_request_path,
};
use super::{ApiError, RuntimeApiState};

/// Generous bound for local git work on very large repositories.
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Pushes cross the network; still bounded so a dead remote cannot pin a
/// handler forever.
const GIT_WRITE_TIMEOUT: Duration = Duration::from_secs(120);
/// Output tail carried back to the client for mutations.
const MAX_OUTPUT_TAIL: usize = 8 * 1024;
/// Commit-graph row bounds.
const GRAPH_LIMIT_DEFAULT: usize = 100;
const GRAPH_LIMIT_MAX: usize = 500;
/// Unified-diff response caps: a single file gets a generous window; the
/// whole-tree surface defaults smaller and always reports `truncated`.
const FILE_DIFF_MAX_BYTES: usize = 512 * 1024;
const WORKSPACE_DIFF_DEFAULT_BYTES: usize = 256 * 1024;
const WORKSPACE_DIFF_MAX_BYTES: usize = 4 * 1024 * 1024;
/// The empty tree — diff base for repositories whose HEAD is unborn.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Branch/commit message caps — generous for real messages, hostile to
/// accidental binary paste.
const MAX_COMMIT_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_BRANCH_NAME_BYTES: usize = 256;
const MAX_PATH_ARGS: usize = 512;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
/// Per-read bound on working-tree bytes hashed for `files[].rev`. Rows past
/// the budget get a stat fingerprint (`s-` token) instead of a content digest
/// (`c-` token), so a large untracked tree cannot turn a status poll into
/// gigabytes of reads. Stat tokens are display-only: guarded writes refuse
/// them, and a tree containing one has no whole-tree revision.
const REV_CONTENT_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const REV_CONTENT_BUDGET_FILES: usize = 4_096;

// ---------------------------------------------------------------------------
// git invocation
// ---------------------------------------------------------------------------

struct GitRun {
    status_success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Hardened read path: the same primitive review/tooling uses, so fsmonitor,
/// content filters, hooks, lazy fetches and replace-objects cannot run inside
/// an HTTP read either.
async fn git_read(workspace: &FsPath, args: &[&str]) -> Result<GitRun, ApiError> {
    let workspace = workspace.to_path_buf();
    let command = tokio::task::spawn_blocking(move || Git::review_command(&workspace))
        .await
        .map_err(|_| ApiError::internal("git read setup failed"))?
        .map_err(|error| ApiError::internal(format!("git is unavailable: {error}")))?;
    let mut command = tokio::process::Command::from(command);
    command.args(args);
    finish_git(
        codewhale_runtime::process_tree::contained_output(&mut command),
        GIT_READ_TIMEOUT,
    )
    .await
}

/// Write path for operator-driven mutations. Non-interactive by contract:
/// [`Git::tokio_command`] carries the shared no-prompt environment
/// ([`codewhale_dependencies::apply_git_noninteractive_env`]) so a credential or
/// key prompt can never hang the request. Hooks and filters run exactly as they do for the user's own
/// `git` — a Review-sheet commit is the user's commit.
async fn git_write(workspace: &FsPath, args: Vec<String>) -> Result<GitRun, ApiError> {
    let mut command = Git::tokio_command()
        .ok_or_else(|| ApiError::internal("git is not installed or not in PATH"))?;
    command.args(&args).current_dir(workspace);
    // Contained: hooks and filters run here, and a timeout or a dropped
    // request must end them too, not only the `git` process itself.
    finish_git(
        codewhale_runtime::process_tree::contained_output(&mut command),
        GIT_WRITE_TIMEOUT,
    )
    .await
}

async fn finish_git(
    output: impl std::future::Future<Output = std::io::Result<std::process::Output>>,
    timeout: Duration,
) -> Result<GitRun, ApiError> {
    let output = tokio::time::timeout(timeout, output)
        .await
        .map_err(|_| ApiError::internal("git operation timed out"))?
        .map_err(|error| ApiError::internal(format!("failed to run git: {error}")))?;
    Ok(GitRun {
        status_success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn output_tail(run: &GitRun) -> String {
    let mut text = String::new();
    for part in [run.stdout.trim(), run.stderr.trim()] {
        if !part.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part);
        }
    }
    if text.len() > MAX_OUTPUT_TAIL {
        let mut boundary = text.len() - MAX_OUTPUT_TAIL;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text = text[boundary..].to_string();
    }
    text
}

/// A non-repo workspace is a 404, not a 500: `rev-parse --is-inside-work-tree`
/// exits 128 there, so probe directly rather than through the erroring helper.
fn require_repo(workspace: &FsPath) -> Result<(), ApiError> {
    match Git::output(&["rev-parse", "--is-inside-work-tree"], workspace) {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "true" =>
        {
            Ok(())
        }
        Ok(_) => Err(ApiError::not_found("workspace is not a git repository")),
        Err(error) => Err(ApiError::internal(format!("failed to run git: {error}"))),
    }
}

/// Normal Git reads for repository chrome and filter-aware status, matching
/// the workspace counts and operator writes.
fn run_git_sync(workspace: &FsPath, args: &[&str]) -> Result<String, ApiError> {
    let output = Git::output(args, workspace)
        .map_err(|error| ApiError::internal(format!("failed to run git: {error}")))?;
    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// GET /v1/git — status detail
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
struct GitFileEntry {
    /// Workspace-relative, like every other path on this surface.
    path: String,
    /// Raw porcelain v1 index (X) and worktree (Y) columns.
    index: String,
    worktree: String,
    /// True when the index column records a change.
    staged: bool,
    /// Leading human state: modified / added / deleted / renamed /
    /// typechange / untracked / conflicted / ignored / unknown (unreadable submodule).
    status: &'static str,
    /// Rename/copy source, when it lies inside the workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
    /// Opaque per-row precondition token: this row's index entries plus the
    /// working-tree state of every file it covers (a rename's source and
    /// every file under a collapsed untracked directory included).
    rev: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct GitStatusDetailResponse {
    git_repo: bool,
    workspace: PathBuf,
    branch: Option<String>,
    detached: bool,
    /// Abbreviated HEAD for display. Preconditions use `head_oid`.
    head: Option<String>,
    /// Full HEAD commit id; null on an unborn branch.
    head_oid: Option<String>,
    /// Opaque token for the whole index (every stage entry, repository-wide).
    index_token: Option<String>,
    /// Opaque token for the whole tree: HEAD, the index and every row.
    revision: Option<String>,
    ahead: Option<u32>,
    behind: Option<u32>,
    staged: usize,
    unstaged: usize,
    untracked: usize,
    files: Vec<GitFileEntry>,
    branches: Vec<String>,
    remotes: Vec<String>,
    remote_repositories: Vec<Value>,
    branch_preconditions: bool,
    diff_options: bool,
}

pub(super) async fn git_status_detail(
    State(state): State<RuntimeApiState>,
) -> Result<Json<GitStatusDetailResponse>, ApiError> {
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || collect_git_status_detail(&workspace))
        .await
        .map_err(|_| ApiError::internal("git status failed"))?
        .map(Json)
}

fn collect_git_status_detail(workspace: &FsPath) -> Result<GitStatusDetailResponse, ApiError> {
    let status = collect_workspace_status(workspace);
    let mut detail = GitStatusDetailResponse {
        git_repo: status.git_repo,
        workspace: workspace.to_path_buf(),
        branch: status.branch.clone(),
        detached: false,
        head: status.head,
        head_oid: None,
        index_token: None,
        revision: None,
        ahead: status.ahead,
        behind: status.behind,
        staged: status.staged,
        unstaged: status.unstaged,
        untracked: status.untracked,
        files: Vec::new(),
        branches: Vec::new(),
        remotes: Vec::new(),
        remote_repositories: Vec::new(),
        branch_preconditions: true,
        diff_options: true,
    };
    if !status.git_repo {
        return Ok(detail);
    }
    detail.detached = status.branch.is_none()
        || status
            .branch
            .as_deref()
            .is_some_and(|branch| branch.starts_with("detached@"));

    if let Ok(branches) = run_git_sync(workspace, &["branch", "--format=%(refname:short)"]) {
        detail.branches = branches
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Ok(remotes) = run_git_sync(workspace, &["remote"]) {
        detail.remotes = remotes
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
    }
    detail.head_oid = head_oid(workspace).ok().flatten();
    for name in &detail.remotes {
        if let Ok(remote) = run_git_sync(workspace, &["remote", "get-url", "--", name]) {
            let remote = remote.trim();
            let path = remote
                .strip_prefix("git@github.com:")
                .map(str::to_string)
                .or_else(|| {
                    url::Url::parse(remote)
                        .ok()
                        .filter(|url| {
                            matches!(url.scheme(), "https" | "ssh")
                                && url.host_str() == Some("github.com")
                                && url.query().is_none()
                                && url.fragment().is_none()
                        })
                        .map(|url| url.path().trim_start_matches('/').to_string())
                });
            if let Some(path) = path {
                let repo = path.trim_end_matches('/').trim_end_matches(".git");
                let parts: Vec<_> = repo.split('/').collect();
                if parts.len() == 2
                    && parts.iter().all(|part| {
                        !part.is_empty()
                            && part.len() <= 100
                            && *part != "."
                            && *part != ".."
                            && part
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                    })
                {
                    detail
                        .remote_repositories
                        .push(json!({"name": name, "provider": "github", "repo": repo}));
                }
            }
        }
    }
    if let Ok(frame) = RepoFrame::read(workspace)
        && let Ok((rows, complete)) = status_rows(workspace, &frame)
    {
        let (inside, outside) = split_by_workspace(rows, &frame.prefix);
        detail.files = inside;
        if let Ok(tokens) = compute_tokens(workspace, &frame, &mut detail.files, &outside) {
            detail.head_oid = tokens.head_oid;
            detail.index_token = Some(tokens.index_token);
            detail.revision = tokens.revision.filter(|_| complete);
        }
    }
    Ok(detail)
}

/// Preserve normal filters and untracked mode. Only a collapsed row naming
/// the workspace itself needs expansion to give clients addressable paths.
fn status_rows(
    workspace: &FsPath,
    frame: &RepoFrame,
) -> Result<(Vec<GitFileEntry>, bool), ApiError> {
    let mut complete = true;
    let mut rows = match run_git_sync(workspace, &["status", "--porcelain=v1", "-z"]) {
        Ok(porcelain) => parse_porcelain(&porcelain),
        Err(_) => {
            // One broken submodule can abort all of Git's porcelain output.
            // Recover ordinary rows, then inspect gitlinks individually so
            // only the unreadable submodule is shown as unknown/unguardable.
            let porcelain = run_git_sync(
                workspace,
                &["status", "--porcelain=v1", "-z", "--ignore-submodules=all"],
            )?;
            let mut rows = parse_porcelain(&porcelain);
            for (path, records) in IndexSnapshot::read(frame)?.by_path {
                if !records
                    .split(|byte| *byte == 0)
                    .any(|record| record.starts_with(b"160000 "))
                {
                    continue;
                }
                rows.retain(|row| normalized_row_path(&row.path) != path);
                match run_git_sync(
                    &frame.toplevel,
                    &[
                        "--literal-pathspecs",
                        "status",
                        "--porcelain=v1",
                        "-z",
                        "--",
                        &path,
                    ],
                ) {
                    Ok(porcelain) => rows.extend(parse_porcelain(&porcelain)),
                    Err(_) => {
                        complete = false;
                        rows.push(GitFileEntry {
                            path,
                            index: " ".to_string(),
                            worktree: "?".to_string(),
                            staged: false,
                            status: "unknown",
                            old_path: None,
                            rev: None,
                        });
                    }
                }
            }
            rows
        }
    };
    if let Some(position) = rows
        .iter()
        .position(|row| row.status == "untracked" && row.path == frame.prefix)
    {
        match untracked_paths(&frame.toplevel, &[frame.prefix.as_str()]) {
            Ok(paths) => {
                rows.remove(position);
                for path in paths {
                    rows.extend(parse_porcelain(&format!("?? {path}\0")));
                }
            }
            Err(_) => complete = false,
        }
    }
    Ok((rows, complete))
}

fn untracked_paths(workspace: &FsPath, members: &[&str]) -> Result<BTreeSet<String>, ApiError> {
    if members.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut args = vec![
        "--literal-pathspecs",
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend_from_slice(members);
    let others = review_sync_ok(workspace, &args)?;
    Ok(others
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// Porcelain rows in git's frame: paths are relative to the repository root.
fn parse_porcelain(porcelain: &str) -> Vec<GitFileEntry> {
    let mut entries = Vec::new();
    let mut records = porcelain.split('\0').peekable();
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with("## ") {
            continue;
        }
        if record.len() < 4 {
            continue;
        }
        let index = record.as_bytes()[0] as char;
        let worktree = record.as_bytes()[1] as char;
        let path = record[3..].to_string();
        let mut old_path = None;
        if matches!(index, 'R' | 'C') {
            old_path = records.next().map(str::to_string);
        }
        entries.push(GitFileEntry {
            path,
            index: index.to_string(),
            worktree: worktree.to_string(),
            staged: !matches!(index, ' ' | '?' | '!'),
            status: porcelain_status(index, worktree),
            old_path,
            rev: None,
        });
    }
    entries
}

/// Move repository-root rows into the workspace frame. Omitted paths still
/// feed the whole-tree revision because stage-all and commit reach them.
/// A rename leaving the workspace is shown as its source deletion.
fn split_by_workspace(
    rows: Vec<GitFileEntry>,
    prefix: &str,
) -> (Vec<GitFileEntry>, Vec<GitFileEntry>) {
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    for mut row in rows {
        let old_path = row
            .old_path
            .as_deref()
            .and_then(|old| workspace_frame_path(prefix, old));
        match workspace_frame_path(prefix, &row.path) {
            Some(path) => {
                if row.old_path.is_some() && old_path.is_none() {
                    outside.push(row.clone());
                }
                row.path = path;
                row.old_path = old_path;
                inside.push(row);
            }
            None => {
                if row.index == "R"
                    && let Some(path) = old_path
                {
                    inside.push(GitFileEntry {
                        path,
                        index: "D".to_string(),
                        worktree: " ".to_string(),
                        staged: true,
                        status: "deleted",
                        old_path: None,
                        rev: None,
                    });
                }
                outside.push(row);
            }
        }
    }
    (inside, outside)
}

/// The one translation from git's repository-root frame to the workspace
/// frame. `None` means the path lies outside the workspace.
fn workspace_frame_path(prefix: &str, root_path: &str) -> Option<String> {
    let path = root_path.strip_prefix(prefix)?;
    (!path.is_empty()).then(|| path.to_string())
}

/// A row path as a precondition key: git's collapsed-directory `dir/` and a
/// request's `dir` name the same thing.
fn normalized_row_path(path: &str) -> &str {
    path.trim_end_matches('/')
}

// ---------------------------------------------------------------------------
// Precondition tokens (#6647)
// ---------------------------------------------------------------------------

/// Hardened one-shot read for token material: the review command, so
/// fsmonitor, filters, hooks and replace-objects cannot run (or alter what
/// the token sees) during a precondition read.
fn review_sync(cwd: &FsPath, args: &[&str]) -> Result<std::process::Output, ApiError> {
    #[cfg(test)]
    tests::GIT_READS.with(|count| count.set(count.get() + 1));
    let mut command = Git::review_command(cwd)
        .map_err(|error| ApiError::internal(format!("git is unavailable: {error}")))?;
    command
        .args(args)
        .output()
        .map_err(|error| ApiError::internal(format!("failed to run git: {error}")))
}

fn review_sync_ok(cwd: &FsPath, args: &[&str]) -> Result<Vec<u8>, ApiError> {
    let output = review_sync(cwd, args)?;
    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git {} failed: {}",
            args.iter()
                .find(|arg| !arg.starts_with("--"))
                .copied()
                .unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Where the workspace sits inside its repository.
struct RepoFrame {
    toplevel: PathBuf,
    /// `sub/dir/` for a subdirectory workspace, empty at the root.
    prefix: String,
}

impl RepoFrame {
    fn read(workspace: &FsPath) -> Result<Self, ApiError> {
        let output = review_sync_ok(
            workspace,
            &["rev-parse", "--show-toplevel", "--show-prefix"],
        )?;
        let text = String::from_utf8_lossy(&output);
        let mut lines = text.lines();
        let toplevel = lines
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .ok_or_else(|| ApiError::internal("git rev-parse returned no toplevel"))?;
        Ok(Self {
            toplevel: PathBuf::from(toplevel),
            prefix: lines
                .next()
                .unwrap_or("")
                .trim_end_matches('\n')
                .to_string(),
        })
    }
}

/// Full HEAD commit id, `None` on an unborn branch.
fn head_oid(workspace: &FsPath) -> Result<Option<String>, ApiError> {
    let output = review_sync(workspace, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])?;
    if output.status.success() {
        let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !oid.is_empty() {
            return Ok(Some(oid));
        }
    } else {
        let symbolic = review_sync(workspace, &["symbolic-ref", "-q", "HEAD"])?;
        if symbolic.status.success() {
            let reference = String::from_utf8_lossy(&symbolic.stdout);
            let reference = reference.trim();
            if reference.starts_with("refs/heads/") {
                // for-each-ref warns when it ignores a broken ref. Only a
                // clean, empty enumeration establishes an unborn branch.
                let refs = review_sync(
                    workspace,
                    &["for-each-ref", "--format=%(refname)", reference],
                )?;
                if refs.status.success() && refs.stdout.is_empty() && refs.stderr.is_empty() {
                    return Ok(None);
                }
            }
        }
    }
    Err(ApiError::internal(
        "git HEAD does not resolve to a commit or an unborn branch",
    ))
}

/// The semantic index (mode, blob, stage, path — never the stat cache that
/// `git status` rewrites), read once from the repository root with no
/// pathspec so it covers every entry, including unmerged stages and entries
/// outside a subdirectory workspace.
struct IndexSnapshot {
    token: String,
    /// Repository-root path → that path's raw `ls-files --stage` records.
    by_path: BTreeMap<String, Vec<u8>>,
    /// One scoped inventory, loaded only for directory members. None means
    /// enumeration failed or was not requested; directory tokens fail closed.
    untracked: Option<BTreeSet<String>>,
}

impl IndexSnapshot {
    fn read(frame: &RepoFrame) -> Result<Self, ApiError> {
        let raw = review_sync_ok(&frame.toplevel, &["ls-files", "--stage", "-z"])?;
        let mut by_path: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for record in raw.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                continue;
            };
            let root_path = String::from_utf8_lossy(&record[tab + 1..]);
            let slot = by_path.entry(root_path.into_owned()).or_default();
            slot.extend_from_slice(record);
            slot.push(0);
        }
        Ok(Self {
            token: content_revision(&raw),
            by_path,
            untracked: None,
        })
    }

    fn load_untracked(&mut self, workspace: &FsPath, members: &[String]) {
        let directories: Vec<&str> = members
            .iter()
            .filter(|member| {
                !has_linked_ancestor(workspace, member)
                    && std::fs::symlink_metadata(workspace.join(member))
                        .is_ok_and(|metadata| metadata.is_dir())
                    && !workspace.join(member).join(".git").exists()
            })
            .map(String::as_str)
            .collect();
        self.untracked = untracked_paths(workspace, &directories).ok();
    }

    /// Records at `member` or anywhere below it, in path order.
    fn records_under<'a>(&'a self, member: &'a str) -> impl Iterator<Item = (&'a str, &'a [u8])> {
        self.by_path
            .range::<str, _>((
                std::ops::Bound::Included(member),
                std::ops::Bound::Unbounded,
            ))
            .take_while(move |(path, _)| path.starts_with(member))
            .filter(move |(path, _)| is_at_or_under(path, member))
            .map(|(path, records)| (path.as_str(), records.as_slice()))
    }
}

fn is_at_or_under(path: &str, member: &str) -> bool {
    path == member
        || path
            .strip_prefix(member)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevMode {
    /// Working-tree files are identified by a sha256 of their bytes (the
    /// Files routes' content revision).
    Content,
    /// Display-only size + mtime (oversized files or past the read budget).
    Stat,
}

impl RevMode {
    fn tag(self) -> &'static str {
        match self {
            Self::Content => "c-",
            Self::Stat => "s-",
        }
    }
}

/// One member path of a row (the row itself, or a rename's source) with
/// everything its token covers.
struct MemberListing {
    member: String,
    index_records: Vec<u8>,
    /// Repository-root files whose working-tree state is part of the token.
    files: BTreeSet<String>,
}

fn list_member(
    workspace: &FsPath,
    index: &IndexSnapshot,
    member: &str,
) -> Result<MemberListing, ApiError> {
    let mut listing = MemberListing {
        member: member.to_string(),
        index_records: Vec::new(),
        files: BTreeSet::new(),
    };
    for (path, records) in index.records_under(member) {
        listing.index_records.extend_from_slice(records);
        listing.files.insert(path.to_string());
    }
    // A path beyond a symlinked directory is not part of this repository's
    // worktree; never read through the link.
    if has_linked_ancestor(workspace, member) {
        return Err(ApiError::internal(
            "cannot fingerprint through a symlinked directory",
        ));
    }
    match std::fs::symlink_metadata(workspace.join(member)) {
        Ok(metadata) if metadata.is_dir() && workspace.join(member).join(".git").exists() => {
            listing.files.insert(member.to_string());
        }
        Ok(metadata) if metadata.is_dir() => {
            // Untracked files below a directory (a collapsed `dir/` row, or a
            // directory named in a request): exactly what `git add dir`
            // would pick up, with the same ignore rules.
            let untracked = index
                .untracked
                .as_ref()
                .ok_or_else(|| ApiError::internal("cannot enumerate untracked directory"))?;
            for path in untracked.range(member.to_string()..) {
                if !path.starts_with(member) {
                    break;
                }
                if is_at_or_under(path, member) {
                    listing.files.insert(normalized_row_path(path).to_string());
                }
            }
        }
        Ok(_) => {
            listing.files.insert(member.to_string());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ApiError::internal(format!(
                "cannot inspect {member}: {error}"
            )));
        }
    }
    Ok(listing)
}

fn has_linked_ancestor(workspace: &FsPath, member: &str) -> bool {
    let mut current = workspace.to_path_buf();
    let mut parts = member.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

fn stat_fingerprint(metadata: &std::fs::Metadata) -> String {
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    format!("stat:{}:{mtime_ns}", metadata.len())
}

/// A file's working-tree identity. Links are never followed.
fn worktree_fingerprint(workspace: &FsPath, path: &str, mode: RevMode) -> Result<String, ApiError> {
    if has_linked_ancestor(workspace, path) {
        return Err(ApiError::internal(
            "cannot fingerprint through a symlinked directory",
        ));
    }
    let full = workspace.join(path);
    let unreadable = |error| ApiError::internal(format!("cannot fingerprint {path}: {error}"));
    let metadata = match std::fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok("absent".into()),
        Err(error) => return Err(unreadable(error)),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return std::fs::read_link(&full)
            .map(|target| format!("link:{}", target.to_string_lossy()))
            .map_err(unreadable);
    }
    if file_type.is_dir() {
        // Gitlinks (including newly added nested repositories) record HEAD.
        // Dirty submodule contents are summarized by status, not recursively
        // hashed: ordinary add/checkout do not write those contents.
        if full.join(".git").exists() {
            let head = head_oid(&full)?;
            let status = review_sync_ok(
                &full,
                &["status", "--porcelain=v1", "-z", "--ignore-submodules=none"],
            )?;
            return Ok(format!(
                "gitlink:{}:{}",
                head.as_deref().unwrap_or("unborn"),
                content_revision(&status)
            ));
        }
        return Ok("dir".into());
    }
    if !file_type.is_file() {
        return Err(ApiError::internal(format!(
            "cannot fingerprint special file {path}"
        )));
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o100 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    let fingerprint = if mode == RevMode::Stat || metadata.len() > FILE_SERVE_MAX_BYTES {
        stat_fingerprint(&metadata)
    } else {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        std::fs::File::open(&full)
            .and_then(|file| file.take(FILE_SERVE_MAX_BYTES + 1).read_to_end(&mut bytes))
            .map_err(unreadable)?;
        if bytes.len() as u64 > FILE_SERVE_MAX_BYTES {
            stat_fingerprint(&metadata)
        } else {
            content_revision(&bytes)
        }
    };
    Ok(format!("{fingerprint}:exec:{executable}"))
}

fn members_of(path: &str, old_path: Option<&str>) -> Vec<String> {
    let mut members = BTreeSet::new();
    members.insert(normalized_row_path(path).to_string());
    if let Some(old) = old_path {
        members.insert(normalized_row_path(old).to_string());
    }
    members.into_iter().collect()
}

fn row_rev(
    workspace: &FsPath,
    listings: &[MemberListing],
    mode: RevMode,
) -> Result<String, ApiError> {
    let mut effective_mode = mode;
    let mut hasher = Sha256::new();
    hasher.update(b"cw-git-rev-1\0");
    for listing in listings {
        hasher.update(b"member\0");
        hasher.update(listing.member.as_bytes());
        hasher.update(b"\0index\0");
        hasher.update(&listing.index_records);
        hasher.update(b"\0worktree\0");
        for file in &listing.files {
            hasher.update(file.as_bytes());
            hasher.update(b"\0");
            let fingerprint = worktree_fingerprint(workspace, file, mode)?;
            if fingerprint.starts_with("stat:") {
                effective_mode = RevMode::Stat;
            }
            hasher.update(fingerprint.as_bytes());
            hasher.update(b"\0");
        }
    }
    Ok(format!(
        "{}{}",
        effective_mode.tag(),
        hex(&hasher.finalize())
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Running content-hash budget for one read.
#[derive(Default)]
struct RevBudget {
    bytes: u64,
    files: usize,
}

impl RevBudget {
    /// Content mode while the whole row still fits; otherwise stat mode for
    /// the whole row (a token has exactly one mode).
    fn choose(&mut self, workspace: &FsPath, listings: &[MemberListing]) -> RevMode {
        let mut bytes = 0u64;
        let mut files = 0usize;
        for file in listings.iter().flat_map(|listing| listing.files.iter()) {
            if !has_linked_ancestor(workspace, file)
                && let Ok(metadata) = std::fs::symlink_metadata(workspace.join(file))
                && metadata.is_file()
            {
                if metadata.len() > FILE_SERVE_MAX_BYTES {
                    return RevMode::Stat;
                }
                bytes += metadata.len();
                files += 1;
            }
        }
        if self.bytes + bytes <= REV_CONTENT_BUDGET_BYTES
            && self.files + files <= REV_CONTENT_BUDGET_FILES
        {
            self.bytes += bytes;
            self.files += files;
            RevMode::Content
        } else {
            RevMode::Stat
        }
    }
}

struct GitTokens {
    head_oid: Option<String>,
    index_token: String,
    revision: Option<String>,
}

fn compute_tokens(
    workspace: &FsPath,
    frame: &RepoFrame,
    files: &mut [GitFileEntry],
    outside: &[GitFileEntry],
) -> Result<GitTokens, ApiError> {
    let head = head_oid(workspace);
    let mut index = IndexSnapshot::read(frame)?;
    let members: Vec<String> = files
        .iter()
        .flat_map(|entry| {
            members_of(&entry.path, entry.old_path.as_deref())
                .into_iter()
                .map(|member| format!("{}{member}", frame.prefix))
        })
        .chain(
            outside
                .iter()
                .flat_map(|entry| members_of(&entry.path, entry.old_path.as_deref())),
        )
        .collect();
    index.load_untracked(&frame.toplevel, &members);
    let mut budget = RevBudget::default();
    // Hidden untracked paths are deliberately not enumerated, so the read
    // cannot authorize a repository-wide add that would include them.
    let mut content_safe = head.is_ok()
        && review_sync_ok(
            workspace,
            &[
                "config",
                "--default",
                "normal",
                "--get",
                "status.showUntrackedFiles",
            ],
        )
        .is_ok_and(|mode| String::from_utf8_lossy(&mode).trim() != "no");
    let head_oid = head.ok().flatten();
    let mut row_token = |entry: &GitFileEntry, prefix: &str| -> Option<String> {
        let result = (|| {
            if entry.status == "unknown" {
                return Err(ApiError::internal("submodule status is unavailable"));
            }
            let listings = members_of(&entry.path, entry.old_path.as_deref())
                .iter()
                .map(|member| list_member(&frame.toplevel, &index, &format!("{prefix}{member}")))
                .collect::<Result<Vec<_>, _>>()?;
            let mode = budget.choose(&frame.toplevel, &listings);
            row_rev(&frame.toplevel, &listings, mode)
        })();
        let rev = result.ok();
        content_safe &= rev.as_deref().is_some_and(|rev| rev.starts_with("c-"));
        rev
    };
    for entry in files.iter_mut() {
        entry.rev = row_token(entry, &frame.prefix);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"cw-git-revision-1\0");
    hasher.update(head_oid.as_deref().unwrap_or("unborn").as_bytes());
    hasher.update(b"\0");
    hasher.update(index.token.as_bytes());
    hasher.update(b"\0");
    let mut rows: Vec<&GitFileEntry> = files.iter().collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    for row in rows {
        hasher.update(row.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(row.rev.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\0");
    }
    let mut outside: Vec<&GitFileEntry> = outside.iter().collect();
    outside.sort_by(|a, b| a.path.cmp(&b.path));
    for row in outside {
        hasher.update(b"outside\0");
        hasher.update(row.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(row_token(row, "").as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\0");
    }
    Ok(GitTokens {
        head_oid,
        index_token: index.token,
        revision: content_safe.then(|| hex(&hasher.finalize())),
    })
}

fn porcelain_status(index: char, worktree: char) -> &'static str {
    match (index, worktree) {
        ('?', '?') => "untracked",
        ('!', '!') => "ignored",
        ('U', _) | (_, 'U') | ('D', 'D') | ('A', 'A') => "conflicted",
        ('R', _) | (_, 'R') => "renamed",
        ('C', _) | (_, 'C') => "copied",
        ('A', _) | (_, 'A') => "added",
        ('D', _) | (_, 'D') => "deleted",
        ('T', _) | (_, 'T') => "typechange",
        ('M', _) | (_, 'M') => "modified",
        _ => "unchanged",
    }
}

// ---------------------------------------------------------------------------
// GET /v1/changes — the file-change inventory only
// ---------------------------------------------------------------------------

/// The Review sheet's change list: the same porcelain projection as
/// `GET /v1/git` minus repo chrome (branches/remotes). One authority — a
/// client that loaded both cannot see them disagree.
pub(super) async fn git_changes(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let workspace = state.workspace.clone();
    let detail = tokio::task::spawn_blocking(move || collect_git_status_detail(&workspace))
        .await
        .map_err(|_| ApiError::internal("git status failed"))??;
    Ok(Json(json!({
        "git_repo": detail.git_repo,
        "branch": detail.branch,
        "staged": detail.staged,
        "unstaged": detail.unstaged,
        "untracked": detail.untracked,
        "head_oid": detail.head_oid,
        "index_token": detail.index_token,
        "revision": detail.revision,
        "files": detail.files,
    })))
}

// ---------------------------------------------------------------------------
// GET /v1/diff + /v1/workspace/diff — unified diffs against HEAD
// ---------------------------------------------------------------------------

/// `git diff <base>` compares the worktree to <base>, covering staged and
/// unstaged changes in one output. On an unborn branch the base is the
/// empty tree, which reads every staged/tracked file as new — the honest
/// "everything changed" picture for a repo with no commits.
async fn diff_base(workspace: &FsPath) -> Result<String, ApiError> {
    let head = git_read(workspace, &["rev-parse", "--verify", "HEAD"]).await?;
    Ok(if head.status_success {
        "HEAD".to_string()
    } else {
        EMPTY_TREE.to_string()
    })
}

async fn selected_diff_base(
    workspace: &FsPath,
    selected: Option<&str>,
) -> Result<String, ApiError> {
    let Some(selected) = selected.filter(|value| !value.is_empty()) else {
        return diff_base(workspace).await;
    };
    if selected.len() > 256 || selected.starts_with('-') || selected.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "Choose a valid base branch or commit",
        ));
    }
    let reference = format!("{selected}^{{commit}}");
    let found = git_read(
        workspace,
        &["rev-parse", "--verify", "--end-of-options", &reference],
    )
    .await?;
    if !found.status_success
        || !matches!(found.stdout.trim().len(), 40 | 64)
        || !found
            .stdout
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request(
            "The base branch or commit is unavailable in this workspace",
        ));
    }
    Ok(found.stdout.trim().to_string())
}

fn diff_context(value: Option<usize>) -> Result<String, ApiError> {
    let value = value.unwrap_or(3);
    if value > 10_000 {
        return Err(ApiError::bad_request(
            "Diff context must be at most 10000 lines",
        ));
    }
    Ok(format!("--unified={value}"))
}

/// Byte-bounded cut at a char boundary; reports whether bytes were dropped.
fn bounded_patch(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitDiffQuery {
    /// Workspace-relative file; may name a deleted file (the diff survives).
    path: String,
    base: Option<String>,
    context: Option<usize>,
    ignore_whitespace: Option<bool>,
}

/// `GET /v1/diff?path=` — one file's unified diff against HEAD (or the empty
/// tree on an unborn branch). An untracked file has no diff by definition:
/// the response says `untracked: true` with an empty `diff` so the client
/// reads the file itself instead of mistaking it for unchanged.
pub(super) async fn git_diff(
    State(state): State<RuntimeApiState>,
    Query(query): Query<GitDiffQuery>,
) -> Result<Json<Value>, ApiError> {
    let path = relative_request_path(&query.path, false)?;
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    let base = selected_diff_base(&workspace, query.base.as_deref()).await?;
    let context = diff_context(query.context)?;
    let path_arg = path.to_string_lossy().into_owned();
    let mut args = file_diff_args(&base, &path_arg);
    args.insert(3, &context);
    if query.ignore_whitespace == Some(true) {
        args.insert(3, "--ignore-all-space");
    }
    let run = git_read(&workspace, &args).await?;
    if !run.status_success {
        return Err(ApiError::internal(format!(
            "git diff failed: {}",
            run.stderr.trim()
        )));
    }
    let (diff, truncated) = bounded_patch(&run.stdout, FILE_DIFF_MAX_BYTES);
    let untracked = if diff.is_empty() {
        let status = git_read(
            &workspace,
            &[
                "--literal-pathspecs",
                "status",
                "--porcelain=v1",
                "-z",
                "--ignore-submodules=dirty",
                "--",
                &path_arg,
            ],
        )
        .await?;
        status
            .stdout
            .split('\0')
            .any(|record| record.starts_with("??"))
    } else {
        false
    };
    Ok(Json(json!({
        "ok": true,
        "path": path_arg,
        "base": base,
        "untracked": untracked,
        "diff": diff,
        "truncated": truncated,
    })))
}

/// One file's diff against `base`. The path is literal, never pathspec magic
/// or a glob, so `:/…`/`:(top)…` cannot reach outside a workspace that is a
/// subdirectory of its repository.
fn file_diff_args<'a>(base: &'a str, path: &'a str) -> Vec<&'a str> {
    let mut args = vec!["--literal-pathspecs", "diff", "--no-color"];
    args.extend(Git::REVIEW_DIFF_ARGS);
    args.extend([base, "--", path]);
    args
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceDiffQuery {
    /// Byte cap on the returned patch (default 256 KiB, max 4 MiB).
    limit: Option<usize>,
    base: Option<String>,
    context: Option<usize>,
    ignore_whitespace: Option<bool>,
}

/// `GET /v1/workspace/diff?limit=` — the whole tree's diff against HEAD plus
/// a complete `--numstat` inventory, so a client renders every changed file
/// row even when the patch body is truncated.
pub(super) async fn workspace_diff(
    State(state): State<RuntimeApiState>,
    Query(query): Query<WorkspaceDiffQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(WORKSPACE_DIFF_DEFAULT_BYTES);
    if !(1024..=WORKSPACE_DIFF_MAX_BYTES).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1024 and {WORKSPACE_DIFF_MAX_BYTES} bytes"
        )));
    }
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    let base = selected_diff_base(&workspace, query.base.as_deref()).await?;
    let context = diff_context(query.context)?;

    let mut numstat_args = vec!["diff", "--numstat"];
    numstat_args.extend(Git::REVIEW_DIFF_ARGS);
    if query.ignore_whitespace == Some(true) {
        numstat_args.push("--ignore-all-space");
    }
    numstat_args.push(&base);
    let numstat = git_read(&workspace, &numstat_args).await?;
    if !numstat.status_success {
        return Err(ApiError::internal(format!(
            "git diff --numstat failed: {}",
            numstat.stderr.trim()
        )));
    }
    let files: Vec<Value> = numstat
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next()?;
            Some(json!({
                "path": path,
                // Binary files report "-" rather than a count.
                "added": added.parse::<u64>().ok(),
                "deleted": deleted.parse::<u64>().ok(),
            }))
        })
        .collect();

    let mut diff_args = vec!["diff", "--no-color"];
    diff_args.extend(Git::REVIEW_DIFF_ARGS);
    diff_args.push(&context);
    if query.ignore_whitespace == Some(true) {
        diff_args.push("--ignore-all-space");
    }
    diff_args.push(&base);
    let run = git_read(&workspace, &diff_args).await?;
    if !run.status_success {
        return Err(ApiError::internal(format!(
            "git diff failed: {}",
            run.stderr.trim()
        )));
    }
    let (diff, truncated) = bounded_patch(&run.stdout, limit);
    Ok(Json(json!({
        "ok": true,
        "git_repo": true,
        "base": base,
        "files": files,
        "diff": diff,
        "truncated": truncated,
    })))
}

// ---------------------------------------------------------------------------
// GET /v1/git/graph — bounded commit graph rows
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitGraphQuery {
    limit: Option<usize>,
}

const GRAPH_FIELD: char = '\u{1f}';
const GRAPH_RECORD: char = '\u{1e}';

pub(super) async fn git_graph(
    State(state): State<RuntimeApiState>,
    Query(query): Query<GitGraphQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(GRAPH_LIMIT_DEFAULT);
    if !(1..=GRAPH_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {GRAPH_LIMIT_MAX}"
        )));
    }
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    let limit_arg = format!("-n{limit}");
    let format = format!(
        "%H{GRAPH_FIELD}%h{GRAPH_FIELD}%P{GRAPH_FIELD}%an{GRAPH_FIELD}%ae{GRAPH_FIELD}%aI{GRAPH_FIELD}%D{GRAPH_FIELD}%s{GRAPH_RECORD}"
    );
    let format_arg = format!("--format={format}");
    let run = git_read(&workspace, &["log", &limit_arg, &format_arg]).await?;
    if !run.status_success {
        // `git log` exits non-zero on an unborn branch; that is a valid empty
        // graph, not a failure. Distinguish with a HEAD probe instead of
        // trusting stderr text.
        let head = git_read(&workspace, &["rev-parse", "--verify", "HEAD"]).await?;
        if head.status_success {
            return Err(ApiError::internal(format!(
                "git log failed: {}",
                run.stderr.trim()
            )));
        }
        return Ok(Json(json!({ "commits": [], "truncated": false })));
    }
    let mut commits = Vec::new();
    for record in run.stdout.split(GRAPH_RECORD) {
        let record = record.trim_matches('\n');
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split(GRAPH_FIELD);
        let (
            Some(id),
            Some(short),
            Some(parents),
            Some(name),
            Some(email),
            Some(timestamp),
            Some(refs),
            Some(subject),
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            continue;
        };
        commits.push(json!({
            "id": id,
            "short": short,
            "parents": parents.split_whitespace().collect::<Vec<_>>(),
            "author": { "name": name, "email": email },
            "timestamp": timestamp,
            "refs": refs
                .split(", ")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>(),
            "subject": subject,
        }));
    }
    let truncated = commits.len() >= limit;
    Ok(Json(json!({ "commits": commits, "truncated": truncated })))
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitPushRequest {
    remote: Option<String>,
    #[serde(default)]
    set_upstream: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitBranchRequest {
    name: String,
    /// Create and switch (`git switch -c`); default is switch to existing.
    #[serde(default)]
    create: bool,
    #[serde(default)]
    expect: Option<GitExpect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitPathsRequest {
    #[serde(default)]
    paths: Vec<String>,
    /// Stage/unstage may take the whole tree; discard cannot.
    #[serde(default)]
    all: bool,
    /// Optional preconditions (#6647); absent keeps the unchecked behaviour.
    #[serde(default)]
    expect: Option<GitExpect>,
    #[serde(default)]
    patch: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitCommitRequest {
    message: String,
    /// Also stage tracked modifications (`git commit -a`).
    #[serde(default)]
    all: bool,
    #[serde(default)]
    expect: Option<GitExpect>,
}

/// What the client last read. Each present field is checked; an absent
/// field is not. `head: null` means "HEAD must be unborn".
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct GitExpect {
    #[serde(default, deserialize_with = "present")]
    head: Option<Option<String>>,
    #[serde(default)]
    index: Option<String>,
    #[serde(default, deserialize_with = "present")]
    revision: Option<Option<String>>,
    #[serde(default)]
    files: Option<BTreeMap<String, String>>,
}

/// Distinguishes an explicit `null` (`Some(None)`) from an absent field.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

/// A validated precondition, normalized (trimmed, lowercase hex, workspace-
/// relative file keys).
#[derive(Debug, Default)]
struct Preconditions {
    head: Option<Option<String>>,
    index: Option<String>,
    revision: Option<String>,
    files: Option<BTreeMap<String, String>>,
}

impl Preconditions {
    fn is_empty(&self) -> bool {
        self.head.is_none()
            && self.index.is_none()
            && self.revision.is_none()
            && self.files.is_none()
    }
}

fn normalized_hex(raw: &str, lengths: &[usize], field: &str) -> Result<String, ApiError> {
    let value = raw.trim().to_ascii_lowercase();
    if lengths.contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(value)
    } else {
        Err(ApiError::bad_request(format!(
            "expect.{field} must be a token read from GET /v1/git"
        )))
    }
}

/// Where the preconditions are being applied, for the rules that differ.
enum ExpectTarget<'a> {
    /// Stage/unstage/discard with explicit, validated paths.
    Paths(&'a [String]),
    /// Stage/unstage of the whole tree.
    AllPaths,
    Commit,
}

/// Validate `expect` before any lock or git call; every failure is a 400 and
/// nothing is written.
fn validate_expect(
    expect: Option<GitExpect>,
    target: ExpectTarget<'_>,
) -> Result<Preconditions, ApiError> {
    let Some(expect) = expect else {
        return Ok(Preconditions::default());
    };
    let head = match expect.head {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => Some(Some(normalized_hex(&raw, &[40, 64], "head")?)),
    };
    let index = expect
        .index
        .map(|raw| normalized_hex(&raw, &[64], "index"))
        .transpose()?;
    let revision = expect
        .revision
        .map(|raw| {
            let raw = raw.ok_or_else(|| ApiError::bad_request(
                "expect.revision cannot be null; a content-safe whole-tree revision is required",
            ))?;
            normalized_hex(&raw, &[64], "revision")
        })
        .transpose()?;
    let files = match (expect.files, target) {
        (None, _) => None,
        (Some(_), ExpectTarget::Commit) => {
            return Err(ApiError::bad_request(
                "expect.files applies to path operations; use expect.index or expect.revision for commit",
            ));
        }
        (Some(_), ExpectTarget::AllPaths) => {
            return Err(ApiError::bad_request(
                "use expect.revision for whole-tree operations",
            ));
        }
        (Some(raw), ExpectTarget::Paths(paths)) => {
            let mut files = BTreeMap::new();
            for (key, token) in raw {
                let path = relative_request_path(&key, false)?
                    .to_string_lossy()
                    .into_owned();
                let token = token.trim().to_ascii_lowercase();
                let valid = token.len() == 66
                    && token.starts_with("c-")
                    && token[2..].bytes().all(|byte| byte.is_ascii_hexdigit());
                if !valid {
                    return Err(ApiError::bad_request(format!(
                        "expect.files[{path}] must be a content-safe c- rev read from GET /v1/git; stat-only revisions cannot guard writes"
                    )));
                }
                if files.insert(path.clone(), token).is_some() {
                    return Err(ApiError::bad_request(format!(
                        "expect.files names {path} twice"
                    )));
                }
            }
            let requested: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
            let guarded: BTreeSet<&str> = files.keys().map(String::as_str).collect();
            if requested != guarded {
                let missing: Vec<&str> = requested.difference(&guarded).copied().collect();
                let extra: Vec<&str> = guarded.difference(&requested).copied().collect();
                return Err(ApiError::bad_request(format!(
                    "expect.files must name exactly the requested paths (missing: [{}]; extra: [{}])",
                    missing.join(", "),
                    extra.join(", ")
                )));
            }
            Some(files)
        }
    };
    Ok(Preconditions {
        head,
        index,
        revision,
        files,
    })
}

/// A git-route failure. Ordinary failures keep the shared [`ApiError`]
/// envelope; the precondition and busy answers add a machine `code` and
/// fields a client acts on.
pub(super) enum GitWriteError {
    Api(ApiError),
    Coded {
        status: StatusCode,
        code: &'static str,
        message: String,
        extra: serde_json::Map<String, Value>,
    },
}

impl From<ApiError> for GitWriteError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl IntoResponse for GitWriteError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::Coded {
                status,
                code,
                message,
                mut extra,
            } => {
                extra.insert(
                    "error".to_string(),
                    json!({ "message": message, "status": status.as_u16(), "code": code }),
                );
                (status, Json(Value::Object(extra))).into_response()
            }
        }
    }
}

fn busy_error() -> GitWriteError {
    GitWriteError::Coded {
        status: StatusCode::CONFLICT,
        code: "git_busy",
        message:
            "Another Git write from this runtime is still running. Try again when it finishes."
                .to_string(),
        extra: serde_json::Map::new(),
    }
}

/// Compare the preconditions with the repository as it is now. Runs on a
/// blocking thread under the write lock.
fn check_preconditions(
    workspace: &FsPath,
    expect: &Preconditions,
) -> Result<Result<(), GitWriteError>, ApiError> {
    let mut stale: Vec<&'static str> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut stale_paths: Vec<String> = Vec::new();
    let frame = RepoFrame::read(workspace)?;

    if let Some(expected) = &expect.head {
        let current = head_oid(workspace)?;
        if &current != expected {
            stale.push("head");
            parts.push(match (expected, &current) {
                (None, Some(_)) => "HEAD is no longer unborn".to_string(),
                (Some(_), None) => "HEAD is now unborn".to_string(),
                _ => "HEAD moved".to_string(),
            });
        }
    }
    let index = if expect.index.is_some() || expect.files.is_some() {
        Some(IndexSnapshot::read(&frame)?)
    } else {
        None
    };
    if let (Some(expected), Some(index)) = (&expect.index, &index)
        && &index.token != expected
    {
        stale.push("index");
        parts.push("the index changed".to_string());
    }
    if let (Some(files), Some(mut index)) = (&expect.files, index) {
        // A rename row's token also covers its source path; recover that
        // pairing from the current status, as the read did.
        let (rows, _) = status_rows(workspace, &frame)?;
        let (rows, _) = split_by_workspace(rows, &frame.prefix);
        let sources: BTreeMap<String, String> = rows
            .iter()
            .filter_map(|row| {
                row.old_path
                    .as_deref()
                    .map(|old| (normalized_row_path(&row.path).to_string(), old.to_string()))
            })
            .collect();
        let members: Vec<String> = files
            .keys()
            .flat_map(|path| members_of(path, sources.get(path).map(String::as_str)))
            .map(|member| format!("{}{member}", frame.prefix))
            .collect();
        index.load_untracked(&frame.toplevel, &members);
        for (path, expected) in files {
            let current = members_of(path, sources.get(path).map(String::as_str))
                .iter()
                .map(|member| {
                    list_member(
                        &frame.toplevel,
                        &index,
                        &format!("{}{member}", frame.prefix),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .and_then(|listings| row_rev(&frame.toplevel, &listings, RevMode::Content));
            if current.as_ref().ok() != Some(expected)
                || rows
                    .iter()
                    .any(|row| row.status == "unknown" && is_at_or_under(&row.path, path))
            {
                stale_paths.push(path.clone());
            }
        }
        if !stale_paths.is_empty() {
            stale.push("files");
            parts.push(match stale_paths.as_slice() {
                [one] => format!("{one} changed"),
                many => {
                    let shown: Vec<&str> = many.iter().take(3).map(String::as_str).collect();
                    let more = many.len().saturating_sub(shown.len());
                    if more == 0 {
                        format!("{} files changed: {}", many.len(), shown.join(", "))
                    } else {
                        format!(
                            "{} files changed: {} and {more} more",
                            many.len(),
                            shown.join(", ")
                        )
                    }
                }
            });
        }
    }
    let mut current = None;
    if let Some(expected) = &expect.revision {
        let detail = collect_git_status_detail(workspace)?;
        if detail.revision.as_deref() != Some(expected.as_str()) {
            stale.push("revision");
            parts.push("the working tree or index changed".to_string());
        }
        current = Some(detail);
    }
    if stale.is_empty() {
        return Ok(Ok(()));
    }
    let current = match current {
        Some(detail) => detail,
        None => collect_git_status_detail(workspace)?,
    };
    let mut extra = serde_json::Map::new();
    extra.insert("stale".to_string(), json!(stale));
    extra.insert("stale_paths".to_string(), json!(stale_paths));
    extra.insert(
        "current".to_string(),
        serde_json::to_value(current)
            .map_err(|_| ApiError::internal("git status serialization failed"))?,
    );
    Ok(Err(GitWriteError::Coded {
        status: StatusCode::CONFLICT,
        code: "git_state_changed",
        message: format!(
            "The repository changed since it was read ({}). Nothing was written; refresh and review again.",
            parts.join("; ")
        ),
        extra,
    }))
}

async fn mutation_response(workspace: &FsPath, run: GitRun) -> Result<Json<Value>, ApiError> {
    if !run.status_success {
        let tail = output_tail(&run);
        return Err(ApiError::bad_request(if tail.is_empty() {
            format!("git exited {}", run.exit_code.unwrap_or(-1))
        } else {
            tail
        }));
    }
    let output = output_tail(&run);
    let workspace = workspace.to_path_buf();
    let (status, current) = tokio::task::spawn_blocking(move || {
        (
            collect_workspace_status(&workspace),
            collect_git_status_detail(&workspace),
        )
    })
    .await
    .map_err(|_| ApiError::internal("git status failed"))?;
    Ok(Json(json!({
        "ok": true,
        "output": output,
        "status": status,
        "current": current?,
    })))
}

/// The shared check-then-write path. The per-runtime lock makes the check
/// and the write atomic with respect to this server's other git writes (two
/// app windows on one runtime); a second writer gets `git_busy` instead of
/// waiting behind a long hook. External processes are outside the lock —
/// see docs/RUNTIME_API.md for that window.
async fn guarded_write(
    state: &RuntimeApiState,
    expect: Preconditions,
    args: Vec<String>,
) -> Result<Json<Value>, GitWriteError> {
    let _guard = state.git_writes.try_lock().map_err(|_| busy_error())?;
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    if !expect.is_empty() {
        let root = workspace.clone();
        tokio::task::spawn_blocking(move || check_preconditions(&root, &expect))
            .await
            .map_err(|_| ApiError::internal("git precondition check failed"))???;
    }
    Ok(mutation_response(&workspace, git_write(&workspace, args).await?).await?)
}

/// Validate and collect pathspecs. Every path is workspace-relative with no
/// `.`/`..`/`.git` components and is passed after `--` with
/// `--literal-pathspecs`, so it can never be read as an option or a glob.
fn validated_paths(request: &GitPathsRequest, allow_all: bool) -> Result<Vec<String>, ApiError> {
    if request.all && !allow_all {
        return Err(ApiError::bad_request(
            "discard requires explicit paths; refusing to discard the whole tree",
        ));
    }
    if request.all && !request.paths.is_empty() {
        return Err(ApiError::bad_request(
            "all and paths are mutually exclusive",
        ));
    }
    if !request.all && request.paths.is_empty() {
        return Err(ApiError::bad_request("paths is required (or all: true)"));
    }
    if request.paths.len() > MAX_PATH_ARGS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_PATH_ARGS} paths per request"
        )));
    }
    request
        .paths
        .iter()
        .map(|raw| {
            relative_request_path(raw, false).map(|path| path.to_string_lossy().into_owned())
        })
        .collect()
}

/// Validated paths plus their preconditions.
fn paths_and_expect(
    request: GitPathsRequest,
    allow_all: bool,
) -> Result<(bool, Vec<String>, Preconditions), ApiError> {
    let paths = validated_paths(&request, allow_all)?;
    let target = if request.all {
        ExpectTarget::AllPaths
    } else {
        ExpectTarget::Paths(&paths)
    };
    let expect = validate_expect(request.expect, target)?;
    Ok((request.all, paths, expect))
}

/// `git --literal-pathspecs <subcommand…> -- <paths>`.
fn literal_path_args(subcommand: &[&str], paths: Vec<String>) -> Vec<String> {
    let mut args = vec!["--literal-pathspecs".to_string()];
    args.extend(subcommand.iter().map(|arg| arg.to_string()));
    args.push("--".to_string());
    args.extend(paths);
    args
}

fn validated_patch(patch: &str, path: &str) -> Result<(), ApiError> {
    let refuse = |why: &str| Err(ApiError::bad_request(format!("patch {why}")));
    if patch.len() > MAX_PATCH_BYTES {
        return refuse(&format!("must be at most {MAX_PATCH_BYTES} bytes"));
    }
    let header = format!("diff --git a/{path} b/{path}");
    let old = format!("--- a/{path}");
    let new = format!("+++ b/{path}");
    let (mut seen_old, mut seen_new, mut hunks) = (false, false, 0usize);
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for line in patch.lines() {
        if old_left > 0 || new_left > 0 {
            match line.as_bytes().first() {
                Some(b' ') | None => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'-') => old_left = old_left.saturating_sub(1),
                Some(b'+') => new_left = new_left.saturating_sub(1),
                Some(b'\\') => {}
                _ => return refuse("has a hunk shorter than its header"),
            }
            continue;
        }
        if line.starts_with('\\') {
            continue;
        }
        if let Some(range) = line.strip_prefix("@@ ") {
            if !(seen_old && seen_new) {
                return refuse(&format!("must start with --- a/{path} and +++ b/{path}"));
            }
            let mut counts = range.split_whitespace().take(2).map(|side| {
                let side = &side[1..];
                match side.split_once(',') {
                    Some((_, count)) => count.parse::<usize>().ok(),
                    None => side.parse::<usize>().ok().map(|_| 1),
                }
            });
            let (Some(Some(a)), Some(Some(b))) = (counts.next(), counts.next()) else {
                return refuse("has a malformed @@ line");
            };
            (old_left, new_left) = (a, b);
            hunks += 1;
        } else if line.starts_with("diff --git ") {
            if line != header {
                return refuse(&format!("may change only {path}"));
            }
            (seen_old, seen_new) = (false, false);
        } else if line.starts_with("--- ") {
            if line != old {
                return refuse(&format!("may change only {path}, and not create it"));
            }
            seen_old = true;
        } else if line.starts_with("+++ ") {
            if line != new {
                return refuse(&format!("may change only {path}, and not delete it"));
            }
            seen_new = true;
        } else if !line.starts_with("index ") {
            return refuse("may only change lines (no rename, copy, mode or binary changes)");
        }
    }
    if hunks == 0 || old_left > 0 || new_left > 0 {
        return refuse("needs at least one complete hunk");
    }
    Ok(())
}

async fn patch_write(
    state: &RuntimeApiState,
    request: GitPathsRequest,
    apply: &[&str],
) -> Result<Json<Value>, GitWriteError> {
    let patch = request.patch.clone().unwrap_or_default();
    if request.all || request.paths.len() != 1 {
        return Err(ApiError::bad_request("patch needs exactly one path").into());
    }
    let (_, paths, expect) = paths_and_expect(request, false)?;
    validated_patch(&patch, &paths[0])?;
    let file = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(patch.as_bytes())?;
        file.flush()?;
        Ok::<_, std::io::Error>(file)
    })
    .await
    .map_err(|_| ApiError::internal("patch staging failed"))?
    .map_err(|error| ApiError::internal(format!("patch staging failed: {error}")))?;
    let workspace = canonical_workspace(&state.workspace)?;
    let place = git_read_ok(&workspace, &["rev-parse", "--show-cdup", "--show-prefix"]).await?;
    let mut place = place.lines();
    let (cdup, prefix) = (
        place.next().unwrap_or_default().to_string(),
        place.next().unwrap_or_default().to_string(),
    );
    let mut args = Vec::new();
    if !cdup.is_empty() {
        args.extend(["-C".to_string(), cdup]);
    }
    args.extend(apply.iter().map(|arg| arg.to_string()));
    if !prefix.is_empty() {
        args.push(format!("--directory={prefix}"));
    }
    args.push("--".to_string());
    args.push(file.path().to_string_lossy().into_owned());
    let result = guarded_write(state, expect, args).await;
    drop(file);
    result
}

pub(super) async fn git_stage(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitPathsRequest>,
) -> Result<Json<Value>, GitWriteError> {
    if request.patch.is_some() {
        return patch_write(&state, request, &["apply", "--cached"]).await;
    }
    let (all, paths, expect) = paths_and_expect(request, true)?;
    let args = if all {
        vec!["add".to_string(), "--all".to_string()]
    } else {
        literal_path_args(&["add"], paths)
    };
    guarded_write(&state, expect, args).await
}

pub(super) async fn git_unstage(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitPathsRequest>,
) -> Result<Json<Value>, GitWriteError> {
    if request.patch.is_some() {
        return patch_write(&state, request, &["apply", "--cached", "--reverse"]).await;
    }
    let (all, paths, expect) = paths_and_expect(request, true)?;
    let args = if all {
        // `:/` is pathspec magic for the repository root, so this one form
        // cannot run under --literal-pathspecs.
        vec![
            "restore".to_string(),
            "--staged".to_string(),
            ":/".to_string(),
        ]
    } else {
        literal_path_args(&["restore", "--staged"], paths)
    };
    guarded_write(&state, expect, args).await
}

pub(super) async fn git_discard(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitPathsRequest>,
) -> Result<Json<Value>, GitWriteError> {
    if request.patch.is_some() {
        return patch_write(&state, request, &["apply", "--reverse"]).await;
    }
    let (_, paths, expect) = paths_and_expect(request, false)?;
    guarded_write(&state, expect, literal_path_args(&["checkout"], paths)).await
}

pub(super) async fn git_commit(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitCommitRequest>,
) -> Result<Json<Value>, GitWriteError> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad_request("message is required").into());
    }
    if message.len() > MAX_COMMIT_MESSAGE_BYTES {
        return Err(ApiError::bad_request(format!(
            "message must be at most {MAX_COMMIT_MESSAGE_BYTES} bytes"
        ))
        .into());
    }
    let expect = validate_expect(request.expect, ExpectTarget::Commit)?;
    let mut args = vec!["commit".to_string()];
    if request.all {
        args.push("--all".to_string());
    }
    args.push("--message".to_string());
    args.push(message.to_string());
    guarded_write(&state, expect, args).await
}

/// A push target must be one of the repository's configured remotes: never an
/// option lookalike (`--force`, `--mirror`) and never a path or URL.
fn validate_push_remote(remote: &str, configured: &str) -> Result<(), ApiError> {
    let shaped = !remote.starts_with('-')
        && remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'));
    if !shaped {
        return Err(ApiError::bad_request("remote must be a remote name"));
    }
    if !configured.lines().any(|name| name.trim() == remote) {
        return Err(ApiError::bad_request(format!(
            "remote {remote} is not configured in this repository"
        )));
    }
    Ok(())
}

/// A bounded async read that must succeed; its stdout. Discovery for request
/// validation goes through [`git_read`] so it never blocks a runtime worker.
async fn git_read_ok(workspace: &FsPath, args: &[&str]) -> Result<String, ApiError> {
    let run = git_read(workspace, args).await?;
    if !run.status_success {
        return Err(ApiError::internal(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            run.stderr.trim()
        )));
    }
    Ok(run.stdout)
}

/// `git push` arguments for a request. The remote that will be pushed to —
/// the requested one, or `origin` when only `set_upstream` is asked — must be
/// a configured remote, and `--` keeps it from ever parsing as an option.
async fn push_args(
    workspace: &FsPath,
    remote: Option<&str>,
    set_upstream: bool,
) -> Result<Vec<String>, ApiError> {
    let remote = remote.map(str::trim).filter(|remote| !remote.is_empty());
    let remote = match (remote, set_upstream) {
        (Some(remote), _) => Some(remote),
        (None, true) => Some("origin"),
        (None, false) => None,
    };
    let mut args = vec!["push".to_string()];
    let Some(remote) = remote else {
        return Ok(args);
    };
    validate_push_remote(remote, &git_read_ok(workspace, &["remote"]).await?)?;
    if set_upstream {
        let branch = git_read_ok(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        let branch = branch.trim();
        if branch.is_empty() || branch == "HEAD" {
            return Err(ApiError::bad_request(
                "cannot set upstream from a detached HEAD",
            ));
        }
        args.push("--set-upstream".to_string());
        args.extend(["--".to_string(), remote.to_string(), branch.to_string()]);
    } else {
        args.extend(["--".to_string(), remote.to_string()]);
    }
    Ok(args)
}

pub(super) async fn git_push(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitPushRequest>,
) -> Result<Json<Value>, ApiError> {
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    let args = push_args(&workspace, request.remote.as_deref(), request.set_upstream).await?;
    // Push stays outside the write lock: it crosses the network under a
    // 120 s bound and only moves a remote ref, never the index or worktree.
    mutation_response(&workspace, git_write(&workspace, args).await?).await
}

pub(super) async fn git_branch(
    State(state): State<RuntimeApiState>,
    Json(request): Json<GitBranchRequest>,
) -> Result<Json<Value>, GitWriteError> {
    let name = request.name.trim();
    if name.is_empty() || name.len() > MAX_BRANCH_NAME_BYTES {
        return Err(ApiError::bad_request("name is required").into());
    }
    let expect = validate_expect(request.expect, ExpectTarget::Commit)?;
    let _admission = state.runtime_threads.session_checkpoint_guard().await;
    // Switching rewrites the worktree and index, so it shares the lock with
    // stage/discard/commit.
    let _guard = state.git_writes.try_lock().map_err(|_| busy_error())?;
    let workspace = canonical_workspace(&state.workspace)?;
    require_repo(&workspace)?;
    state
        .runtime_threads
        .reject_active_turns_in_workspace(&workspace)
        .await
        .map_err(|error| ApiError::conflict(error.to_string()))?;
    if !expect.is_empty() {
        let root = workspace.clone();
        tokio::task::spawn_blocking(move || check_preconditions(&root, &expect))
            .await
            .map_err(|_| ApiError::internal("git precondition check failed"))???;
    }
    // `check-ref-format --branch` is the ref authority — it rejects option
    // lookalikes, `..`, `@{`, control bytes, and every other unsafe name.
    let check = git_write(
        &workspace,
        vec![
            "check-ref-format".to_string(),
            "--branch".to_string(),
            name.to_string(),
        ],
    )
    .await?;
    if !check.status_success {
        return Err(ApiError::bad_request("name is not a valid branch").into());
    }
    let mut args = vec!["switch".to_string()];
    if request.create {
        args.push("--create".to_string());
    }
    args.push(name.to_string());
    Ok(mutation_response(&workspace, git_write(&workspace, args).await?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    thread_local! {
        pub(super) static GIT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn git(dir: &FsPath, args: &[&str]) {
        let output = Git::output(args, dir).expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "git-rev@example.test"]);
        git(dir, &["config", "user.name", "Git Rev Test"]);
        git(dir, &["config", "core.autocrlf", "false"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "initial"]);
        tmp
    }

    fn rev(workspace: &FsPath, path: &str) -> String {
        let frame = RepoFrame::read(workspace).unwrap();
        let mut index = IndexSnapshot::read(&frame).unwrap();
        index.load_untracked(
            &frame.toplevel,
            &[format!("{}{}", frame.prefix, normalized_row_path(path))],
        );
        let listings: Vec<_> = members_of(path, None)
            .iter()
            .map(|member| {
                list_member(
                    &frame.toplevel,
                    &index,
                    &format!("{}{member}", frame.prefix),
                )
                .unwrap()
            })
            .collect();
        row_rev(&frame.toplevel, &listings, RevMode::Content).unwrap()
    }

    fn index_token(workspace: &FsPath) -> String {
        IndexSnapshot::read(&RepoFrame::read(workspace).unwrap())
            .unwrap()
            .token
    }

    #[test]
    fn row_rev_tracks_content_index_and_deletion_but_not_touch() {
        let tmp = repo();
        let ws = tmp.path();
        fs::write(ws.join("a.txt"), "two\n").unwrap();
        let modified = rev(ws, "a.txt");
        assert!(
            modified.starts_with("c-") && modified.len() == 66,
            "{modified}"
        );
        assert_eq!(rev(ws, "a.txt"), modified, "deterministic");

        // Touching without changing bytes keeps the content token.
        let file = fs::File::options()
            .write(true)
            .open(ws.join("a.txt"))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + Duration::from_secs(90))
            .unwrap();
        drop(file);
        assert_eq!(rev(ws, "a.txt"), modified, "touch is not a change");

        fs::write(ws.join("a.txt"), "three\n").unwrap();
        let rewritten = rev(ws, "a.txt");
        assert_ne!(rewritten, modified, "content change");

        git(ws, &["add", "a.txt"]);
        let staged = rev(ws, "a.txt");
        assert_ne!(staged, rewritten, "staging changes the index part");

        fs::remove_file(ws.join("a.txt")).unwrap();
        assert_ne!(rev(ws, "a.txt"), staged, "deletion reads as absent");
    }

    #[test]
    fn directory_rev_covers_untracked_files_inside() {
        let tmp = repo();
        let ws = tmp.path();
        fs::create_dir_all(ws.join("dir/deep")).unwrap();
        fs::write(ws.join("dir/deep/x.rs"), "x\n").unwrap();
        let before = rev(ws, "dir/");
        assert_eq!(rev(ws, "dir"), before, "dir and dir/ are one key");
        fs::write(ws.join("dir/deep/x.rs"), "y\n").unwrap();
        assert_ne!(rev(ws, "dir"), before);
        fs::write(ws.join("dir/deep/x.rs"), "x\n").unwrap();
        fs::write(ws.join("dir/new.rs"), "n\n").unwrap();
        assert_ne!(rev(ws, "dir"), before, "a new file under the row");
    }

    #[test]
    fn index_token_ignores_stat_refresh_and_tracks_real_changes() {
        let tmp = repo();
        let ws = tmp.path();
        let before = index_token(ws);
        let file = fs::File::options()
            .write(true)
            .open(ws.join("a.txt"))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + Duration::from_secs(90))
            .unwrap();
        drop(file);
        git(ws, &["status", "--porcelain"]);
        git(ws, &["update-index", "--refresh"]);
        assert_eq!(index_token(ws), before, "stat refresh is not a change");
        fs::write(ws.join("b.txt"), "b\n").unwrap();
        git(ws, &["add", "b.txt"]);
        assert_ne!(index_token(ws), before);
    }

    #[test]
    fn rename_row_rev_covers_the_source_path() {
        let tmp = repo();
        let ws = tmp.path();
        git(ws, &["mv", "a.txt", "b.txt"]);
        let detail = collect_git_status_detail(ws).unwrap();
        let row = detail.files.iter().find(|row| row.path == "b.txt").unwrap();
        assert_eq!(row.old_path.as_deref(), Some("a.txt"));
        let before = row.rev.clone().unwrap();
        // Recreating the source in the worktree changes what a stage of the
        // rename would record.
        fs::write(ws.join("a.txt"), "resurrected\n").unwrap();
        let detail = collect_git_status_detail(ws).unwrap();
        let after = detail
            .files
            .iter()
            .find(|row| row.path == "b.txt")
            .and_then(|row| row.rev.clone())
            .unwrap();
        assert_ne!(after, before);
    }

    #[test]
    fn subdirectory_workspace_reports_workspace_relative_rows() {
        let tmp = repo();
        let root = tmp.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/in.txt"), "in\n").unwrap();
        git(root, &["add", "sub/in.txt"]);
        git(root, &["commit", "-q", "-m", "sub"]);
        fs::write(root.join("sub/in.txt"), "changed\n").unwrap();
        fs::write(root.join("a.txt"), "outside change\n").unwrap();
        let ws = root.join("sub").canonicalize().unwrap();
        let detail = collect_git_status_detail(&ws).unwrap();
        let paths: Vec<&str> = detail.files.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["in.txt"],
            "workspace frame, outside rows dropped"
        );
        assert_eq!(
            detail.files[0].rev.as_deref(),
            Some(rev(&ws, "in.txt").as_str())
        );
        // The whole-tree revision still sees the change outside the
        // workspace, because stage-all and commit reach it.
        let before = detail.revision.clone().unwrap();
        fs::write(root.join("a.txt"), "outside change again\n").unwrap();
        git(root, &["add", "a.txt"]);
        let after = collect_git_status_detail(&ws).unwrap().revision.unwrap();
        assert_ne!(before, after);
    }

    fn expect_file(path: &str, token: String) -> Preconditions {
        validate_expect(
            Some(GitExpect {
                files: Some(BTreeMap::from([(path.to_string(), token)])),
                ..Default::default()
            }),
            ExpectTarget::Paths(&[path.to_string()]),
        )
        .unwrap()
    }

    fn assert_conflict(workspace: &FsPath, expect: &Preconditions) {
        assert!(matches!(
            check_preconditions(workspace, expect).unwrap(),
            Err(GitWriteError::Coded {
                status: StatusCode::CONFLICT,
                code: "git_state_changed",
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn review_executable_mode_invalidates_row_guard() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = repo();
        let ws = tmp.path();
        git(ws, &["config", "core.filemode", "true"]);
        fs::write(ws.join("a.txt"), "modified\n").unwrap();
        fs::set_permissions(ws.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        let detail = collect_git_status_detail(ws).unwrap();
        let expect = expect_file("a.txt", detail.files[0].rev.clone().unwrap());
        assert!(check_preconditions(ws, &expect).unwrap().is_ok());
        fs::set_permissions(ws.join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_conflict(ws, &expect);
        assert_ne!(
            collect_git_status_detail(ws).unwrap().revision,
            detail.revision
        );
    }

    #[test]
    fn review_outside_content_invalidates_whole_tree_guard() {
        let tmp = repo();
        let root = tmp.path();
        fs::create_dir(root.join("sub")).unwrap();
        let ws = root.join("sub");
        fs::write(root.join("a.txt"), "outside one\n").unwrap();
        let detail = collect_git_status_detail(&ws).unwrap();
        assert!(detail.files.is_empty());
        let expect = Preconditions {
            head: Some(detail.head_oid),
            revision: Some(detail.revision.unwrap()),
            ..Default::default()
        };
        assert!(check_preconditions(&ws, &expect).unwrap().is_ok());
        // Keep the porcelain XY and index unchanged.
        fs::write(root.join("a.txt"), "outside two\n").unwrap();
        assert_conflict(&ws, &expect);
    }

    #[test]
    fn review_submodule_head_invalidates_row_guard() {
        let tmp = repo();
        let ws = tmp.path();
        let sub = ws.join("vendor");
        fs::create_dir(&sub).unwrap();
        git(&sub, &["init", "-q", "-b", "main"]);
        git(&sub, &["config", "user.email", "git-rev@example.test"]);
        git(&sub, &["config", "user.name", "Git Rev Test"]);
        fs::write(sub.join("lib.txt"), "one\n").unwrap();
        git(&sub, &["add", "lib.txt"]);
        git(&sub, &["commit", "-q", "-m", "one"]);
        git(ws, &["add", "vendor"]);
        git(ws, &["commit", "-q", "-m", "gitlink"]);
        for contents in ["two\n", "three\n"] {
            fs::write(sub.join("lib.txt"), contents).unwrap();
            git(&sub, &["commit", "-q", "-am", contents]);
        }
        git(&sub, &["checkout", "-q", "HEAD~1"]);
        let detail = collect_git_status_detail(ws).unwrap();
        let row = detail
            .files
            .iter()
            .find(|row| row.path == "vendor")
            .unwrap();
        let expect = expect_file("vendor", row.rev.clone().unwrap());
        assert!(check_preconditions(ws, &expect).unwrap().is_ok());
        git(&sub, &["checkout", "-q", "main"]);
        assert_conflict(ws, &expect);
    }

    #[test]
    fn review_untracked_workspace_prefix_exposes_children() {
        let tmp = repo();
        let root = tmp.path();
        fs::create_dir_all(root.join("sub/deep")).unwrap();
        fs::write(root.join("sub/new.txt"), "new\n").unwrap();
        fs::write(root.join("sub/deep/child.txt"), "child\n").unwrap();
        let ws = root.join("sub");
        let detail = collect_git_status_detail(&ws).unwrap();
        assert_eq!(
            detail
                .files
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["deep/child.txt", "new.txt"]
        );
        for row in detail.files {
            assert_eq!(row.status, "untracked");
            let expect = expect_file(&row.path, row.rev.unwrap());
            assert!(check_preconditions(&ws, &expect).unwrap().is_ok());
            git(&ws, &["add", "--", &row.path]);
        }
        assert_eq!(collect_git_status_detail(&ws).unwrap().staged, 2);
    }

    #[test]
    fn review_outgoing_rename_exposes_source_deletion() {
        let tmp = repo();
        let root = tmp.path();
        fs::create_dir(root.join("sub")).unwrap();
        git(root, &["mv", "a.txt", "sub/a.txt"]);
        git(root, &["commit", "-q", "-m", "inside"]);
        git(root, &["mv", "sub/a.txt", "outside.txt"]);
        let ws = root.join("sub");
        let detail = collect_git_status_detail(&ws).unwrap();
        assert_eq!(detail.files.len(), 1);
        let row = &detail.files[0];
        assert_eq!(row.path, "a.txt");
        assert_eq!(row.index, "D");
        assert_eq!(row.status, "deleted");
        assert!(row.staged);
        assert!(row.old_path.is_none());
        let expect = expect_file("a.txt", row.rev.clone().unwrap());
        assert!(check_preconditions(&ws, &expect).unwrap().is_ok());
        // The projected row can unstage the in-workspace deletion.
        git(&ws, &["restore", "--staged", "--", "a.txt"]);
        assert_ne!(index_token(&ws), detail.index_token.unwrap());
    }

    #[test]
    fn review_untracked_inventory_has_constant_git_reads() {
        let tmp = repo();
        let ws = tmp.path();
        fs::write(ws.join(".gitignore"), "*.ignored\n").unwrap();
        let mut read_counts = Vec::new();
        for count in [1, 32] {
            for n in 0..count {
                let dir = ws.join(format!("dir{n}"));
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("file.txt"), "file\n").unwrap();
                fs::write(dir.join("skip.ignored"), "ignored\n").unwrap();
            }
            GIT_READS.with(|reads| reads.set(0));
            let detail = collect_git_status_detail(ws).unwrap();
            read_counts.push(GIT_READS.with(|reads| reads.get()));
            assert!(detail.files.iter().all(|row| row.rev.is_some()));
            assert!(
                !detail
                    .files
                    .iter()
                    .any(|row| row.path.ends_with(".ignored"))
            );
        }
        assert_eq!(
            read_counts[0], read_counts[1],
            "Git reads must not grow per directory"
        );
    }

    #[test]
    fn review_broken_head_is_not_unborn() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        git(ws, &["init", "-q", "-b", "main"]);
        assert_eq!(head_oid(ws).unwrap(), None);
        assert!(collect_git_status_detail(ws).unwrap().index_token.is_some());
        let expect = Preconditions {
            head: Some(None),
            ..Default::default()
        };
        assert!(check_preconditions(ws, &expect).unwrap().is_ok());
        // An existing branch pointing at a missing object is broken, not unborn.
        fs::write(
            ws.join(".git/refs/heads/main"),
            format!("{}\n", "1".repeat(40)),
        )
        .unwrap();
        assert!(head_oid(ws).is_err());
        assert!(collect_git_status_detail(ws).unwrap().revision.is_none());
        assert!(check_preconditions(ws, &expect).is_err());
        fs::write(ws.join(".git/refs/heads/main"), "broken\n").unwrap();
        assert!(
            head_oid(ws).is_err(),
            "a malformed ref is not an absent ref"
        );
        assert!(collect_git_status_detail(ws).unwrap().revision.is_none());
        // The same failure in detached HEAD must also propagate.
        fs::write(ws.join(".git/HEAD"), format!("{}\n", "1".repeat(40))).unwrap();
        assert!(head_oid(ws).is_err());
    }

    #[test]
    fn review_stat_only_revisions_cannot_guard_writes() {
        use std::io::{Seek as _, Write as _};
        let tmp = repo();
        let ws = tmp.path();
        let path = ws.join("a.txt");
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_len(FILE_SERVE_MAX_BYTES + 1).unwrap();
        drop(file);
        let detail = collect_git_status_detail(ws).unwrap();
        let token = detail.files[0].rev.clone().unwrap();
        assert!(
            token.starts_with("s-"),
            "oversized row must not claim content safety"
        );
        assert!(
            detail.revision.is_none(),
            "whole-tree token cannot hide stat-only rows"
        );
        // Same size and mtime, different bytes: the display token can match.
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let mut file = fs::File::options().write(true).open(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(b"two\n").unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        let after = collect_git_status_detail(ws).unwrap();
        assert_eq!(after.files[0].rev.as_ref(), Some(&token));
        assert!(
            validate_expect(
                Some(GitExpect {
                    files: Some(BTreeMap::from([("a.txt".to_string(), token)])),
                    ..Default::default()
                }),
                ExpectTarget::Paths(&["a.txt".to_string()]),
            )
            .is_err()
        );
        // An old c- token cannot pass after a file grows beyond the bound.
        fs::write(&path, "small\n").unwrap();
        let expect = expect_file("a.txt", rev(ws, "a.txt"));
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(FILE_SERVE_MAX_BYTES + 1)
            .unwrap();
        assert_conflict(ws, &expect);
    }

    #[test]
    fn review_hash_budget_withholds_whole_tree_revision() {
        let tmp = repo();
        let ws = tmp.path();
        for n in 0..=REV_CONTENT_BUDGET_FILES {
            fs::write(ws.join(format!("file{n:05}")), "x").unwrap();
        }
        let detail = collect_git_status_detail(ws).unwrap();
        assert!(detail.revision.is_none());
        assert!(detail.index_token.is_some());
        assert_eq!(
            detail
                .files
                .iter()
                .filter(|row| row.rev.as_deref().unwrap().starts_with("c-"))
                .count(),
            REV_CONTENT_BUDGET_FILES
        );
        let row = detail.files.last().unwrap();
        assert!(row.rev.as_deref().unwrap().starts_with("s-"));
        assert!(
            validate_expect(
                Some(GitExpect {
                    files: Some(BTreeMap::from([(
                        row.path.clone(),
                        row.rev.clone().unwrap()
                    )])),
                    ..Default::default()
                }),
                ExpectTarget::Paths(std::slice::from_ref(&row.path)),
            )
            .is_err()
        );
    }

    fn assert_unguardable_row(workspace: &FsPath, bad_path: &str) {
        let detail = collect_git_status_detail(workspace).unwrap();
        assert_eq!(detail.branch.as_deref(), Some("main"));
        assert!(detail.head_oid.is_some());
        assert!(detail.index_token.is_some());
        assert!(detail.revision.is_none());
        let bad = detail
            .files
            .iter()
            .find(|row| normalized_row_path(&row.path) == bad_path)
            .unwrap();
        assert!(bad.rev.is_none());
        assert_eq!(
            serde_json::to_value(bad).unwrap().get("rev"),
            Some(&Value::Null)
        );
        let healthy = detail
            .files
            .iter()
            .find(|row| row.path == "healthy.txt")
            .unwrap();
        let expect = expect_file("healthy.txt", healthy.rev.clone().unwrap());
        assert!(check_preconditions(workspace, &expect).unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_only_withholds_affected_row() {
        let tmp = repo();
        let ws = tmp.path();
        fs::create_dir(ws.join("vendor")).unwrap();
        fs::write(ws.join("vendor/a.txt"), "tracked\n").unwrap();
        git(ws, &["add", "vendor"]);
        git(ws, &["commit", "-q", "-m", "vendor"]);
        let expect = expect_file("vendor/a.txt", rev(ws, "vendor/a.txt"));
        fs::remove_dir_all(ws.join("vendor")).unwrap();
        let shared = tempfile::tempdir().unwrap();
        fs::write(shared.path().join("a.txt"), "outside\n").unwrap();
        std::os::unix::fs::symlink(shared.path(), ws.join("vendor")).unwrap();
        fs::write(ws.join("healthy.txt"), "healthy\n").unwrap();
        assert_unguardable_row(ws, "vendor/a.txt");
        assert_conflict(ws, &expect);
        assert_eq!(
            fs::read_to_string(shared.path().join("a.txt")).unwrap(),
            "outside\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_only_withholds_affected_row() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = repo();
        let ws = tmp.path();
        fs::write(ws.join("private.txt"), "private\n").unwrap();
        fs::write(ws.join("healthy.txt"), "healthy\n").unwrap();
        let expect = expect_file("private.txt", rev(ws, "private.txt"));
        fs::set_permissions(ws.join("private.txt"), fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            fs::File::open(ws.join("private.txt")).is_err(),
            "fixture must be unreadable"
        );
        assert_unguardable_row(ws, "private.txt");
        assert_conflict(ws, &expect);
    }

    #[cfg(unix)]
    #[test]
    fn special_file_only_withholds_affected_row() {
        let tmp = repo();
        let ws = tmp.path();
        let expect = expect_file("a.txt", rev(ws, "a.txt"));
        fs::remove_file(ws.join("a.txt")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(ws.join("a.txt"))
                .status()
                .unwrap()
                .success()
        );
        fs::write(ws.join("healthy.txt"), "healthy\n").unwrap();
        assert_unguardable_row(ws, "a.txt");
        assert_conflict(ws, &expect);
    }

    #[test]
    fn broken_nested_repo_only_withholds_affected_row() {
        for tracked in [false, true] {
            let tmp = repo();
            let ws = tmp.path();
            let nested = ws.join("vendor");
            fs::create_dir(&nested).unwrap();
            git(&nested, &["init", "-q", "-b", "main"]);
            git(&nested, &["config", "user.email", "git-rev@example.test"]);
            git(&nested, &["config", "user.name", "Git Rev Test"]);
            fs::write(nested.join("lib.txt"), "library\n").unwrap();
            git(&nested, &["add", "lib.txt"]);
            git(&nested, &["commit", "-q", "-m", "nested"]);
            if tracked {
                git(ws, &["add", "vendor"]);
                git(ws, &["commit", "-q", "-m", "gitlink"]);
            }
            fs::write(
                nested.join(".git/refs/heads/main"),
                format!("{}\n", "1".repeat(40)),
            )
            .unwrap();
            fs::write(ws.join("healthy.txt"), "healthy\n").unwrap();
            assert_unguardable_row(ws, "vendor");
        }
    }

    #[cfg(unix)]
    #[test]
    fn status_and_guards_respect_clean_filters() {
        let tmp = repo();
        let ws = tmp.path();
        git(ws, &["config", "filter.up.clean", "tr a-z A-Z"]);
        fs::write(ws.join(".gitattributes"), "*.txt filter=up\n").unwrap();
        git(ws, &["add", ".gitattributes"]);
        // The tracked file is unchanged since the initial commit. Apply the
        // new attributes explicitly instead of depending on Git's cached
        // stat entry deciding to run the newly configured clean filter.
        git(ws, &["add", "--renormalize", "a.txt"]);
        assert_eq!(run_git_sync(ws, &["show", ":a.txt"]).unwrap(), "ONE\n");
        git(ws, &["commit", "-q", "-m", "filtered"]);
        fs::File::options()
            .write(true)
            .open(ws.join("a.txt"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(90))
            .unwrap();
        let detail = collect_git_status_detail(ws).unwrap();
        assert_eq!(detail.unstaged, 0);
        assert!(
            detail.files.is_empty(),
            "clean filtered files must not appear modified"
        );
        fs::write(ws.join("a.txt"), "two\n").unwrap();
        let detail = collect_git_status_detail(ws).unwrap();
        let expect = expect_file("a.txt", detail.files[0].rev.clone().unwrap());
        assert!(check_preconditions(ws, &expect).unwrap().is_ok());
        git(ws, &["add", "a.txt"]);
        assert_eq!(run_git_sync(ws, &["show", ":a.txt"]).unwrap(), "TWO\n");
        assert_conflict(ws, &expect);
    }

    #[test]
    fn status_respects_collapsed_and_hidden_untracked_modes() {
        let tmp = repo();
        let ws = tmp.path();
        fs::create_dir_all(ws.join("build/deep")).unwrap();
        fs::write(ws.join("build/one.txt"), "one\n").unwrap();
        fs::write(ws.join("build/deep/two.txt"), "two\n").unwrap();
        let detail = collect_git_status_detail(ws).unwrap();
        assert_eq!(detail.files.len(), 1);
        assert_eq!(detail.files[0].path, "build/");
        let expect = expect_file("build", detail.files[0].rev.clone().unwrap());
        assert!(check_preconditions(ws, &expect).unwrap().is_ok());
        fs::write(ws.join("build/deep/two.txt"), "changed\n").unwrap();
        assert_conflict(ws, &expect);
        git(ws, &["config", "status.showUntrackedFiles", "no"]);
        let detail = collect_git_status_detail(ws).unwrap();
        assert!(detail.files.is_empty());
        assert_eq!(detail.untracked, 0);
        assert!(
            detail.revision.is_none(),
            "hidden untracked content cannot guard add --all"
        );
        assert!(
            collect_git_status_detail(&ws.join("build"))
                .unwrap()
                .files
                .is_empty()
        );
    }

    #[test]
    fn index_token_does_not_enumerate_untracked_tree() {
        let tmp = repo();
        GIT_READS.with(|reads| reads.set(0));
        index_token(tmp.path());
        // One rev-parse for the frame and one ls-files --stage for the index.
        assert_eq!(GIT_READS.with(|reads| reads.get()), 2);
    }

    #[test]
    fn expect_validation_rules() {
        let parse = |value: Value| serde_json::from_value::<GitExpect>(value);
        let paths = vec!["a.txt".to_string()];
        let rev = format!("c-{}", "a".repeat(64));

        // Absent head is unchecked; explicit null means unborn.
        let unchecked =
            validate_expect(Some(parse(json!({})).unwrap()), ExpectTarget::Commit).unwrap();
        assert!(unchecked.is_empty());
        let unborn = validate_expect(
            Some(parse(json!({ "head": null })).unwrap()),
            ExpectTarget::Commit,
        )
        .unwrap();
        assert_eq!(unborn.head, Some(None));
        let upper = validate_expect(
            Some(parse(json!({ "head": "A".repeat(40) })).unwrap()),
            ExpectTarget::Commit,
        )
        .unwrap();
        assert_eq!(upper.head, Some(Some("a".repeat(40))));

        for bad in [
            json!({ "head": "abc" }),
            json!({ "head": "g".repeat(40) }),
            json!({ "index": "a".repeat(40) }),
            json!({ "revision": "zz" }),
            json!({ "revision": null }),
        ] {
            assert!(
                validate_expect(Some(parse(bad.clone()).unwrap()), ExpectTarget::Commit).is_err(),
                "{bad}"
            );
        }
        assert!(parse(json!({ "heads": null })).is_err(), "unknown key");

        let files = |map: Value| parse(json!({ "files": map })).unwrap();
        assert!(
            validate_expect(Some(files(json!({ "a.txt": rev }))), ExpectTarget::Commit).is_err()
        );
        assert!(
            validate_expect(Some(files(json!({ "a.txt": rev }))), ExpectTarget::AllPaths).is_err()
        );
        assert!(
            validate_expect(Some(files(json!({}))), ExpectTarget::Paths(&paths)).is_err(),
            "missing a requested path"
        );
        assert!(
            validate_expect(
                Some(files(json!({ "a.txt": rev, "b.txt": rev }))),
                ExpectTarget::Paths(&paths)
            )
            .is_err(),
            "extra path"
        );
        assert!(
            validate_expect(
                Some(files(json!({ "a.txt": "a".repeat(64) }))),
                ExpectTarget::Paths(&paths)
            )
            .is_err(),
            "rev without a mode"
        );
        let ok = validate_expect(
            Some(files(json!({ "./a.txt/": rev }))),
            ExpectTarget::Paths(&paths),
        );
        // `./` is refused by the shared path confinement, like request paths.
        assert!(ok.is_err());
        let ok = validate_expect(
            Some(files(json!({ "a.txt/": rev }))),
            ExpectTarget::Paths(&paths),
        )
        .unwrap();
        assert_eq!(ok.files.unwrap().keys().collect::<Vec<_>>(), vec!["a.txt"]);
    }

    /// A dropped (or timed-out) write takes down what git started — a hook, a
    /// filter, an alias — not just the `git` process.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_git_write_kills_what_git_started() {
        let tmp = repo();
        let pid_file = tmp.path().join("hang.pid");
        let args = vec![
            "-c".to_string(),
            "alias.hang=!sleep 300 & echo $! > hang.pid; wait".to_string(),
            "hang".to_string(),
        ];
        let grandchild = codewhale_runtime::process_tree::drop_once_pid_written(
            git_write(tmp.path(), args),
            &pid_file,
        )
        .await;
        assert!(
            codewhale_runtime::process_tree::wait_for_pid_exit(grandchild, Duration::from_secs(5)),
            "a process git started outlived the dropped request"
        );
    }

    #[test]
    fn file_diff_reads_the_path_literally_inside_a_subdirectory_workspace() {
        let tmp = repo();
        let root = tmp.path();
        fs::create_dir_all(root.join("apps/web")).unwrap();
        fs::create_dir_all(root.join("apps/other")).unwrap();
        fs::write(root.join("apps/web/w.txt"), "w\n").unwrap();
        fs::write(root.join("apps/other/secret.txt"), "old\n").unwrap();
        git(root, &["add", "apps"]);
        git(root, &["commit", "-q", "-m", "apps"]);
        fs::write(root.join("apps/web/w.txt"), "w2\n").unwrap();
        fs::write(root.join("apps/other/secret.txt"), "new\n").unwrap();
        let workspace = root.join("apps/web");
        let diff = |path: &str| {
            let output = Git::review_command(&workspace)
                .unwrap()
                .args(file_diff_args("HEAD", path))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        assert!(diff("w.txt").contains("+w2"), "in-workspace diff works");
        for path in [
            ":/apps/other/secret.txt",
            ":(top)apps/other/secret.txt",
            "*",
        ] {
            let body = diff(path);
            assert!(!body.contains("secret"), "{path} escaped: {body}");
        }
    }

    #[tokio::test]
    async fn push_args_validate_the_effective_remote_and_end_options() {
        let tmp = repo();
        let root = tmp.path();
        let status = |error: ApiError| error.status;

        // No remote configured: plain `push` stays git's call, but an
        // upstream push defaults to `origin`, which must exist too.
        assert_eq!(push_args(root, None, false).await.unwrap(), ["push"]);
        assert_eq!(push_args(root, Some("  "), false).await.unwrap(), ["push"]);
        assert_eq!(
            push_args(root, None, true)
                .await
                .map_err(status)
                .unwrap_err(),
            StatusCode::BAD_REQUEST
        );

        let bare = tempfile::tempdir().unwrap();
        git(bare.path(), &["init", "-q", "--bare"]);
        git(
            root,
            &["remote", "add", "origin", &bare.path().to_string_lossy()],
        );
        assert_eq!(
            push_args(root, Some(" origin "), false).await.unwrap(),
            ["push", "--", "origin"]
        );
        for remote in ["--mirror", "fork", "../other"] {
            assert_eq!(
                push_args(root, Some(remote), true)
                    .await
                    .map_err(status)
                    .unwrap_err(),
                StatusCode::BAD_REQUEST,
                "{remote}"
            );
        }
        let args = push_args(root, None, true).await.unwrap();
        assert_eq!(args, ["push", "--set-upstream", "--", "origin", "main"]);

        // The built arguments are ones git accepts: the push lands in the
        // bare remote and records the upstream.
        git(root, &args.iter().map(String::as_str).collect::<Vec<_>>());
        let upstream = run_git_sync(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]).unwrap();
        assert_eq!(upstream.trim(), "origin/main");
    }

    #[test]
    fn push_remote_must_be_a_configured_remote_name() {
        let configured = "origin\nupstream\n";
        assert!(validate_push_remote("origin", configured).is_ok());
        assert!(validate_push_remote("upstream", configured).is_ok());
        for remote in [
            "--force",
            "--mirror",
            "-f",
            "--delete",
            "../other-repo",
            "fork",
        ] {
            let error = validate_push_remote(remote, configured).expect_err(remote);
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{remote}");
        }
    }
}

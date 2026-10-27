//! Git power tools: `git_status`, `git_diff`, and the propose-only
//! `git_commit_plan`.
//!
//! These tools are read-only wrappers around common git inspection commands,
//! scoped to the workspace and optionally to a sub-path within it. The commit
//! planner (#3999) is read-only too: it proposes an ordered atomic split and
//! leaves staging and committing to the ordinary shell write path.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use codewhale_dependencies::ExternalTool;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, optional_u64,
};

const DEFAULT_UNIFIED: u64 = 3;
const MAX_UNIFIED: u64 = 50;

/// Resolve untrusted revision text before using it as an argument to another
/// Git command. Only a verified commit ID crosses that option boundary.
pub(super) async fn resolve_commit_ref(workspace: &Path, base: &str) -> Result<String, ToolError> {
    let workspace = workspace.to_path_buf();
    let revision = format!("{base}^{{commit}}");
    let output = tokio::task::spawn_blocking(move || {
        run_git_command(
            &workspace,
            &[
                "rev-parse".to_string(),
                "--verify".to_string(),
                "--end-of-options".to_string(),
                revision,
            ],
        )
    })
    .await
    .map_err(|error| {
        ToolError::execution_failed(format!("git resolve task panicked: {error}"))
    })??;
    if !output.status.success() {
        return Err(ToolError::invalid_input(format!(
            "Invalid git base ref '{base}': {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ToolError::execution_failed(
            "git resolved base to an invalid commit id",
        ));
    }
    Ok(commit)
}

// === GitStatusTool ===

/// Tool for reading the concise git status of the workspace.
pub struct GitStatusTool;

#[async_trait]
impl ToolSpec for GitStatusTool {
    fn name(&self) -> &'static str {
        "git_status"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Run `git status --porcelain=v1 -b` in the workspace (optionally scoped to a path)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file to scope the status to (must be within the workspace)."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let context = context.clone();
        run_git_tool_blocking(move || git_status_blocking(input, &context)).await
    }
}

fn git_status_blocking(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let git_ctx = resolve_git_context(context, optional_str(&input, "path")?)?;

    let mut args = vec![
        "-c".to_string(),
        "core.quotepath=false".to_string(),
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "-b".to_string(),
    ];
    if let Some(pathspec) = &git_ctx.pathspec {
        args.push("--".to_string());
        args.push(pathspec.display().to_string());
    }

    let command_str = format_command(&git_ctx.working_dir, &args);
    let output = run_git_command(&git_ctx.working_dir, &args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!("git status failed: {}", stderr.trim());
        return Ok(ToolResult::error(message).with_metadata(json!({
            "command": command_str,
            "exit_code": output.status.code(),
            "stderr": stderr.trim(),
        })));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let content = stdout.into_owned();

    Ok(ToolResult::success(content).with_metadata(json!({
        "command": command_str,
        "working_dir": git_ctx.working_dir,
        "pathspec": git_ctx.pathspec,
    })))
}

// === GitDiffTool ===

/// Tool for reading git diffs in the workspace.
pub struct GitDiffTool;

#[async_trait]
impl ToolSpec for GitDiffTool {
    fn name(&self) -> &'static str {
        "git_diff"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Run `git diff` in the workspace with sensible defaults and safe truncation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file to scope the diff to (must be within the workspace)."
                },
                "cached": {
                    "type": "boolean",
                    "description": "When true, diff staged changes (`--cached`)."
                },
                "unified": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_UNIFIED,
                    "default": DEFAULT_UNIFIED,
                    "description": "Number of context lines to include around changes."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let context = context.clone();
        run_git_tool_blocking(move || git_diff_blocking(input, &context)).await
    }
}

fn git_diff_blocking(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let git_ctx = resolve_git_context(context, optional_str(&input, "path")?)?;
    let cached = optional_bool(&input, "cached", false)?;
    let unified = optional_u64(&input, "unified", DEFAULT_UNIFIED)?.min(MAX_UNIFIED);

    let mut args = vec![
        "-c".to_string(),
        "core.quotepath=false".to_string(),
        "diff".to_string(),
        "--no-color".to_string(),
    ];
    args.extend(codewhale_dependencies::Git::REVIEW_DIFF_ARGS.map(String::from));
    args.push(format!("--unified={unified}"));
    if cached {
        args.push("--cached".to_string());
    }
    if let Some(pathspec) = &git_ctx.pathspec {
        args.push("--".to_string());
        args.push(pathspec.display().to_string());
    }

    let command_str = format_command(&git_ctx.working_dir, &args);
    let output = run_git_command(&git_ctx.working_dir, &args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!("git diff failed: {}", stderr.trim());
        return Ok(ToolResult::error(message).with_metadata(json!({
            "command": command_str,
            "exit_code": output.status.code(),
            "stderr": stderr.trim(),
        })));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let content = stdout.into_owned();

    Ok(ToolResult::success(content).with_metadata(json!({
        "command": command_str,
        "working_dir": git_ctx.working_dir,
        "pathspec": git_ctx.pathspec,
        "cached": cached,
        "unified": unified,
    })))
}

// === GitCommitPlanTool ===

/// Propose-only planner that splits the working tree into ordered atomic
/// commits (#3999).
///
/// The planner reads `git diff HEAD` plus the untracked-file list, groups
/// whole files into logical commits, orders the groups so a commit that
/// defines a symbol lands before the commit that uses it, and refuses the
/// whole plan when that dependency graph has a cycle. It never touches the
/// index or the object store — no `git add -N`, no `git apply --cached`, no
/// `git commit`. The model lands each proposed commit through the ordinary
/// `git add` / `git commit` shell path, which is where the approval gate
/// already lives: one commit authority, not two.
pub struct GitCommitPlanTool;

#[async_trait]
impl ToolSpec for GitCommitPlanTool {
    fn name(&self) -> &'static str {
        "git_commit_plan"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Propose how to split the working tree into ordered atomic commits. Read-only: returns the plan and writes nothing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file to scope the plan to (must be within the workspace)."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let context = context.clone();
        run_git_tool_blocking(move || git_commit_plan_blocking(input, &context)).await
    }
}

fn git_commit_plan_blocking(input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let git_ctx = resolve_git_context(context, optional_str(&input, "path")?)?;
    let working_dir = &git_ctx.working_dir;

    let root_args = vec!["rev-parse".to_string(), "--show-toplevel".to_string()];
    let repo_root = match git_stdout(working_dir, &root_args)? {
        Ok(stdout) => PathBuf::from(String::from_utf8_lossy(&stdout).trim_end()),
        Err(failure) => return Ok(failure),
    };

    let mut diff_args = vec![
        "-c".to_string(),
        "core.quotepath=false".to_string(),
        "diff".to_string(),
        "HEAD".to_string(),
        "--no-color".to_string(),
        "-U3".to_string(),
    ];
    diff_args.extend(codewhale_dependencies::Git::REVIEW_DIFF_ARGS.map(String::from));
    if let Some(pathspec) = &git_ctx.pathspec {
        diff_args.push("--".to_string());
        diff_args.push(pathspec.display().to_string());
    }
    let command = format_command(working_dir, &diff_args);
    let diff_output = run_git_command(working_dir, &diff_args)?;
    let mut files = match stdout_or_failure(working_dir, &diff_args, diff_output) {
        Ok(stdout) => parse_diff(&String::from_utf8_lossy(&stdout)),
        Err(failure) => return Ok(failure),
    };

    // Untracked files are listed by path and read for symbol analysis.
    // Never `git add -N` them: intent-to-add mutates the index, and a
    // planner that mutates the index is not propose-only.
    let mut untracked_args = vec![
        "-c".to_string(),
        "core.quotepath=false".to_string(),
        "ls-files".to_string(),
        "--others".to_string(),
        "--exclude-standard".to_string(),
        "--full-name".to_string(),
        "-z".to_string(),
    ];
    if let Some(pathspec) = &git_ctx.pathspec {
        untracked_args.push("--".to_string());
        untracked_args.push(pathspec.display().to_string());
    }
    match git_stdout(working_dir, &untracked_args)? {
        Ok(stdout) => {
            for path in String::from_utf8_lossy(&stdout)
                .split('\0')
                .filter(|path| !path.is_empty())
            {
                files.push(ChangedFile {
                    path: path.to_string(),
                    hunks: untracked_hunk(&repo_root, path).into_iter().collect(),
                    untracked: true,
                });
            }
        }
        Err(failure) => return Ok(failure),
    }

    if files.is_empty() {
        return Ok(
            ToolResult::success("No changes to plan: the working tree matches HEAD.")
                .with_metadata(json!({
                    "command": command,
                    "propose_only": true,
                    "commits": [],
                })),
        );
    }

    let mut staged_args = vec![
        "diff".to_string(),
        "--cached".to_string(),
        "--quiet".to_string(),
    ];
    staged_args.extend(codewhale_dependencies::Git::REVIEW_DIFF_ARGS.map(String::from));
    let index_has_staged_changes =
        run_git_command(working_dir, &staged_args)?.status.code() == Some(1);

    let commits = match plan_commits(files) {
        Ok(commits) => commits,
        Err(cycle) => {
            let message = format!(
                "Dependency cycle detected among changes in: {}. Atomic commit split rejected; nothing was written.\nCycle edges:\n{}",
                cycle.files.join(", "),
                cycle
                    .edges
                    .iter()
                    .map(|edge| format!("  {edge}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Ok(ToolResult::error(message).with_metadata(json!({
                "command": command,
                "propose_only": true,
                "cycle_detected": true,
                "cyclic_files": cycle.files,
                "cycle_edges": cycle.edges,
            })));
        }
    };

    let content = render_commit_plan(&repo_root, index_has_staged_changes, &commits);
    let metadata_commits: Vec<Value> = commits
        .iter()
        .enumerate()
        .map(|(idx, commit)| {
            json!({
                "order": idx + 1,
                "message": commit.message,
                "files": commit.files.iter().filter(|f| !f.untracked).map(|f| &f.path).collect::<Vec<_>>(),
                "untracked": commit.files.iter().filter(|f| f.untracked).map(|f| &f.path).collect::<Vec<_>>(),
                "hunks": commit.files.iter().flat_map(|f| f.hunks.iter().map(|h| json!({"file": h.file_path, "header": h.header}))).collect::<Vec<_>>(),
                "defines": commit.defines,
                "depends_on": commit.depends_on.iter().map(|(order, reason)| json!({"order": order, "reason": reason})).collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(ToolResult::success(content).with_metadata(json!({
        "command": command,
        "repo_root": repo_root.display().to_string(),
        "propose_only": true,
        "cycle_detected": false,
        "index_has_staged_changes": index_has_staged_changes,
        "commits": metadata_commits,
    })))
}

// === Helpers ===

struct GitContext {
    working_dir: PathBuf,
    pathspec: Option<PathBuf>,
}

fn resolve_git_context(context: &ToolContext, path: Option<&str>) -> Result<GitContext, ToolError> {
    let workspace = canonical_or_workspace(&context.workspace);
    let mut working_dir = workspace.clone();
    let mut pathspec = None;

    if let Some(raw) = path {
        let resolved = context.resolve_path(raw)?;
        let metadata = fs::metadata(&resolved).map_err(|e| {
            ToolError::invalid_input(format!(
                "Path does not exist or is not accessible: {raw} ({e})"
            ))
        })?;

        if metadata.is_dir() {
            working_dir = resolved;
            pathspec = Some(PathBuf::from("."));
        } else {
            // For file paths, run from the parent and scope to the file name.
            let parent = resolved.parent().ok_or_else(|| {
                ToolError::invalid_input(format!("Path has no parent directory: {raw}"))
            })?;
            working_dir = parent.to_path_buf();
            pathspec = Some(pathspec_from(&working_dir, &resolved));
        }
    }

    if !working_dir.exists() {
        return Err(ToolError::invalid_input(format!(
            "Working directory does not exist: {}",
            working_dir.display()
        )));
    }

    Ok(GitContext {
        working_dir,
        pathspec,
    })
}

fn canonical_or_workspace(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

fn pathspec_from(working_dir: &Path, resolved: &Path) -> PathBuf {
    match resolved.strip_prefix(working_dir) {
        Ok(rel) if rel.as_os_str().is_empty() => PathBuf::from("."),
        Ok(rel) => rel.to_path_buf(),
        Err(_) => PathBuf::from("."),
    }
}

/// Build the git command for the read-only workspace tools (`git_status`,
/// `git_diff`, `git_log`, `git_show`, `git_blame`, `verify`). It goes through
/// [`codewhale_dependencies::Git::review_command`], so fsmonitor, hooks and
/// clean/process filters from the workspace's own config do not run.
pub(super) fn read_only_git_command(
    working_dir: &Path,
) -> Result<std::process::Command, ToolError> {
    if !codewhale_dependencies::Git::available() {
        return Err(ToolError::not_available(
            "git is not installed or not in PATH",
        ));
    }
    codewhale_dependencies::Git::review_command(working_dir)
        .map_err(|e| ToolError::execution_failed(format!("Failed to prepare git: {e:#}")))
}

/// Run a git tool's synchronous body (path resolution, filter discovery,
/// `git` itself) on the blocking pool rather than an async worker
/// (#6561 D03-m1).
///
/// Known limitation: there is no deadline or cancellation here — a dropped
/// call leaves the blocking `git` to finish. These are local read-only
/// commands with fsmonitor, hooks and filters disabled; `git_fetch`, the
/// one network command, runs contained under a deadline instead.
async fn run_git_tool_blocking(
    work: impl FnOnce() -> Result<ToolResult, ToolError> + Send + 'static,
) -> Result<ToolResult, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ToolError::execution_failed(format!("git task panicked: {error}")))?
}

pub(super) fn run_git_command(
    working_dir: &Path,
    args: &[String],
) -> Result<std::process::Output, ToolError> {
    let mut cmd = read_only_git_command(working_dir)?;
    cmd.args(args).current_dir(working_dir);
    cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::not_available("git is not installed or not in PATH")
        } else {
            ToolError::execution_failed(format!("Failed to run git: {e}"))
        }
    })
}

fn format_command(working_dir: &Path, args: &[String]) -> String {
    // `[String]::join` produces the same string as collecting `&str` first, so
    // join the slice directly and skip the intermediate `Vec<&str>` allocation.
    format!("git -C {} {}", working_dir.display(), args.join(" "))
}

// === Commit Split Specific Types & Helpers ===

// === Commit plan: types, parsing, grouping, ordering ===

/// Largest untracked file the planner reads for symbol analysis. Bigger or
/// binary files are still listed by path; they just carry no hunks.
const MAX_UNTRACKED_BYTES: u64 = 1 << 20;

/// One hunk of a unified diff, kept for symbol analysis and for the plan's
/// per-file hunk listing. The `@@` ranges stay in `header`: nothing rebuilds
/// a patch from them anymore, so parsed copies would be dead weight.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub file_path: String,
    pub header: String,
    pub lines: Vec<String>,
}

/// One file the working tree changed relative to HEAD. Tracked binary and
/// mode-only changes carry no hunks; untracked files carry a synthesized
/// all-additions hunk when they are readable text.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub untracked: bool,
}

/// One proposed commit in dependency order.
#[derive(Debug, Clone)]
pub struct PlannedCommit {
    pub message: String,
    /// Sorted by path; every hunk of a file stays in the same commit.
    pub files: Vec<ChangedFile>,
    pub defines: Vec<String>,
    /// `(order, reason)` pairs naming the earlier commits this one builds on.
    pub depends_on: Vec<(usize, String)>,
}

/// Why a plan was refused: the files on the cycle and the edges that close it.
#[derive(Debug, Clone)]
pub struct CycleDiagnostic {
    pub files: Vec<String>,
    pub edges: Vec<String>,
}

struct CommitGroup {
    files: BTreeMap<String, ChangedFile>,
    defined_symbols: BTreeSet<String>,
    referenced_symbols: BTreeSet<String>,
}

impl CommitGroup {
    fn from_file(file: ChangedFile) -> Self {
        let mut defined_symbols = BTreeSet::new();
        let mut referenced_symbols = BTreeSet::new();
        for hunk in &file.hunks {
            defined_symbols.extend(extract_defined_symbols(hunk));
            referenced_symbols.extend(extract_referenced_symbols(hunk));
        }
        let mut group = Self {
            files: BTreeMap::from([(file.path.clone(), file)]),
            defined_symbols,
            referenced_symbols,
        };
        group.drop_self_references();
        group
    }

    fn absorb(&mut self, other: CommitGroup) {
        self.files.extend(other.files);
        self.defined_symbols.extend(other.defined_symbols);
        self.referenced_symbols.extend(other.referenced_symbols);
        self.drop_self_references();
    }

    fn drop_self_references(&mut self) {
        for symbol in &self.defined_symbols {
            self.referenced_symbols.remove(symbol);
        }
    }

    fn has_source_file(&self) -> bool {
        self.files.keys().any(|path| is_source_file(path))
    }

    fn first_path(&self) -> &str {
        self.files.keys().next().map_or("", String::as_str)
    }
}

/// Run git and hand back stdout, or the operator-facing failure result.
fn git_stdout(
    working_dir: &Path,
    args: &[String],
) -> Result<Result<Vec<u8>, ToolResult>, ToolError> {
    let output = run_git_command(working_dir, args)?;
    Ok(stdout_or_failure(working_dir, args, output))
}

fn stdout_or_failure(
    working_dir: &Path,
    args: &[String],
    output: std::process::Output,
) -> Result<Vec<u8>, ToolResult> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ToolResult::error(format!(
        "{} failed: {}",
        format_command(working_dir, args),
        stderr.trim()
    )))
}

/// Parse `git diff` output into per-file hunks. Every `diff --git` section
/// yields a file even when it has no hunks (binary or mode-only change), so
/// no changed file can silently drop out of the plan.
fn parse_diff(diff_output: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;

    let flush = |files: &mut Vec<ChangedFile>, hunk: Option<Hunk>| {
        if let (Some(hunk), Some(file)) = (hunk, files.last_mut()) {
            file.hunks.push(hunk);
        }
    };

    for line in diff_output.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&mut files, current_hunk.take());
            let path = rest
                .rfind(" b/")
                .map(|pos| &rest[pos + 3..])
                .unwrap_or(rest)
                .trim_matches('"')
                .to_string();
            files.push(ChangedFile {
                path,
                hunks: Vec::new(),
                untracked: false,
            });
        } else if line.starts_with("@@ ") {
            flush(&mut files, current_hunk.take());
            let Some(file) = files.last() else { continue };
            current_hunk = Some(Hunk {
                file_path: file.path.clone(),
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current_hunk.as_mut() {
            hunk.lines.push(line.to_string());
        }
    }
    flush(&mut files, current_hunk.take());
    files
}

/// Synthesize an all-additions hunk for an untracked text file so its
/// symbols take part in grouping. Binary, oversized, or unreadable files
/// yield `None` and are listed by path only. So does a path that is a link
/// or sits under one: an untracked link must not make this read-only tool
/// return text from outside the repository.
fn untracked_hunk(repo_root: &Path, path: &str) -> Option<Hunk> {
    use std::io::Read as _;
    let full = repo_root.join(path);
    let file = codewhale_util::fs_confined::open_read(repo_root, &full).ok()?;
    if file.metadata().ok()?.len() > MAX_UNTRACKED_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_UNTRACKED_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.iter().take(8000).any(|byte| *byte == 0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let lines: Vec<String> = text.lines().map(|line| format!("+{line}")).collect();
    Some(Hunk {
        file_path: path.to_string(),
        header: format!("@@ -0,0 +1,{} @@", lines.len()),
        lines,
    })
}

/// Group changed files into commits and order them by dependency.
///
/// Pure: reads nothing from git and writes nothing anywhere. Returns the
/// cycle diagnostic instead of a plan when the dependency graph is not a DAG.
fn plan_commits(files: Vec<ChangedFile>) -> Result<Vec<PlannedCommit>, CycleDiagnostic> {
    let (lock_files, files): (Vec<ChangedFile>, Vec<ChangedFile>) =
        files.into_iter().partition(|file| is_lock_file(&file.path));

    let mut groups: Vec<CommitGroup> = files.into_iter().map(CommitGroup::from_file).collect();

    // Lock files are excluded from symbol analysis and ride with the manifest
    // change that moved them; a lock file with no manifest change stands alone.
    for lock in lock_files {
        let lock_path = lock.path.clone();
        let mut group = CommitGroup::from_file(lock);
        group.defined_symbols.clear();
        group.referenced_symbols.clear();
        match groups.iter_mut().find(|existing| {
            existing
                .files
                .keys()
                .any(|manifest| matches_lock_file(manifest, &lock_path))
        }) {
            Some(manifest_group) => manifest_group.files.extend(group.files),
            None => groups.push(group),
        }
    }

    // Merge files with closely related names (source with its test/spec).
    let mut merged: Vec<CommitGroup> = Vec::new();
    for group in groups {
        let related = merged.iter_mut().find(|existing| {
            group
                .files
                .keys()
                .any(|a| existing.files.keys().any(|b| are_files_related(a, b)))
        });
        match related {
            Some(existing) => existing.absorb(group),
            None => merged.push(group),
        }
    }
    let groups = merged;

    // Dependency graph: `edges[j]` lists the groups that must land after j.
    let n = groups.len();
    let mut edges: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
    let mut in_degree = vec![0usize; n];
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let reason = groups[i]
                .referenced_symbols
                .iter()
                .find(|symbol| groups[j].defined_symbols.contains(*symbol))
                .map(|symbol| format!("uses `{symbol}`"))
                .or_else(|| {
                    // Tests, docs, and configs follow the source change that
                    // lives beside them.
                    (!groups[i].has_source_file() && groups[j].has_source_file())
                        .then(|| {
                            groups[i]
                                .files
                                .keys()
                                .any(|a| groups[j].files.keys().any(|b| share_context(a, b)))
                        })
                        .filter(|shares| *shares)
                        .map(|_| "follows the source change in the same directory".to_string())
                });
            if let Some(reason) = reason {
                edges[j].push((i, reason));
                in_degree[i] += 1;
            }
        }
    }

    // Kahn's algorithm; among ready groups, source changes land first, then
    // path order, so the plan is deterministic.
    let mut ready: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order: Vec<usize> = Vec::with_capacity(n);
    while !ready.is_empty() {
        ready.sort_by(|&a, &b| {
            groups[b]
                .has_source_file()
                .cmp(&groups[a].has_source_file())
                .then_with(|| groups[a].first_path().cmp(groups[b].first_path()))
        });
        let current = ready.remove(0);
        order.push(current);
        for (next, _) in &edges[current] {
            in_degree[*next] -= 1;
            if in_degree[*next] == 0 {
                ready.push(*next);
            }
        }
    }

    if order.len() < n {
        let cyclic: Vec<usize> = (0..n).filter(|&i| in_degree[i] > 0).collect();
        let files = cyclic
            .iter()
            .flat_map(|&i| groups[i].files.keys().cloned())
            .collect();
        let mut cycle_edges = Vec::new();
        for &j in &cyclic {
            for (i, reason) in &edges[j] {
                if cyclic.contains(i) {
                    cycle_edges.push(format!(
                        "{} -> {} ({reason})",
                        groups[*i].first_path(),
                        groups[j].first_path()
                    ));
                }
            }
        }
        return Err(CycleDiagnostic {
            files,
            edges: cycle_edges,
        });
    }

    let mut position = vec![0usize; n];
    for (idx, &group) in order.iter().enumerate() {
        position[group] = idx + 1;
    }
    let mut depends_on: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
    for (j, outgoing) in edges.iter().enumerate() {
        for (i, reason) in outgoing {
            depends_on[*i].push((position[j], reason.clone()));
        }
    }

    Ok(order
        .into_iter()
        .map(|idx| {
            let mut deps = std::mem::take(&mut depends_on[idx]);
            deps.sort();
            let group = &groups[idx];
            PlannedCommit {
                message: generate_commit_message(group),
                files: group.files.values().cloned().collect(),
                defines: group.defined_symbols.iter().cloned().collect(),
                depends_on: deps,
            }
        })
        .collect())
}

fn render_commit_plan(
    repo_root: &Path,
    index_has_staged_changes: bool,
    commits: &[PlannedCommit],
) -> String {
    let mut out = format!(
        "Commit plan for {}: {} commit{} (propose-only; nothing was staged or committed).\n\
         Groups are whole files. Land each in order from the repo root with \
         `git add -- <files>` then `git commit -m '<message>'`; those commands go \
         through the normal shell approval gate.\n",
        repo_root.display(),
        commits.len(),
        if commits.len() == 1 { "" } else { "s" }
    );
    if index_has_staged_changes {
        out.push_str(
            "WARNING: the index already holds staged changes. Run `git reset` before \
             staging commit 1, or those hunks will ride into it.\n",
        );
    }
    for (idx, commit) in commits.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n", idx + 1, commit.message));
        for file in &commit.files {
            let hunks = match file.hunks.len() {
                0 if file.untracked => "untracked; listed by path".to_string(),
                0 => "no text hunks (binary or mode change)".to_string(),
                1 => format!("1 hunk: {}", file.hunks[0].header),
                count => format!(
                    "{count} hunks: {}",
                    file.hunks
                        .iter()
                        .map(|hunk| hunk.header.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            };
            let flag = if file.untracked { " (untracked)" } else { "" };
            out.push_str(&format!("   {}{flag} — {hunks}\n", file.path));
        }
        if !commit.defines.is_empty() {
            out.push_str(&format!("   defines: {}\n", commit.defines.join(", ")));
        }
        if commit.depends_on.is_empty() {
            out.push_str("   depends on: none\n");
        } else {
            let deps: Vec<String> = commit
                .depends_on
                .iter()
                .map(|(order, reason)| format!("{order} ({reason})"))
                .collect();
            out.push_str(&format!("   depends on: {}\n", deps.join(", ")));
        }
    }
    out
}

fn is_lock_file(path: &str) -> bool {
    let name = file_name(path);
    name.ends_with(".lock")
        || matches!(
            name,
            "go.sum" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock"
        )
}

fn is_manifest_file(path: &str) -> bool {
    matches!(file_name(path), "Cargo.toml" | "package.json" | "go.mod")
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

fn file_stem(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
}

fn matches_lock_file(manifest: &str, lock: &str) -> bool {
    let m_path = Path::new(manifest);
    let l_path = Path::new(lock);
    if m_path.parent() != l_path.parent() {
        return false;
    }
    match (file_name(manifest), file_name(lock)) {
        ("Cargo.toml", "Cargo.lock") | ("go.mod", "go.sum") => true,
        ("package.json", "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml") => true,
        (m_name, l_name) => {
            file_stem(manifest) == file_stem(lock)
                || (m_name.ends_with(".json") && l_name.ends_with(".json"))
        }
    }
}

/// Lowercased file stem with test/spec markers removed — the name two
/// related files share (`math.rs` and `math_test.rs` both reduce to `math`).
fn related_stem(path: &str) -> String {
    file_stem(path)
        .to_lowercase()
        .replace("_test", "")
        .replace("test_", "")
        .replace("_spec", "")
        .replace("spec_", "")
        .replace("test", "")
}

fn are_files_related(f1: &str, f2: &str) -> bool {
    let stem1 = file_stem(f1).to_lowercase();
    let stem2 = file_stem(f2).to_lowercase();
    if stem1 == stem2 {
        return true;
    }
    let clean1 = related_stem(f1);
    !clean1.is_empty() && clean1 == related_stem(f2)
}

fn is_source_file(path: &str) -> bool {
    let p = Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let name = file_name(path).to_lowercase();
    if name.contains("test") || name.contains("spec") || name.contains("mock") {
        return false;
    }
    matches!(
        ext,
        "rs" | "py" | "go" | "js" | "ts" | "cpp" | "h" | "c" | "java" | "cs" | "rb" | "php"
    )
}

fn is_doc_file(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|e| e.to_str()),
        Some("md" | "rst" | "txt" | "adoc")
    )
}

fn share_context(f1: &str, f2: &str) -> bool {
    Path::new(f1).parent() == Path::new(f2).parent()
}

const DEFINING_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "def",
    "function",
    "struct",
    "enum",
    "trait",
    "class",
    "interface",
    "type",
    "const",
    "let",
    "mod",
];

fn extract_defined_symbols(hunk: &Hunk) -> HashSet<String> {
    let mut symbols = HashSet::new();
    for content in added_lines(hunk) {
        let tokens = tokenize(content);
        for pair in tokens.windows(2) {
            if DEFINING_KEYWORDS.contains(&pair[0].as_str()) && is_valid_identifier(&pair[1]) {
                symbols.insert(pair[1].clone());
            }
        }
    }
    symbols
}

fn extract_referenced_symbols(hunk: &Hunk) -> HashSet<String> {
    let mut symbols = HashSet::new();
    for content in added_lines(hunk) {
        for token in tokenize(content) {
            if is_valid_identifier(&token) && !is_keyword(&token) {
                symbols.insert(token);
            }
        }
    }
    symbols
}

fn added_lines(hunk: &Hunk) -> impl Iterator<Item = &str> {
    hunk.lines
        .iter()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .map(|line| &line[1..])
}

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "if" | "else"
            | "while"
            | "for"
            | "return"
            | "import"
            | "use"
            | "pub"
            | "impl"
            | "crate"
            | "self"
            | "true"
            | "false"
            | "let"
            | "mut"
            | "match"
            | "var"
            | "void"
            | "int"
            | "string"
            | "bool"
            | "float"
            | "double"
            | "public"
            | "private"
            | "protected"
            | "static"
            | "final"
            | "class"
            | "fn"
            | "struct"
            | "enum"
            | "trait"
            | "interface"
            | "type"
            | "const"
            | "mod"
            | "def"
            | "func"
            | "function"
            | "and"
            | "or"
            | "not"
            | "in"
            | "as"
            | "break"
            | "continue"
            | "new"
            | "this"
            | "super"
    )
}

/// A conventional-commit proposal for one group. The model is expected to
/// refine it; the point is that it names the group's single concern rather
/// than "wip".
fn generate_commit_message(group: &CommitGroup) -> String {
    let paths: Vec<&str> = group.files.keys().map(String::as_str).collect();
    let scope = match paths.as_slice() {
        [single] => file_stem(single).to_string(),
        _ => {
            // A test or spec rides with the source it names; when every file
            // in the group shares that stem, the stem is the subject. An
            // unrelated group falls back to its directory name.
            let shared = related_stem(paths[0]);
            if !shared.is_empty() && paths.iter().all(|path| related_stem(path) == shared) {
                shared
            } else {
                paths
                    .iter()
                    .map(|path| Path::new(path).parent())
                    .reduce(|a, b| if a == b { a } else { None })
                    .flatten()
                    .and_then(|dir| dir.file_name().and_then(|n| n.to_str()))
                    .map_or_else(|| "repo".to_string(), str::to_string)
            }
        }
    };
    if !group.defined_symbols.is_empty() {
        let shown: Vec<&str> = group
            .defined_symbols
            .iter()
            .take(3)
            .map(String::as_str)
            .collect();
        let more = group.defined_symbols.len().saturating_sub(shown.len());
        let suffix = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        return format!("feat({scope}): add {}{suffix}", shown.join(", "));
    }
    let names: Vec<&str> = paths.iter().map(|path| file_name(path)).collect();
    let kind = if paths
        .iter()
        .all(|path| is_lock_file(path) || is_manifest_file(path))
    {
        return format!("chore(deps): update {}", names.join(", "));
    } else if paths.iter().all(|path| is_doc_file(path)) {
        "docs"
    } else if paths
        .iter()
        .all(|path| !is_source_file(path) && file_name(path).to_lowercase().contains("test"))
    {
        "test"
    } else {
        "chore"
    };
    format!("{kind}({scope}): update {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn git_available() -> bool {
        codewhale_dependencies::Git::available()
    }

    fn init_git_repo(root: &Path) {
        let run = |args: &[&str]| {
            let status = codewhale_dependencies::Git::status(args, root).expect("git should spawn");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "-q"]);
        run(&["config", "core.autocrlf", "false"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    fn commit_all(root: &Path, message: &str) {
        let run = |args: &[&str]| {
            let status = codewhale_dependencies::Git::status(args, root).expect("git should spawn");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["add", "."]);
        run(&["commit", "-q", "-m", message]);
    }

    /// The read-only git tools must not run programs named by the
    /// workspace's own config: `core.fsmonitor` would otherwise execute on
    /// every `git_status`.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_only_git_tools_do_not_run_workspace_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("file.txt"), "hello\n").expect("write");
        commit_all(tmp.path(), "init");
        fs::write(tmp.path().join("file.txt"), "hello\nworld\n").expect("modify");

        let hooks = tempdir().expect("hooks");
        let marker = hooks.path().join("ran");
        let hook = hooks.path().join("fsmonitor.sh");
        fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .expect("write hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
        let status = codewhale_dependencies::Git::status(
            &["config", "core.fsmonitor", &hook.to_string_lossy()],
            tmp.path(),
        )
        .expect("git config");
        assert!(status.success());

        let ctx = ToolContext::new(tmp.path());
        let status = GitStatusTool
            .execute(json!({}), &ctx)
            .await
            .expect("status");
        assert!(status.success, "{}", status.content);
        assert!(status.content.contains("file.txt"), "{}", status.content);
        let diff = GitDiffTool.execute(json!({}), &ctx).await.expect("diff");
        assert!(diff.success, "{}", diff.content);
        assert!(diff.content.contains("+world"), "{}", diff.content);
        assert!(!marker.exists(), "workspace fsmonitor hook ran");
    }

    #[tokio::test]
    async fn git_status_reports_branch_and_changes() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let file = tmp.path().join("file.txt");
        fs::write(&file, "hello\n").expect("write");
        commit_all(tmp.path(), "init");

        fs::write(&file, "hello\nworld\n").expect("modify");

        let ctx = ToolContext::new(tmp.path());
        let tool = GitStatusTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);
        assert!(result.content.contains("##"));
        assert!(result.content.contains("file.txt"));
    }

    #[tokio::test]
    async fn git_status_reports_unquoted_unicode_paths() {
        if !git_available() {
            return;
        }

        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let file = tmp.path().join("中文-данные.txt");
        fs::write(&file, "hello\n").expect("write");
        commit_all(tmp.path(), "init");

        fs::write(&file, "hello\nworld\n").expect("modify");

        let ctx = ToolContext::new(tmp.path());
        let tool = GitStatusTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);
        assert!(
            result
                .metadata
                .as_ref()
                .and_then(|m| m.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains("-c core.quotepath=false"))
        );
        assert!(result.content.contains("中文-данные.txt"));
        assert!(!result.content.contains("\\344"));
        assert!(!result.content.contains("\\320"));
    }

    #[tokio::test]
    async fn git_diff_supports_cached_and_path_scoping() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let subdir = tmp.path().join("src");
        fs::create_dir_all(&subdir).expect("mkdir");
        let file = subdir.join("lib.rs");
        fs::write(&file, "pub fn one() -> i32 { 1 }\n").expect("write");
        commit_all(tmp.path(), "init");

        fs::write(&file, "pub fn one() -> i32 { 2 }\n").expect("modify");

        let ctx = ToolContext::new(tmp.path());
        let tool = GitDiffTool;

        let uncached = tool
            .execute(json!({ "path": "src" }), &ctx)
            .await
            .expect("diff");
        assert!(uncached.success);
        assert!(uncached.content.contains("diff --git"));
        assert!(uncached.content.contains("lib.rs"));

        let _ = codewhale_dependencies::Git::status(&["add", "src/lib.rs"], tmp.path())
            .expect("git add");

        let cached = tool
            .execute(json!({ "path": "src", "cached": true }), &ctx)
            .await
            .expect("diff cached");
        assert!(cached.success);
        assert!(cached.content.contains("diff --git"));
        assert!(
            cached
                .metadata
                .as_ref()
                .and_then(|m| m.get("cached"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
    }

    #[tokio::test]
    async fn git_diff_reports_unquoted_unicode_paths() {
        if !git_available() {
            return;
        }

        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let unicode_name = "\u{4e2d}\u{6587}-\u{0434}\u{0430}\u{043d}\u{043d}\u{044b}\u{0435}.txt";
        let file = tmp.path().join(unicode_name);
        fs::write(&file, "hello\n").expect("write");
        commit_all(tmp.path(), "init");

        fs::write(&file, "hello\nworld\n").expect("modify");

        let ctx = ToolContext::new(tmp.path());
        let tool = GitDiffTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");

        assert!(result.success);
        assert!(
            result
                .metadata
                .as_ref()
                .and_then(|m| m.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains("-c core.quotepath=false"))
        );
        assert!(result.content.contains(unicode_name));
        assert!(!result.content.contains("\\344"));
        assert!(!result.content.contains("\\320"));
    }

    #[test]
    fn format_command_joins_args_without_intermediate_vec() {
        let args = vec![
            "-c".to_string(),
            "core.quotepath=false".to_string(),
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "-b".to_string(),
        ];
        let rendered = format_command(Path::new("/tmp/repo"), &args);
        assert_eq!(
            rendered,
            "git -C /tmp/repo -c core.quotepath=false status --porcelain=v1 -b"
        );

        assert_eq!(
            format_command(Path::new("/tmp/repo"), &[]),
            "git -C /tmp/repo "
        );
    }

    #[tokio::test]
    async fn git_diff_returns_a_large_diff_whole_with_its_last_file() {
        // #6508: git_diff used to keep the first 40,000 characters, so a big
        // diff silently lost its last files. Size is now the engine's one
        // recoverable budget, not a per-tool cut.
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());
        for name in ["a_first.txt", "z_last.txt"] {
            fs::write(tmp.path().join(name), "base\n").expect("write");
        }
        commit_all(tmp.path(), "init");
        fs::write(tmp.path().join("a_first.txt"), "big line\n".repeat(6_000)).expect("write");
        fs::write(tmp.path().join("z_last.txt"), "LAST FILE CHANGE\n").expect("write");

        let result = GitDiffTool
            .execute(json!({}), &ToolContext::new(tmp.path()))
            .await
            .expect("diff");
        assert!(result.success);
        assert!(result.content.chars().count() > 40_000);
        assert!(result.content.contains("+LAST FILE CHANGE"));
        assert!(!result.content.contains("output truncated"));
    }

    // === Commit plan (#3999) ===

    #[test]
    fn test_parse_diff() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index e69de29..4b2a8d3 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 line1
-line2
+line2 modified
 line3
+line4 added
diff --git a/image.png b/image.png
index 1111111..2222222 100644
Binary files a/image.png and b/image.png differ
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
        let hunks = &files[0].hunks;
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file_path, "src/lib.rs");
        assert_eq!(hunks[0].header, "@@ -1,3 +1,4 @@");
        assert_eq!(hunks[0].lines.len(), 5);
        // A binary change has no hunks but must not vanish from the plan.
        assert_eq!(files[1].path, "image.png");
        assert!(files[1].hunks.is_empty());
    }

    #[test]
    fn test_dependency_extraction() {
        let hunk = Hunk {
            file_path: "src/lib.rs".to_string(),
            header: "@@ -1 +1,2 @@".to_string(),
            lines: vec![
                " pub fn add(a: i32, b: i32) -> i32 {".to_string(),
                "+    let sum = a + b;".to_string(),
                "+    struct Answer;".to_string(),
                "     sum".to_string(),
            ],
        };
        let defined = extract_defined_symbols(&hunk);
        let referenced = extract_referenced_symbols(&hunk);

        assert!(defined.contains("Answer"));
        assert!(defined.contains("sum"));
        assert!(referenced.contains("sum"));
        assert!(referenced.contains("Answer"));
    }

    fn text_file(path: &str, added: &[&str]) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            hunks: vec![Hunk {
                file_path: path.to_string(),
                header: format!("@@ -1 +1,{} @@", added.len()),
                lines: added.iter().map(|line| format!("+{line}")).collect(),
            }],
            untracked: false,
        }
    }

    fn paths(commit: &PlannedCommit) -> Vec<&str> {
        commit.files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn plan_orders_definition_before_use_and_tests_after_source() {
        let commits = plan_commits(vec![
            text_file("src/main.rs", &["fn main() { let y = math::sub(3, 4); }"]),
            text_file(
                "src/math.rs",
                &["pub fn sub(a: i32, b: i32) -> i32 { a - b }"],
            ),
            text_file("src/math_test.rs", &["#[test] fn sub_works() {}"]),
            text_file("docs/notes.md", &["Some notes"]),
        ])
        .expect("acyclic");

        assert_eq!(commits.len(), 3, "{commits:#?}");
        // The test rides with the source file it names.
        assert_eq!(paths(&commits[0]), vec!["src/math.rs", "src/math_test.rs"]);
        assert!(commits[0].depends_on.is_empty());
        assert_eq!(paths(&commits[1]), vec!["src/main.rs"]);
        assert_eq!(commits[1].depends_on, vec![(1, "uses `sub`".to_string())]);
        assert_eq!(paths(&commits[2]), vec!["docs/notes.md"]);
        assert!(
            commits[0].message.starts_with("feat(math): add sub"),
            "{}",
            commits[0].message
        );
        assert_eq!(commits[2].message, "docs(notes): update notes.md");
    }

    #[test]
    fn plan_rejects_cycles_with_a_diagnostic() {
        let cycle = plan_commits(vec![
            text_file("a.rs", &["pub fn func_a2() { b::func_b2(); }"]),
            text_file("b.rs", &["pub fn func_b2() { a::func_a2(); }"]),
        ])
        .expect_err("cycle");
        assert_eq!(cycle.files, vec!["a.rs", "b.rs"]);
        assert!(
            cycle
                .edges
                .iter()
                .any(|edge| edge.contains("uses `func_b2`")),
            "{:?}",
            cycle.edges
        );
    }

    #[test]
    fn lock_file_rides_with_its_manifest_and_stays_out_of_analysis() {
        let mut lock = text_file("Cargo.lock", &["name = \"serde\"", "fn sub() {}"]);
        lock.hunks[0].file_path = "Cargo.lock".to_string();
        let commits = plan_commits(vec![
            text_file("Cargo.toml", &["serde = \"1\""]),
            lock,
            text_file("src/math.rs", &["pub fn sub() {}"]),
        ])
        .expect("acyclic");
        assert_eq!(commits.len(), 2, "{commits:#?}");
        let deps = commits
            .iter()
            .find(|c| paths(c).contains(&"Cargo.lock"))
            .expect("lock group");
        assert_eq!(paths(deps), vec!["Cargo.lock", "Cargo.toml"]);
        assert_eq!(deps.message, "chore(deps): update Cargo.lock, Cargo.toml");
        // The lock file's tokens never create a dependency edge.
        assert!(
            commits.iter().all(|c| c.depends_on.is_empty()),
            "{commits:#?}"
        );
    }

    fn git_out(root: &Path, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let output = run_git_command(root, &args).expect("git");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    #[tokio::test]
    async fn commit_plan_proposes_without_touching_the_index() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let math_file = tmp.path().join("math.rs");
        let main_file = tmp.path().join("main.rs");
        fs::write(&math_file, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").expect("write");
        fs::write(&main_file, "fn main() { let x = math::add(1, 2); }\n").expect("write");
        commit_all(tmp.path(), "init");

        fs::write(
            &math_file,
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn sub(a: i32, b: i32) -> i32 { a - b }\n",
        )
        .expect("modify");
        fs::write(
            &main_file,
            "fn main() { let x = math::add(1, 2); let y = math::sub(3, 4); }\n",
        )
        .expect("modify");
        fs::write(tmp.path().join("NOTES.md"), "untracked notes\n").expect("write");

        let ctx = ToolContext::new(tmp.path());
        let result = GitCommitPlanTool
            .execute(json!({}), &ctx)
            .await
            .expect("execute");
        assert!(result.success, "{}", result.content);
        assert!(
            result.content.contains("propose-only"),
            "{}",
            result.content
        );
        let metadata = result.metadata.expect("metadata");
        let commits = metadata["commits"].as_array().expect("commits");
        assert_eq!(commits.len(), 3, "{}", result.content);
        assert_eq!(commits[0]["files"], json!(["math.rs"]));
        assert_eq!(commits[1]["files"], json!(["main.rs"]));
        assert_eq!(commits[1]["depends_on"][0]["order"], json!(1));
        assert_eq!(commits[2]["untracked"], json!(["NOTES.md"]));
        assert_eq!(metadata["index_has_staged_changes"], json!(false));

        // Nothing was staged, intent-added, or committed.
        assert_eq!(
            git_out(tmp.path(), &["diff", "--cached", "--name-only"]),
            ""
        );
        assert_eq!(
            git_out(tmp.path(), &["rev-list", "--count", "HEAD"]).trim(),
            "1"
        );
        assert_eq!(
            git_out(tmp.path(), &["ls-files", "--others", "--exclude-standard"]).trim(),
            "NOTES.md"
        );

        // The plan lands through the ordinary write path, in order.
        for commit in commits {
            let mut add = vec!["add", "--"];
            let files: Vec<String> = commit["files"]
                .as_array()
                .unwrap()
                .iter()
                .chain(commit["untracked"].as_array().unwrap())
                .map(|f| f.as_str().unwrap().to_string())
                .collect();
            add.extend(files.iter().map(String::as_str));
            git_out(tmp.path(), &add);
            git_out(
                tmp.path(),
                &["commit", "-q", "-m", commit["message"].as_str().unwrap()],
            );
        }
        assert_eq!(
            git_out(tmp.path(), &["rev-list", "--count", "HEAD"]).trim(),
            "4"
        );
        assert_eq!(git_out(tmp.path(), &["status", "--porcelain"]), "");
    }

    #[tokio::test]
    async fn commit_plan_rejects_cycles_and_writes_nothing() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());

        let a_file = tmp.path().join("a.rs");
        let b_file = tmp.path().join("b.rs");
        fs::write(&a_file, "pub fn func_a() {}\n").expect("write a");
        fs::write(&b_file, "pub fn func_b() {}\n").expect("write b");
        commit_all(tmp.path(), "init");

        fs::write(
            &a_file,
            "pub fn func_a() {}\npub fn func_a2() { b::func_b2(); }\n",
        )
        .expect("modify a");
        fs::write(
            &b_file,
            "pub fn func_b() {}\npub fn func_b2() { a::func_a2(); }\n",
        )
        .expect("modify b");

        let ctx = ToolContext::new(tmp.path());
        let result = GitCommitPlanTool
            .execute(json!({}), &ctx)
            .await
            .expect("execute");
        assert!(!result.success);
        assert!(
            result.content.contains("Dependency cycle detected"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("nothing was written"),
            "{}",
            result.content
        );
        assert_eq!(result.metadata.unwrap()["cycle_detected"], json!(true));
        assert_eq!(
            git_out(tmp.path(), &["diff", "--cached", "--name-only"]),
            ""
        );
        assert_eq!(
            git_out(tmp.path(), &["rev-list", "--count", "HEAD"]).trim(),
            "1"
        );
    }

    #[tokio::test]
    async fn commit_plan_warns_when_the_index_already_holds_changes() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());
        let file = tmp.path().join("a.rs");
        fs::write(&file, "pub fn a() {}\n").expect("write");
        commit_all(tmp.path(), "init");
        fs::write(&file, "pub fn a() {}\npub fn a2() {}\n").expect("modify");
        git_out(tmp.path(), &["add", "a.rs"]);

        let ctx = ToolContext::new(tmp.path());
        let result = GitCommitPlanTool
            .execute(json!({}), &ctx)
            .await
            .expect("execute");
        assert!(result.success, "{}", result.content);
        assert!(
            result
                .content
                .contains("WARNING: the index already holds staged changes")
        );
        assert_eq!(
            result.metadata.unwrap()["index_has_staged_changes"],
            json!(true)
        );
        // Still staged exactly as the user left it.
        assert_eq!(
            git_out(tmp.path(), &["diff", "--cached", "--name-only"]).trim(),
            "a.rs"
        );
    }

    #[tokio::test]
    async fn commit_plan_reports_a_clean_tree() {
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("a.rs"), "pub fn a() {}\n").expect("write");
        commit_all(tmp.path(), "init");

        let ctx = ToolContext::new(tmp.path());
        let result = GitCommitPlanTool
            .execute(json!({}), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        assert!(
            result.content.contains("No changes to plan"),
            "{}",
            result.content
        );
    }

    /// Diff and show reads must not run commands a repository configures:
    /// a superproject textconv driver, or — through a child git spawned in a
    /// submodule for its dirty check or `diff.submodule=diff` — a submodule's
    /// clean filter or external diff driver.
    #[cfg(unix)]
    #[tokio::test]
    async fn diff_and_show_reads_run_no_repository_configured_commands() {
        use std::os::unix::fs::PermissionsExt;
        if !git_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let marker = tmp.path().join("marker");
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            fs::write(
                &path,
                format!("#!/bin/sh\necho {name} >> '{}'\n{body}", marker.display()),
            )
            .expect("write script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
            path.display().to_string()
        };
        let clean = script("clean.sh", "cat\n");
        let external = script("external.sh", "");
        let textconv = script("textconv.sh", "cat \"$1\"\n");
        let git = |dir: &Path, args: &[&str]| {
            let status = codewhale_dependencies::Git::status(args, dir).expect("git should spawn");
            assert!(status.success(), "git {args:?} failed");
        };
        let identity = ["-c", "user.email=test@example.com", "-c", "user.name=Test"];

        let upstream = tmp.path().join("upstream");
        fs::create_dir_all(&upstream).expect("mkdir upstream");
        init_git_repo(&upstream);
        fs::write(upstream.join("f.txt"), "a\n").expect("write");
        commit_all(&upstream, "init");

        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).expect("mkdir repo");
        init_git_repo(&repo);
        fs::write(
            repo.join(".gitattributes"),
            "a.md diff=conv\nc.txt filter=x\n",
        )
        .expect("attrs");
        fs::write(repo.join("a.md"), "one\n").expect("write");
        fs::write(repo.join("c.txt"), "c1\n").expect("write");
        let upstream_arg = upstream.display().to_string();
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &upstream_arg,
                "sub",
            ],
        );
        commit_all(&repo, "init");

        let sub = repo.join("sub");
        fs::write(sub.join(".gitattributes"), "* filter=evil\n").expect("sub attrs");
        git(&sub, &[&identity[..], &["add", "."]].concat());
        git(
            &sub,
            &[&identity[..], &["commit", "-q", "-m", "attrs"]].concat(),
        );
        git(&sub, &["config", "filter.evil.clean", &clean]);
        git(&sub, &["config", "diff.external", &external]);
        let edited = sub.join("f.txt");
        fs::write(&edited, "c\n").expect("same-size edit");
        fs::File::options()
            .write(true)
            .open(&edited)
            .and_then(|file| file.set_modified(std::time::UNIX_EPOCH))
            .expect("age the edit so git re-reads it");
        git(&repo, &["config", "diff.submodule", "diff"]);
        git(&repo, &["config", "submodule.sub.ignore", "none"]);
        git(&repo, &["config", "diff.conv.textconv", &textconv]);
        fs::write(repo.join("a.md"), "two\n").expect("modify");

        let no_marker = |step: &str| {
            assert!(
                !marker.exists(),
                "{step} ran a repository-configured command: {}",
                fs::read_to_string(&marker).unwrap_or_default()
            );
        };
        let ctx = ToolContext::new(&repo);
        let diff = GitDiffTool
            .execute(json!({}), &ctx)
            .await
            .expect("git_diff");
        no_marker("git_diff");
        assert!(diff.success, "{}", diff.content);
        assert!(
            diff.content.contains("Subproject commit"),
            "{}",
            diff.content
        );
        assert!(diff.content.contains("+two"), "{}", diff.content);

        git(&repo, &["add", "sub"]);
        git(
            &repo,
            &[&identity[..], &["commit", "-q", "-m", "bump", "--", "sub"]].concat(),
        );
        let show = super::super::git_history::GitShowTool
            .execute(json!({"rev": "HEAD"}), &ctx)
            .await
            .expect("git_show");
        no_marker("git_show");
        assert!(show.success, "{}", show.content);
        assert!(
            show.content.contains("Subproject commit"),
            "{}",
            show.content
        );

        // Working-tree reads also run the superproject's clean filter, which
        // no diff flag disables; blame applies textconv unless told not to.
        git(&repo, &["config", "filter.x.clean", &clean]);
        fs::write(repo.join("c.txt"), "c2\n").expect("modify");
        let diff = GitDiffTool
            .execute(json!({}), &ctx)
            .await
            .expect("git_diff");
        no_marker("git_diff with a clean filter");
        assert!(diff.content.contains("+c2"), "{}", diff.content);
        let plan = GitCommitPlanTool
            .execute(json!({}), &ctx)
            .await
            .expect("git_commit_plan");
        no_marker("git_commit_plan");
        assert!(plan.success, "{}", plan.content);
        let blame = super::super::git_history::GitBlameTool
            .execute(json!({"path": "a.md"}), &ctx)
            .await
            .expect("git_blame");
        no_marker("git_blame");
        assert!(blame.success, "{}", blame.content);
    }

    #[cfg(unix)]
    #[test]
    fn untracked_links_are_listed_by_path_and_never_read() {
        let outside = tempdir().expect("outside");
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "fn outside_marker() {}\n").expect("write");
        let repo = tempdir().expect("repo");
        fs::write(repo.path().join("plain.rs"), "fn inside() {}\n").expect("write");
        std::os::unix::fs::symlink(&secret, repo.path().join("link.rs")).expect("link");
        std::os::unix::fs::symlink(outside.path(), repo.path().join("dir")).expect("link");

        assert!(untracked_hunk(repo.path(), "plain.rs").is_some());
        assert!(untracked_hunk(repo.path(), "link.rs").is_none());
        assert!(untracked_hunk(repo.path(), "dir/secret.txt").is_none());
    }
}

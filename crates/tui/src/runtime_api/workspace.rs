use std::io::Read as _;
use std::path::{Component, Path as FsPath, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dependencies::Git;
use crate::snapshot::is_git_metadata_name;

use super::{ApiError, RuntimeApiState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceFileSearchQuery {
    #[serde(default)]
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceFileSearchResponse {
    paths: Vec<String>,
}

/// Read-only suggestions: never use the process cwd or a caller-selected root.
pub(super) async fn workspace_file_search(
    State(state): State<RuntimeApiState>,
    Query(query): Query<WorkspaceFileSearchQuery>,
) -> Result<Json<WorkspaceFileSearchResponse>, ApiError> {
    if query.query.len() > 256 {
        return Err(ApiError::bad_request(
            "query must be at most 256 UTF-8 bytes",
        ));
    }
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 100"));
    }
    if query.query.trim().is_empty() {
        return Ok(Json(WorkspaceFileSearchResponse { paths: Vec::new() }));
    }
    let paths = tokio::task::spawn_blocking(move || {
        collect_workspace_file_suggestions(&state.workspace, &query.query, limit)
    })
    .await
    .map_err(|_| ApiError::internal("workspace file search failed"))??;
    Ok(Json(WorkspaceFileSearchResponse { paths }))
}

fn collect_workspace_file_suggestions(
    root: &FsPath,
    query: &str,
    limit: usize,
) -> Result<Vec<String>, ApiError> {
    use crate::working_set::{Workspace, rank_completion_candidates};

    let root = root
        .canonicalize()
        .map_err(|_| ApiError::internal("workspace is unavailable"))?;
    let workspace = Workspace::with_cwd(root.clone(), None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    // Reuse the composer's bounded discovery and ignore policy, without its
    // divergent-cwd pass or symlink-directory traversal. No persistent index.
    let candidates = workspace
        .completion_discovery_candidates(20_000, &|| std::time::Instant::now() >= deadline);
    Ok(
        rank_completion_candidates(&candidates, query, candidates.len())
            .into_iter()
            .filter(|candidate| {
                let path = FsPath::new(candidate);
                path.components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
                    && root
                        .join(path)
                        .canonicalize()
                        .is_ok_and(|resolved| resolved.starts_with(&root) && resolved.is_file())
            })
            .take(limit)
            .collect(),
    )
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceStatusResponse {
    pub(super) workspace: PathBuf,
    pub(super) git_repo: bool,
    pub(super) branch: Option<String>,
    pub(super) head: Option<String>,
    pub(super) dirty: bool,
    pub(super) staged: usize,
    pub(super) unstaged: usize,
    pub(super) untracked: usize,
    pub(super) ahead: Option<u32>,
    pub(super) behind: Option<u32>,
}

#[derive(Debug, Default)]
pub(super) struct WorkspaceGitMetadata {
    pub(super) branch: Option<String>,
    pub(super) head: Option<String>,
    pub(super) dirty: bool,
}

/// Status is several blocking `git` spawns, so it runs off the async workers
/// (#6149) like every other git read on this surface. It deliberately keeps
/// normal Git filters and untracked settings rather than the full review
/// command, so its counts match `git status` and the operator writes (see
/// `git.rs`), but it never runs a repository-configured helper: fsmonitor and
/// hooks are off, as on every read-only review path (see `run_git`).
pub(super) async fn workspace_status(
    State(state): State<RuntimeApiState>,
) -> Result<Json<WorkspaceStatusResponse>, ApiError> {
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || collect_workspace_status(&workspace))
        .await
        .map(Json)
        .map_err(|_| ApiError::internal("workspace status failed"))
}

pub(super) fn collect_workspace_status(workspace: &FsPath) -> WorkspaceStatusResponse {
    let mut status = WorkspaceStatusResponse {
        workspace: workspace.to_path_buf(),
        git_repo: false,
        branch: None,
        head: None,
        dirty: false,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        ahead: None,
        behind: None,
    };

    let Some(repo_check) = run_git(workspace, &["rev-parse", "--is-inside-work-tree"]) else {
        return status;
    };
    if repo_check.trim() != "true" {
        return status;
    }

    status.git_repo = true;
    let metadata = collect_workspace_git_metadata(workspace);
    status.branch = metadata.branch;
    status.head = metadata.head;
    status.dirty = metadata.dirty;

    if let Some(porcelain) = run_git(workspace, &["status", "--porcelain=v1"]) {
        for line in porcelain.lines() {
            if line.starts_with("??") {
                status.untracked += 1;
                continue;
            }
            let chars: Vec<char> = line.chars().collect();
            if chars.len() >= 2 {
                if chars[0] != ' ' {
                    status.staged += 1;
                }
                if chars[1] != ' ' {
                    status.unstaged += 1;
                }
            }
        }
    }

    if let Some(counts) = run_git(
        workspace,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    ) {
        let mut parts = counts.split_whitespace();
        if let (Some(behind), Some(ahead)) = (parts.next(), parts.next()) {
            status.behind = behind.parse::<u32>().ok();
            status.ahead = ahead.parse::<u32>().ok();
        }
    }

    status
}

pub(super) fn collect_workspace_git_metadata(workspace: &FsPath) -> WorkspaceGitMetadata {
    let Some(repo_check) = run_git(workspace, &["rev-parse", "--is-inside-work-tree"]) else {
        return WorkspaceGitMetadata::default();
    };
    if repo_check.trim() != "true" {
        return WorkspaceGitMetadata::default();
    }

    WorkspaceGitMetadata {
        branch: current_git_branch(workspace),
        head: current_git_head(workspace),
        dirty: run_git(workspace, &["status", "--porcelain=v1"])
            .is_some_and(|porcelain| !porcelain.trim().is_empty()),
    }
}

/// Read-only git for status and metadata. `Git::review_base` disables
/// `core.fsmonitor`, `core.hooksPath`, lazy fetch and replace objects: a
/// status poll must not execute a helper the repository's config names.
fn run_git(workspace: &FsPath, args: &[&str]) -> Option<String> {
    let mut command = Git::review_base(workspace).ok()?;
    let output = command.args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn current_git_branch(workspace: &FsPath) -> Option<String> {
    let repo_check = run_git(workspace, &["rev-parse", "--is-inside-work-tree"])?;
    if repo_check.trim() != "true" {
        return None;
    }
    let branch = run_git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() {
        return None;
    }
    if branch != "HEAD" {
        return Some(branch.to_string());
    }
    let short_hash = run_git(workspace, &["rev-parse", "--short", "HEAD"])?;
    let short_hash = short_hash.trim();
    (!short_hash.is_empty()).then(|| format!("detached@{short_hash}"))
}

fn current_git_head(workspace: &FsPath) -> Option<String> {
    let head = run_git(workspace, &["rev-parse", "--short", "HEAD"])?;
    let head = head.trim();
    (!head.is_empty()).then(|| head.to_string())
}

// ---------------------------------------------------------------------------
// Workspace files (#6163): a bounded directory listing, bounded reads that
// carry a content revision, and revision-checked writes. The server's
// configured workspace is the only root and every path is workspace-relative
// with `/` separators. Symlinks are never followed, `.git` is never served,
// and there is no second file store: bytes go straight to the workspace
// through the same confined opener Fleet artifacts use.
// ---------------------------------------------------------------------------

const FILE_LIST_LIMIT_DEFAULT: usize = 200;
const FILE_LIST_LIMIT_MAX: usize = 2_000;
pub(super) const FILE_READ_LIMIT_DEFAULT: usize = 256 * 1024;
pub(super) const FILE_READ_LIMIT_MAX: usize = 4 * 1024 * 1024;
/// Files above this size are not served at all: the revision is a digest of
/// the whole file, and a Files browser should not page through larger blobs.
pub(super) const FILE_SERVE_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const FILE_WRITE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Request-body ceiling for the write route: the content cap plus headroom for
/// base64 expansion and the JSON envelope, so an oversized `content` reaches
/// the handler and gets a 413 with a message instead of a dropped connection.
pub(super) const FILE_WRITE_BODY_LIMIT_BYTES: usize = FILE_WRITE_MAX_BYTES * 4 / 3 + 64 * 1024;
const FILE_PATH_MAX_BYTES: usize = 4_096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceFilesListQuery {
    #[serde(default)]
    path: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceFileEntry {
    name: String,
    path: String,
    /// `file`, `directory`, `symlink` (listed by name, never followed) or `other`.
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceFilesListResponse {
    path: String,
    entries: Vec<WorkspaceFileEntry>,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceFileReadQuery {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceFileReadResponse {
    path: String,
    size: u64,
    /// SHA-256 of the whole file, not of the returned window.
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    offset: usize,
    bytes: usize,
    truncated: bool,
    encoding: &'static str,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceFileWriteRequest {
    path: String,
    content: String,
    /// `utf-8` (default) or `base64`.
    #[serde(default)]
    encoding: Option<String>,
    /// Required to overwrite an existing file; must be absent for a new one.
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceFileWriteResponse {
    path: String,
    size: u64,
    revision: String,
    created: bool,
    written_at: String,
}

/// Bytes of one confined file plus the facts a client needs to reason about
/// them: total size, the whole-file revision and the modification time.
pub(super) struct ConfinedFileBytes {
    pub(super) size: u64,
    pub(super) revision: String,
    pub(super) modified: Option<String>,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn parse_read_window(
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<(usize, usize), ApiError> {
    let limit = limit.unwrap_or(FILE_READ_LIMIT_DEFAULT);
    if !(1..=FILE_READ_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {FILE_READ_LIMIT_MAX} bytes"
        )));
    }
    Ok((offset.unwrap_or(0), limit))
}

/// Byte window `[offset, offset + limit)` clamped to the content.
pub(super) fn read_window(bytes: &[u8], offset: usize, limit: usize) -> (&[u8], bool) {
    let start = offset.min(bytes.len());
    let end = start.saturating_add(limit).min(bytes.len());
    (&bytes[start..end], end < bytes.len())
}

/// Text windows come back as UTF-8; anything with a NUL byte, invalid UTF-8,
/// or a window that splits a multi-byte character comes back as base64.
pub(super) fn encode_window(window: &[u8]) -> (&'static str, String) {
    if !window.contains(&0)
        && let Ok(text) = std::str::from_utf8(window)
    {
        return ("utf-8", text.to_string());
    }
    (
        "base64",
        base64::engine::general_purpose::STANDARD.encode(window),
    )
}

pub(super) fn content_revision(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn rfc3339(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

pub(super) fn map_fs_error(error: std::io::Error, what: &str) -> ApiError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => ApiError::not_found(format!("{what} not found")),
        ErrorKind::PermissionDenied => ApiError::forbidden(format!("{what} is not readable")),
        ErrorKind::InvalidInput | ErrorKind::InvalidData => ApiError::forbidden(format!(
            "{what} must be a regular, non-linked path inside the workspace"
        )),
        ErrorKind::Unsupported => {
            ApiError::not_implemented("confined file access is unavailable on this platform")
        }
        _ => ApiError::internal(format!("{what} access failed: {error}")),
    }
}

/// A workspace-relative request path. Empty and `.` mean the root, which only
/// listing accepts. Absolute paths, `..`, backslashes and `.git` are refused.
pub(super) fn relative_request_path(raw: &str, allow_root: bool) -> Result<PathBuf, ApiError> {
    let trimmed = raw.trim();
    if trimmed.len() > FILE_PATH_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "path must be at most {FILE_PATH_MAX_BYTES} UTF-8 bytes"
        )));
    }
    if trimmed.contains('\\') {
        return Err(ApiError::bad_request("path must use / separators"));
    }
    if trimmed.starts_with('/') || FsPath::new(trimmed).is_absolute() {
        return Err(ApiError::bad_request("path must be workspace-relative"));
    }
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return if allow_root {
            Ok(PathBuf::new())
        } else {
            Err(ApiError::bad_request("path is required"))
        };
    }
    let path = PathBuf::from(trimmed);
    if !crate::fleet::files::path_is_confined(&path) {
        return Err(ApiError::bad_request(
            "path must be workspace-relative without . or .. components",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::Normal(name) if is_git_metadata_name(name)))
    {
        return Err(ApiError::forbidden("the .git directory is not served"));
    }
    Ok(path)
}

pub(super) fn canonical_workspace(workspace: &FsPath) -> Result<PathBuf, ApiError> {
    workspace
        .canonicalize()
        .map_err(|_| ApiError::internal("workspace is unavailable"))
}

fn relative_display(path: &FsPath) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Walk `relative` from the root one component at a time, refusing any link
/// or reparse point on the way, and confirm the result is a directory that
/// still resolves inside the root.
fn confined_directory(root: &FsPath, relative: &FsPath) -> Result<PathBuf, ApiError> {
    let mut directory = root.to_path_buf();
    for component in relative.components() {
        directory.push(component);
        let metadata = std::fs::symlink_metadata(&directory)
            .map_err(|error| map_fs_error(error, "directory"))?;
        if metadata.file_type().is_symlink()
            || crate::plugins::metadata_is_link_or_reparse(&metadata)
        {
            return Err(ApiError::forbidden("symlinks are not followed"));
        }
        if !metadata.is_dir() {
            return Err(ApiError::bad_request("path is not a directory"));
        }
    }
    let resolved = directory
        .canonicalize()
        .map_err(|error| map_fs_error(error, "directory"))?;
    if !resolved.starts_with(root) {
        return Err(ApiError::forbidden("path resolves outside the workspace"));
    }
    Ok(directory)
}

pub(super) async fn workspace_files_list(
    State(state): State<RuntimeApiState>,
    Query(query): Query<WorkspaceFilesListQuery>,
) -> Result<Json<WorkspaceFilesListResponse>, ApiError> {
    let relative = relative_request_path(&query.path, true)?;
    let limit = query.limit.unwrap_or(FILE_LIST_LIMIT_DEFAULT);
    if !(1..=FILE_LIST_LIMIT_MAX).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {FILE_LIST_LIMIT_MAX}"
        )));
    }
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || list_workspace_directory(&workspace, &relative, limit))
        .await
        .map_err(|_| ApiError::internal("workspace listing failed"))?
        .map(Json)
}

fn list_workspace_directory(
    workspace: &FsPath,
    relative: &FsPath,
    limit: usize,
) -> Result<WorkspaceFilesListResponse, ApiError> {
    let root = canonical_workspace(workspace)?;
    let directory = confined_directory(&root, relative)?;
    let mut entries = Vec::new();
    let read_dir =
        std::fs::read_dir(&directory).map_err(|error| map_fs_error(error, "directory"))?;
    for entry in read_dir {
        let entry = entry.map_err(|error| map_fs_error(error, "directory"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_git_metadata_name(std::ffi::OsStr::new(&name)) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let metadata = entry.metadata().ok();
        let is_link = file_type.is_symlink()
            || metadata
                .as_ref()
                .is_some_and(crate::plugins::metadata_is_link_or_reparse);
        let kind = if is_link {
            "symlink"
        } else if file_type.is_dir() {
            "directory"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        let file_metadata = (kind == "file").then_some(metadata).flatten();
        entries.push(WorkspaceFileEntry {
            path: relative_display(&relative.join(&name)),
            name,
            kind,
            size: file_metadata.as_ref().map(std::fs::Metadata::len),
            modified: file_metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .map(rfc3339),
        });
    }
    entries.sort_by(|left, right| {
        (left.kind != "directory")
            .cmp(&(right.kind != "directory"))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok(WorkspaceFilesListResponse {
        path: relative_display(relative),
        entries,
        truncated,
    })
}

pub(super) fn open_confined_file(
    root: &FsPath,
    relative: &FsPath,
    create: bool,
) -> Result<crate::fleet::files::WorkspaceFile, ApiError> {
    crate::fleet::files::WorkspaceFile::open(root, relative, create)
        .map_err(|error| map_fs_error(error, "file"))
}

/// [`open_confined_file`] for the application's own state root, which a user
/// may have relocated behind a link (on Windows, a junction to another
/// volume). The root is resolved once and every component below it is still
/// refused when linked, so a reader follows its writer and a caller-supplied
/// workspace root keeps the stricter [`open_confined_file`].
pub(super) fn open_confined_file_resolved(
    root: &FsPath,
    relative: &FsPath,
    create: bool,
) -> Result<crate::fleet::files::WorkspaceFile, ApiError> {
    crate::fleet::files::WorkspaceFile::open_resolved_root(root, relative, create)
        .map_err(|error| map_fs_error(error, "file"))
}

/// Read one confined file completely (bounded by `FILE_SERVE_MAX_BYTES`) so
/// the revision always describes the whole file.
pub(super) fn read_confined_bytes(
    file: &crate::fleet::files::WorkspaceFile,
) -> Result<ConfinedFileBytes, ApiError> {
    let mut handle = file
        .open_file()
        .map_err(|error| map_fs_error(error, "file"))?;
    let metadata = handle
        .metadata()
        .map_err(|error| map_fs_error(error, "file"))?;
    if metadata.len() > FILE_SERVE_MAX_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "file is larger than the {FILE_SERVE_MAX_BYTES}-byte serving limit"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    handle
        .by_ref()
        .take(FILE_SERVE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| map_fs_error(error, "file"))?;
    if bytes.len() as u64 > FILE_SERVE_MAX_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "file grew past the {FILE_SERVE_MAX_BYTES}-byte serving limit while it was read"
        )));
    }
    Ok(ConfinedFileBytes {
        size: bytes.len() as u64,
        revision: content_revision(&bytes),
        modified: metadata.modified().ok().map(rfc3339),
        bytes,
    })
}

/// Refuse links and non-files before the confined opener runs, so a client
/// sees a precise status instead of a generic confinement error.
pub(super) fn precheck_file_target(
    root: &FsPath,
    relative: &FsPath,
) -> Result<Option<std::fs::Metadata>, ApiError> {
    if let Some(parent) = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        // Every directory on the way must be a real directory; a link would
        // otherwise surface as a confinement error from the opener.
        confined_directory(root, parent).or_else(|error| {
            if error.status == StatusCode::NOT_FOUND {
                Ok(PathBuf::new())
            } else {
                Err(error)
            }
        })?;
    }
    match std::fs::symlink_metadata(root.join(relative)) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || crate::plugins::metadata_is_link_or_reparse(&metadata)
            {
                return Err(ApiError::forbidden("symlinks are not followed"));
            }
            if metadata.is_dir() {
                return Err(ApiError::bad_request("path is a directory"));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(map_fs_error(error, "file")),
    }
}

pub(super) async fn workspace_file_read(
    State(state): State<RuntimeApiState>,
    Query(query): Query<WorkspaceFileReadQuery>,
) -> Result<Json<WorkspaceFileReadResponse>, ApiError> {
    let relative = relative_request_path(&query.path, false)?;
    let (offset, limit) = parse_read_window(query.offset, query.limit)?;
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || {
        let root = canonical_workspace(&workspace)?;
        if precheck_file_target(&root, &relative)?.is_none() {
            return Err(ApiError::not_found("file not found"));
        }
        let file = open_confined_file(&root, &relative, false)?;
        let read = read_confined_bytes(&file)?;
        let (window, truncated) = read_window(&read.bytes, offset, limit);
        let (encoding, content) = encode_window(window);
        Ok(WorkspaceFileReadResponse {
            path: relative_display(&relative),
            size: read.size,
            revision: read.revision,
            modified: read.modified,
            offset: offset.min(read.bytes.len()),
            bytes: window.len(),
            truncated,
            encoding,
            content,
        })
    })
    .await
    .map_err(|_| ApiError::internal("workspace file read failed"))?
    .map(Json)
}

pub(super) async fn workspace_file_write(
    State(state): State<RuntimeApiState>,
    Json(request): Json<WorkspaceFileWriteRequest>,
) -> Result<(StatusCode, Json<WorkspaceFileWriteResponse>), ApiError> {
    let relative = relative_request_path(&request.path, false)?;
    let bytes = match request.encoding.as_deref().unwrap_or("utf-8") {
        "utf-8" => request.content.into_bytes(),
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(request.content.as_bytes())
            .map_err(|_| ApiError::bad_request("content is not valid base64"))?,
        _ => return Err(ApiError::bad_request("encoding must be utf-8 or base64")),
    };
    if bytes.len() > FILE_WRITE_MAX_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "content must be at most {FILE_WRITE_MAX_BYTES} bytes"
        )));
    }
    let expected_revision = request
        .expected_revision
        .map(|revision| revision.trim().to_ascii_lowercase())
        .filter(|revision| !revision.is_empty());
    let workspace = state.workspace.clone();
    tokio::task::spawn_blocking(move || {
        write_workspace_file(&workspace, &relative, &bytes, expected_revision.as_deref())
    })
    .await
    .map_err(|_| ApiError::internal("workspace file write failed"))?
}

fn write_workspace_file(
    workspace: &FsPath,
    relative: &FsPath,
    bytes: &[u8],
    expected_revision: Option<&str>,
) -> Result<(StatusCode, Json<WorkspaceFileWriteResponse>), ApiError> {
    static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _write_guard = WRITES
        .lock()
        .map_err(|_| ApiError::internal("workspace file write lock is unavailable"))?;
    let root = canonical_workspace(workspace)?;
    let created = precheck_file_target(&root, relative)?.is_none();
    match (created, expected_revision) {
        (true, Some(_)) => {
            return Err(ApiError::conflict(
                "expected_revision was given but the file does not exist",
            ));
        }
        (false, None) => {
            return Err(ApiError::conflict(
                "expected_revision is required to overwrite an existing file; read it first",
            ));
        }
        _ => {}
    }
    // Parents are created only for a new file, and only through the confined
    // opener, which refuses links at every component. This is the user's own
    // file, not a private store: keep its mode on edit, follow the umask on
    // creation.
    let file = crate::fleet::files::WorkspaceFile::open_shared(&root, relative, created)
        .map_err(|error| map_fs_error(error, "file"))?;
    if let Some(expected) = expected_revision {
        let current = read_confined_bytes(&file)?;
        if current.revision != expected {
            return Err(ApiError::conflict(format!(
                "file changed since it was read; current revision is {}",
                current.revision
            )));
        }
    }
    publish_workspace_file(&file, bytes, created)?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(WorkspaceFileWriteResponse {
            path: relative_display(relative),
            size: bytes.len() as u64,
            revision: content_revision(bytes),
            created,
            written_at: chrono::Utc::now().to_rfc3339(),
        }),
    ))
}

pub(super) fn publish_workspace_file(
    file: &crate::fleet::files::WorkspaceFile,
    bytes: &[u8],
    created: bool,
) -> Result<(), ApiError> {
    // Absence was observed before opening the confined parent. Another
    // creator can publish meanwhile; never replace that creator's file.
    let publication = if created {
        file.publish(bytes)
    } else {
        file.replace(bytes)
    };
    publication.map_err(|error| {
        if created && error.kind() == std::io::ErrorKind::AlreadyExists {
            ApiError::conflict("file was created by another writer; read it before overwriting")
        } else {
            map_fs_error(error, "file")
        }
    })
}

// ── Effective instruction sources (#6168) ────────────────────────────────

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceInstructionSource {
    /// `project` | `rule` | `global` | `fragment` | `configured` |
    /// `constitution` | `ignored`.
    kind: &'static str,
    /// `loaded` | `shadowed` | `skipped` | `missing`.
    status: &'static str,
    scope_dir: PathBuf,
    path: PathBuf,
    /// Workspace-relative spelling when the path sits under the workspace.
    relative_path: Option<String>,
    exists: bool,
    bytes: Option<u64>,
    warning: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceInstructionsResponse {
    workspace: PathBuf,
    sources: Vec<WorkspaceInstructionSource>,
    /// Foreign instruction formats the operator opted into.
    foreign_imports: Vec<String>,
    /// True when no file-based instructions exist anywhere and the
    /// prompt carries the ephemeral generated context instead.
    generated_fallback: bool,
    /// Aggregate byte ceiling applied across instructions and rules at
    /// render (`project_context::MAX_PROJECT_INSTRUCTION_BYTES`).
    aggregate_budget_bytes: usize,
    /// Warnings from the real load pass — includes assembly-level notices
    /// (unimported foreign formats, ignored WHALE.md, constitution parse)
    /// alongside what per-source `warning` fields already carry.
    warnings: Vec<String>,
}

fn instruction_source_kind_name(
    kind: crate::project_context::InstructionSourceKind,
) -> &'static str {
    use crate::project_context::InstructionSourceKind as Kind;
    match kind {
        Kind::Project => "project",
        Kind::Rule => "rule",
        Kind::Global => "global",
        Kind::Fragment => "fragment",
        Kind::Configured => "configured",
        Kind::Constitution => "constitution",
        Kind::Ignored => "ignored",
    }
}

fn instruction_source_status_name(
    status: crate::project_context::InstructionSourceStatus,
) -> &'static str {
    use crate::project_context::InstructionSourceStatus as Status;
    match status {
        Status::Loaded => "loaded",
        Status::Shadowed => "shadowed",
        Status::Skipped => "skipped",
        Status::Missing => "missing",
    }
}

/// Read-only listing of the effective instruction sources for this
/// workspace (#6168): repository-root → workspace chain candidates, rules
/// files, the global fallback layer, opted-in foreign fragments, configured
/// `instructions = [...]` files, and the constitution — each with the status
/// the prompt loaders give it. Edits go through the workspace file routes.
pub(super) async fn workspace_instructions(
    State(state): State<RuntimeApiState>,
) -> Result<Json<WorkspaceInstructionsResponse>, ApiError> {
    let workspace = state.workspace.clone();
    let home = crate::config::effective_home_dir();
    let configured = state.config.read().instructions_paths();

    let (sources, generated_fallback, warnings, workspace_root) =
        tokio::task::spawn_blocking(move || {
            let sources = crate::project_context::project_instruction_sources(
                &workspace,
                home.as_deref(),
                &configured,
            );
            // The real load pass supplies assembly-level warnings and tells us
            // whether the ephemeral generated context is what the prompt
            // carries. Cached — this is the same call the engine makes.
            let ctx = crate::project_context::load_project_context_with_parents(&workspace);
            let generated_fallback = ctx.instructions.is_some() && ctx.source_path.is_none();
            // `project_instruction_sources` canonicalizes the workspace (the
            // `/var` → `/private/var` class of alias), so relative paths must
            // be computed against the canonical spelling or every strip fails.
            // It rides this closure rather than the async body because
            // `canonicalize` is a blocking syscall, and one on a Tokio worker
            // is one too many (#6149).
            let workspace_root =
                std::fs::canonicalize(&workspace).unwrap_or_else(|_| workspace.clone());
            (sources, generated_fallback, ctx.warnings, workspace_root)
        })
        .await
        .map_err(|_| ApiError::internal("instruction source listing failed"))?;
    Ok(Json(WorkspaceInstructionsResponse {
        workspace: workspace_root.clone(),
        sources: sources
            .into_iter()
            .map(|source| WorkspaceInstructionSource {
                kind: instruction_source_kind_name(source.kind),
                status: instruction_source_status_name(source.status),
                // Forward slashes on every platform. `display()` emits the
                // native separator, which would make a wire field describing a
                // repo-relative path differ between a Windows and a Unix host
                // and force every client to branch on the server's OS. The
                // absolute `path` below stays native, because that one is only
                // meaningful on the machine that produced it.
                relative_path: source
                    .path
                    .strip_prefix(&workspace_root)
                    .ok()
                    .map(|relative| {
                        relative
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/")
                    }),
                scope_dir: source.scope_dir,
                path: source.path,
                exists: source.exists,
                bytes: source.bytes,
                warning: source.warning,
            })
            .collect(),
        foreign_imports: crate::project_context::foreign_instruction_imports()
            .keys()
            .into_iter()
            .map(str::to_string)
            .collect(),
        generated_fallback,
        aggregate_budget_bytes: crate::project_context::MAX_PROJECT_INSTRUCTION_BYTES,
        warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::dependencies::ExternalTool as _;

    #[cfg(unix)]
    #[test]
    fn workspace_status_does_not_run_the_repository_fsmonitor() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir()?;
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo)?;
        let git = |args: &[&str]| Git::status(args, &repo).is_ok_and(|status| status.success());
        if !git(&["init", "-q"]) {
            return Ok(());
        }
        std::fs::write(repo.join("file.txt"), "hello\n")?;
        assert!(git(&["add", "file.txt"]));
        assert!(git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "init",
        ]));
        std::fs::write(repo.join("file.txt"), "hello\nworld\n")?;
        let marker = tmp.path().join("ran");
        let hook = tmp.path().join("fsmonitor.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        assert!(git(&["config", "core.fsmonitor", &hook.to_string_lossy()]));

        let status = collect_workspace_status(&repo);
        assert!(status.git_repo);
        assert_eq!(status.unstaged, 1);
        assert!(!marker.exists(), "the repository's fsmonitor hook ran");
        Ok(())
    }

    #[test]
    fn git_metadata_directory_is_refused_in_any_letter_case() {
        for raw in [
            ".git",
            ".git/config",
            ".GIT/config",
            ".Git/hooks/pre-commit",
            "sub/.gIt/HEAD",
        ] {
            let error = relative_request_path(raw, false).expect_err(raw);
            assert_eq!(error.status, StatusCode::FORBIDDEN, "{raw}");
        }
        for raw in [".github/workflows/ci.yml", "a.git/b", ".gitignore", "git"] {
            assert!(relative_request_path(raw, false).is_ok(), "{raw}");
        }
    }

    #[test]
    fn listing_hides_git_metadata_directory_in_any_letter_case() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir_all(tmp.path().join(".Git"))?;
        std::fs::write(tmp.path().join("kept.txt"), "kept")?;
        let listing = list_workspace_directory(tmp.path(), FsPath::new(""), 100)
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let names: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["kept.txt"]);
        Ok(())
    }

    #[test]
    fn workspace_file_search_reuses_discovery_ignores() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join(".agents"))?;
        std::fs::create_dir_all(root.join("target"))?;
        std::fs::write(root.join(".gitignore"), ".agents/\ntarget/\nlocal.rs\n")?;
        std::fs::write(root.join(".ignore"), "blocked.rs\n")?;
        std::fs::write(root.join(".deepseekignore"), "private.rs\n")?;
        for name in [
            "local.rs",
            "blocked.rs",
            "private.rs",
            ".agents/guide.rs",
            "target/build.rs",
        ] {
            std::fs::write(root.join(name), "not returned")?;
        }
        let paths = collect_workspace_file_suggestions(root, ".rs", 100)
            .map_err(|error| anyhow::anyhow!(error.message))?;
        assert_eq!(paths, [".agents/guide.rs", "local.rs"]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn workspace_file_search_contains_symlinks_and_excludes_other_workspaces() -> anyhow::Result<()>
    {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("workspace");
        let other = tmp.path().join("workspace-other");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&other)?;
        std::fs::write(root.join("inside.rs"), "inside")?;
        std::fs::write(other.join("outside.rs"), "outside")?;
        symlink(root.join("inside.rs"), root.join("alias.rs"))?;
        symlink(other.join("outside.rs"), root.join("escape.rs"))?;
        symlink(&other, root.join("external-directory"))?;
        symlink(&other, root.join(".agents"))?;
        symlink(other.join("missing.rs"), root.join("broken.rs"))?;
        symlink(&root, root.join("loop"))?;
        let paths = collect_workspace_file_suggestions(&root, ".rs", 100)
            .map_err(|error| anyhow::anyhow!(error.message))?;
        assert_eq!(paths, ["alias.rs", "inside.rs"]);
        Ok(())
    }
}

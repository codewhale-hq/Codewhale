//! Turn artifact routes (#6653): what a runtime turn produced.
//!
//! The authority is the runtime store's turn record and its items. Nothing
//! is scanned: every reference was recorded where its bytes were written,
//! and the workspace delta comes from the engine's own snapshot pair. Reads
//! go through the same confined openers as the workspace file and session
//! artifact routes; there is no second store.

use std::path::Path as FsPath;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use super::sessions::{ArtifactAuthority, resolve_session_artifact};
use super::workspace::{
    ConfinedFileBytes, FILE_SERVE_MAX_BYTES, canonical_workspace, encode_window,
    open_confined_file, parse_read_window, precheck_file_target, read_confined_bytes, read_window,
    relative_request_path,
};
use super::{ApiError, RuntimeApiState, map_thread_err};
use crate::runtime_threads::{
    CallWorkspaceSpan, FileChangeKind, RuntimeStoreRecordFailure, RuntimeStoreRecordKind,
    TurnArtifactKind, TurnArtifactRef, TurnArtifactsView, TurnItemLifecycleStatus,
    TurnWorkspaceArtifacts,
};

pub(super) async fn list_turn_artifacts(
    State(state): State<RuntimeApiState>,
    Path((thread_id, turn_id)): Path<(String, String)>,
) -> Result<Json<TurnArtifactsView>, ApiError> {
    load_view(&state, &thread_id, &turn_id).await.map(Json)
}

async fn load_view(
    state: &RuntimeApiState,
    thread_id: &str,
    turn_id: &str,
) -> Result<TurnArtifactsView, ApiError> {
    state
        .runtime_threads
        .turn_artifacts(thread_id, turn_id)
        .await
        .map_err(map_thread_err)?
        .ok_or_else(|| ApiError::not_found(format!("turn '{turn_id}' not found in this thread")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TurnArtifactReadQuery {
    offset: Option<usize>,
    limit: Option<usize>,
    /// Read an intermediate revision one of the turn's items recorded,
    /// instead of the reference's final one.
    revision: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TurnArtifactReadResponse {
    artifact: TurnArtifactRef,
    /// `workspace` (the live file), `snapshot` (the turn's post-turn
    /// snapshot), or `session_artifact` (the session artifact directory).
    source: &'static str,
    /// Whether the workspace still holds exactly these bytes. `null` for a
    /// session artifact, or a file reference with no recorded revision.
    current: Option<bool>,
    size: u64,
    /// SHA-256 of the whole content served.
    revision: String,
    offset: usize,
    bytes: usize,
    truncated: bool,
    encoding: &'static str,
    content: String,
}

pub(super) async fn read_turn_artifact(
    State(state): State<RuntimeApiState>,
    Path((thread_id, turn_id, artifact_id)): Path<(String, String, String)>,
    Query(query): Query<TurnArtifactReadQuery>,
) -> Result<Json<TurnArtifactReadResponse>, ApiError> {
    let (offset, limit) = parse_read_window(query.offset, query.limit)?;
    let view = load_view(&state, &thread_id, &turn_id).await?;
    let artifact = select_reference(&view, &artifact_id, query.revision.as_deref())?;
    let workspace = view.workspace.clone();
    let thread_workspace = view.thread_workspace.clone();
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let (source, current, read) = match artifact.kind {
            TurnArtifactKind::File => {
                read_file_artifact(&thread_workspace, workspace.as_ref(), &artifact)?
            }
            TurnArtifactKind::ToolOutput | TurnArtifactKind::Media => {
                let session_id = artifact
                    .session_id
                    .as_deref()
                    .ok_or_else(|| ApiError::internal("artifact reference has no session"))?;
                let root = crate::artifacts::artifact_sessions_root()
                    .ok_or_else(|| ApiError::internal("session artifact root is unavailable"))?;
                let resolved = resolve_session_artifact(
                    &root,
                    session_id,
                    &artifact.id,
                    ArtifactAuthority::TurnRef {
                        path: &artifact.path,
                        revision: artifact.revision.as_deref(),
                    },
                )?;
                ("session_artifact", None, resolved.read)
            }
        };
        let (window, truncated) = read_window(&read.bytes, offset, limit);
        let (encoding, content) = encode_window(window);
        Ok(TurnArtifactReadResponse {
            artifact,
            source,
            current,
            size: read.size,
            revision: read.revision,
            offset: offset.min(read.bytes.len()),
            bytes: window.len(),
            truncated,
            encoding,
            content,
        })
    })
    .await
    .map_err(|_| ApiError::internal("turn artifact read failed"))?
    .map(Json)
}

/// The reference to serve: the turn aggregate's, or an item's when the turn
/// aggregate no longer lists it (a file written and then deleted still has
/// item-level history). `?revision=` selects the item ref that recorded
/// exactly that revision.
fn select_reference(
    view: &TurnArtifactsView,
    artifact_id: &str,
    revision: Option<&str>,
) -> Result<TurnArtifactRef, ApiError> {
    let aggregate = view.artifacts.iter().find(|r| r.id == artifact_id);
    let items = view
        .item_artifacts
        .iter()
        .rev()
        .filter(|r| r.id == artifact_id);
    let Some(wanted) = revision.map(|revision| revision.trim().to_ascii_lowercase()) else {
        return aggregate
            .or_else(|| {
                view.item_artifacts
                    .iter()
                    .rev()
                    .find(|r| r.id == artifact_id)
            })
            .cloned()
            .ok_or_else(|| ApiError::not_found("artifact not found in this turn"));
    };
    aggregate
        .into_iter()
        .chain(items)
        .find(|r| r.revision.as_deref() == Some(wanted.as_str()))
        .cloned()
        .ok_or_else(|| ApiError::not_found("this turn recorded no such revision of the artifact"))
}

/// Serve a file reference: from the workspace when it still holds the
/// recorded revision, otherwise from the turn's post-turn snapshot,
/// otherwise a conflict naming what the workspace holds now.
fn read_file_artifact(
    thread_workspace: &FsPath,
    workspace: Option<&TurnWorkspaceArtifacts>,
    artifact: &TurnArtifactRef,
) -> Result<(&'static str, Option<bool>, ConfinedFileBytes), ApiError> {
    if artifact.change == Some(FileChangeKind::Deleted) {
        return Err(ApiError::gone(
            "this turn deleted the file; restore its prior content with file-revert and the reference's restore_snapshot_id",
        ));
    }
    if artifact
        .size
        .is_some_and(|size| size > FILE_SERVE_MAX_BYTES)
    {
        return Err(ApiError::payload_too_large(format!(
            "file is larger than the {FILE_SERVE_MAX_BYTES}-byte serving limit"
        )));
    }
    let relative = relative_request_path(&artifact.path, false)?;
    let root = canonical_workspace(thread_workspace)?;
    let live = match precheck_file_target(&root, &relative)? {
        Some(_) => Some(read_confined_bytes(&open_confined_file(
            &root, &relative, false,
        )?)?),
        None => None,
    };
    let Some(wanted) = artifact.revision.as_deref() else {
        // No recorded revision (an older receipt): all that can be served
        // is the live file, and whether it is the turn's version is unknown.
        return live
            .map(|read| ("workspace", None, read))
            .ok_or_else(|| ApiError::gone("the file is no longer in the workspace"));
    };
    if let Some(read) = live.as_ref()
        && read.revision == wanted
    {
        return Ok(("workspace", Some(true), live.expect("checked above")));
    }
    if let Some(post) = workspace.and_then(|w| w.post_turn_snapshot_id.as_deref())
        && let Some(bytes) = read_snapshot_blob(&root, post, &artifact.path)?
    {
        let read = ConfinedFileBytes {
            size: bytes.len() as u64,
            revision: super::workspace::content_revision(&bytes),
            modified: None,
            bytes,
        };
        if read.revision == wanted {
            return Ok(("snapshot", Some(false), read));
        }
    }
    Err(ApiError::conflict(match live {
        Some(read) => format!(
            "this turn's revision is no longer in the workspace or the snapshot store; current revision is {}",
            read.revision
        ),
        None => "this turn's revision is no longer in the workspace or the snapshot store; the file is gone".to_string(),
    }))
}

fn read_snapshot_blob(
    workspace: &FsPath,
    snapshot_id: &str,
    path: &str,
) -> Result<Option<Vec<u8>>, ApiError> {
    let Ok(id) = codewhale_snapshot::SnapshotId::parse(snapshot_id) else {
        return Ok(None);
    };
    let Some(repo) = codewhale_snapshot::SnapshotRepo::open_existing(workspace)
        .map_err(|error| ApiError::internal(format!("snapshot repo unavailable: {error}")))?
    else {
        return Ok(None);
    };
    match repo.read_blob(&id, path, FILE_SERVE_MAX_BYTES) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
            Err(ApiError::payload_too_large(format!(
                "file is larger than the {FILE_SERVE_MAX_BYTES}-byte serving limit"
            )))
        }
        // A pruned snapshot is not an error: the revision is just gone.
        Err(error) => {
            tracing::debug!(%error, "snapshot blob read failed");
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// One tool call's changes
// ---------------------------------------------------------------------------

/// Most files one call's change list holds. The list is cut here, never
/// silently; `truncated` says it was.
const DEFAULT_CALL_CHANGE_FILES: usize = 200;
/// Largest list a caller may ask for.
const MAX_CALL_CHANGE_FILES: usize = 1_000;
/// Largest patch served for one path. A longer one is cut at a char boundary
/// and flagged.
const MAX_CALL_CHANGE_PATCH_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CallChangesQuery {
    limit: Option<usize>,
}

/// How one path changed, in the vocabulary the turn artifact contract uses.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CallChangeKind {
    Created,
    Updated,
    Deleted,
}

impl CallChangeKind {
    /// git's own status letter. The diff runs with `--no-renames`, so a move
    /// reads as a delete plus a create, and a type change (`T`) is an update.
    fn from_status(status: char) -> Self {
        match status {
            'A' => Self::Created,
            'D' => Self::Deleted,
            _ => Self::Updated,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct CallChangeFile {
    /// Workspace-relative with `/` separators, as git names it.
    path: String,
    change: CallChangeKind,
    /// Lines added and removed; `null` for a binary file.
    added: Option<u64>,
    removed: Option<u64>,
    /// Byte size and SHA-256 hex of the revision the span left, so a client
    /// can send `expected_hash` to `file-revert`. Both `null` when the path
    /// was deleted here, or is too large to read.
    size: Option<u64>,
    revision: Option<String>,
    /// The restore point `POST /v1/threads/{id}/file-revert` accepts for this
    /// path: the call's `tool:` receipt, which this thread owns.
    restore_snapshot_id: Option<String>,
    /// The patch between the span's two restore points. `null` when there is
    /// nothing to render: a binary path, a change with no content delta, or a
    /// patch git could not write.
    diff: Option<String>,
    /// Whether `diff` was cut at the serving bound.
    diff_truncated: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct CallChangesResponse {
    thread_id: String,
    turn_id: String,
    tool_call_id: String,
    tool_name: Option<String>,
    /// `captured` when the call's two restore points exist and `files` is
    /// authoritative; `pending` while its recorded item is active and the
    /// pair is incomplete; `unavailable` after settlement, with `reason`.
    state: &'static str,
    reason: Option<&'static str>,
    files: Vec<CallChangeFile>,
    /// Whether `files` was cut at the request's `limit`. How many paths are
    /// left off is not counted.
    truncated: bool,
}

/// What one tool call changed, from the workspace restore points the engine
/// recorded around it.
///
/// The per-call counterpart of the turn's aggregated artifacts: the engine
/// brackets every call that may write with a `tool:<call_id>` and a
/// `post-tool:<call_id>` receipt, so a file a shell command wrote is
/// attributable to that command and not only to the turn that ran it. Both
/// trees are read from the side repo; the work tree is never touched, so the
/// answer is what the call did rather than what the file holds now.
///
/// The span is a time window, not attribution by cause: anything else that
/// wrote the same workspace between the two receipts is in it too, exactly as
/// the turn delta says. Snapshots exclude what they exclude (`node_modules/`,
/// `.gitignore` entries, binary and media extensions), and a path they never
/// track cannot appear here. A call the engine judged read-only has no
/// receipts at all and answers `call_not_bounded` after settlement. An active
/// item without a complete pair answers `pending`; a settled call whose closing
/// snapshot was lost answers `post_snapshot_missing`; one whose receipts name
/// trees the store no longer holds (pruned, or a store that is gone) answers
/// `snapshots_pruned`. All three are `unavailable` rather than an empty list,
/// which would read as "this call changed nothing".
pub(super) async fn list_call_changes(
    State(state): State<RuntimeApiState>,
    Path((thread_id, turn_id, tool_call_id)): Path<(String, String, String)>,
    Query(query): Query<CallChangesQuery>,
) -> Result<Json<CallChangesResponse>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_CALL_CHANGE_FILES);
    if !(1..=MAX_CALL_CHANGE_FILES).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_CALL_CHANGE_FILES}"
        )));
    }
    let span = state
        .runtime_threads
        .turn_call_span(&thread_id, &turn_id, &tool_call_id)
        .await
        .map_err(|error| {
            if RuntimeStoreRecordFailure::from_error(&error)
                .is_some_and(|failure| failure.record_kind == RuntimeStoreRecordKind::Item)
            {
                ApiError::internal(error.to_string())
            } else {
                map_thread_err(error)
            }
        })?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "no call '{tool_call_id}' is recorded on turn '{turn_id}' of this thread"
            ))
        })?;

    let (pre, post) = match (
        span.pre_tool_snapshot_id.clone(),
        span.post_tool_snapshot_id.clone(),
    ) {
        (Some(pre), Some(post)) => (pre, post),
        (pre, post) => {
            if matches!(
                span.item_status,
                Some(TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress)
            ) {
                return Ok(Json(empty_call_changes(&span, "pending", None)));
            }
            let reason = match (pre.is_some(), post.is_some()) {
                (true, false) => "post_snapshot_missing",
                (false, true) => "pre_snapshot_missing",
                _ => "call_not_bounded",
            };
            return Ok(Json(empty_call_changes(&span, "unavailable", Some(reason))));
        }
    };

    let workspace = span.thread_workspace.clone();
    let restore_snapshot_id = Some(pre.clone());
    // The side repo lives under the sealed test home; a blocking-pool thread
    // is a foreign reader until it joins the test's env scope, and would
    // otherwise resolve the isolated root and report every span pruned.
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    let read = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        call_span_files(
            &workspace,
            &pre,
            &post,
            restore_snapshot_id.as_deref(),
            limit,
        )
    })
    .await
    .map_err(|_| ApiError::internal("call change read task failed"))??;

    let read = match read {
        CallSpanOutcome::Captured(read) => read,
        // The receipt survived; the objects it names did not. Pruning is the
        // store's own housekeeping, so this is a fact about the span, not an
        // error a client should report as a failure — the paths the receipt
        // recorded are still on the turn, and the caller keeps them.
        CallSpanOutcome::Pruned => {
            return Ok(Json(empty_call_changes(
                &span,
                "unavailable",
                Some("snapshots_pruned"),
            )));
        }
    };

    Ok(Json(CallChangesResponse {
        thread_id: span.thread_id,
        turn_id: span.turn_id,
        tool_call_id: span.tool_call_id,
        tool_name: span.tool_name,
        state: "captured",
        reason: None,
        files: read.files,
        truncated: read.truncated,
    }))
}

fn empty_call_changes(
    span: &CallWorkspaceSpan,
    state: &'static str,
    reason: Option<&'static str>,
) -> CallChangesResponse {
    CallChangesResponse {
        thread_id: span.thread_id.clone(),
        turn_id: span.turn_id.clone(),
        tool_call_id: span.tool_call_id.clone(),
        tool_name: span.tool_name.clone(),
        state,
        reason,
        files: Vec::new(),
        truncated: false,
    }
}

struct CallSpanFiles {
    files: Vec<CallChangeFile>,
    truncated: bool,
}

/// What reading one call's span produced.
enum CallSpanOutcome {
    Captured(CallSpanFiles),
    /// A tree the receipts name is no longer in the side repo — or there is
    /// no side repo left at all. The store keeps only its newest snapshots
    /// while the turn record that names them is durable, so an old turn's span
    /// is regularly unrecoverable: a thing to report, not a failure to raise.
    Pruned,
}

/// Turn one call's two restore points into what it changed: git's own status
/// and line counts for the pair, each surviving path's revision read from the
/// span's *end*, and the patch between the two trees.
fn call_span_files(
    workspace: &FsPath,
    pre_tree: &str,
    post_tree: &str,
    restore_snapshot_id: Option<&str>,
    limit: usize,
) -> Result<CallSpanOutcome, ApiError> {
    let (Ok(pre), Ok(post)) = (
        codewhale_snapshot::SnapshotId::parse(pre_tree),
        codewhale_snapshot::SnapshotId::parse(post_tree),
    ) else {
        return Err(ApiError::internal(
            "a recorded restore point is not a valid snapshot id",
        ));
    };
    let Some(repo) = codewhale_snapshot::SnapshotRepo::open_existing(workspace)
        .map_err(|error| ApiError::internal(format!("snapshot repo unavailable: {error}")))?
    else {
        // No store at all. Its receipts cannot be resolved any more than a
        // pruned tree can, and reading a missing store is "absent" everywhere
        // else in this module (`read_snapshot_blob`) — so this is the same
        // answer, not a server failure a client could act on.
        return Ok(CallSpanOutcome::Pruned);
    };
    // Asked before the diff: git's answer to a pruned object is a failure of
    // the whole command, and a caller has to tell "these were pruned" apart
    // from "this repo is broken".
    let has_tree = |id: &codewhale_snapshot::SnapshotId| {
        repo.has_tree(id)
            .map_err(|error| ApiError::internal(format!("snapshot tree lookup failed: {error}")))
    };
    if !has_tree(&pre)? || !has_tree(&post)? {
        return Ok(CallSpanOutcome::Pruned);
    }
    let (changes, truncated) = repo
        .path_changes_between(&pre, &post, limit)
        .map_err(|error| ApiError::internal(format!("snapshot diff failed: {error}")))?;
    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        // A binary path has no line counts, and git's "Binary files differ"
        // notice is not a patch a client can render: serve neither.
        let binary = change.added.is_none() && change.removed.is_none();
        // The revision is the span's own end, never the work tree: the file
        // may have moved on since, and the digest a revert is checked against
        // must be the one this change produced.
        let (size, revision) = if change.status == 'D' {
            (None, None)
        } else {
            match repo.read_blob(&post, &change.path, FILE_SERVE_MAX_BYTES) {
                Ok(Some(bytes)) => (
                    Some(bytes.len() as u64),
                    Some(codewhale_runtime::hashing::sha256_hex(&bytes)),
                ),
                Ok(None) => (None, None),
                Err(error) => {
                    tracing::debug!(path = %change.path, %error, "snapshot blob read failed");
                    (None, None)
                }
            }
        };
        let (diff, diff_truncated) = if binary {
            (None, false)
        } else {
            match repo.patch_between(&pre, &post, &change.path, MAX_CALL_CHANGE_PATCH_BYTES) {
                Ok((text, cut)) if !text.is_empty() => (Some(text), cut),
                Ok(_) => (None, false),
                Err(error) => {
                    tracing::debug!(path = %change.path, %error, "snapshot patch failed");
                    (None, false)
                }
            }
        };
        files.push(CallChangeFile {
            path: change.path,
            change: CallChangeKind::from_status(change.status),
            added: change.added,
            removed: change.removed,
            size,
            revision,
            restore_snapshot_id: restore_snapshot_id.map(str::to_owned),
            diff,
            diff_truncated,
        });
    }
    Ok(CallSpanOutcome::Captured(CallSpanFiles {
        files,
        truncated,
    }))
}

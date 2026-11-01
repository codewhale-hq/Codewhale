//! Typed artifact references on runtime turns and items.
//!
//! A turn's artifacts are what it left behind that a client can open: files
//! a tool created or changed in the workspace, tool output that spilled to
//! the session artifact directory, and media a tool produced. Each is named
//! by a [`TurnArtifactRef`] carrying path, kind, size and revision, so a
//! Preview surface can show "what this turn produced" without scanning.
//!
//! Facts come from where the bytes were written, never from re-reading the
//! disk later: file tools and `apply_patch` record `size`/`sha256` in their
//! `mutation.files[]` receipt, spills record `artifact_digest`, and media
//! publication records `sha256`. This module only parses those receipts and
//! confines every path; there is no second artifact store.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hard ceiling on refs parsed from one tool result. A patch touching more
/// files than this is still applied; the extra refs are simply not listed.
pub const MAX_ITEM_ARTIFACTS: usize = 1_000;

/// What kind of thing the reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnArtifactKind {
    /// A file in the thread workspace; `path` is workspace-relative.
    File,
    /// Tool output spilled to the session artifact directory; `path` is
    /// session-relative (`artifacts/art_<call>.txt`).
    ToolOutput,
    /// Media a tool returned, published immutably under the session.
    Media,
}

/// How a file changed. Only set on `kind = file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Created,
    Updated,
    Deleted,
    Renamed,
}

/// Which receipt produced the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnArtifactSource {
    /// A file tool's `mutation` receipt (write/edit/apply_patch).
    ToolMutation,
    /// A large tool result spilled to a session artifact.
    ToolOutputSpill,
    /// A tool's media publication.
    ToolMedia,
    /// The workspace snapshot delta between the turn's pre-turn and
    /// post-turn snapshots. This is everything that changed in the workspace
    /// while the turn ran, which includes writes by shell commands and
    /// sub-agents but also by anything else writing the same workspace at the
    /// same time (an editor, another thread, a background job).
    WorkspaceChangedDuringTurn,
}

/// One thing a turn (or one tool call inside it) produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnArtifactRef {
    /// URL-safe id, stable within the turn: `file_<32 hex>` for a file (a
    /// digest of its path), the spill's `art_<call>` id, or the media
    /// `art_image_<sha256>` handle.
    pub id: String,
    pub kind: TurnArtifactKind,
    /// `kind = file`: workspace-relative with `/` separators.
    /// `tool_output`/`media`: session-relative (`artifacts/...`).
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<FileChangeKind>,
    /// The old path of a renamed file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    /// Byte size of the content this reference names. `None` when deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// SHA-256 (lowercase hex) of the whole content: the same value the
    /// workspace file read reports as `revision`. `None` when deleted or too
    /// large to hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Exact media type, for `kind = media`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Owning artifact session, for `tool_output`/`media`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub source: TurnArtifactSource,
    /// A restore point `POST /v1/threads/{id}/file-revert` accepts for this
    /// file on this thread: the tree id of a `tool` or `pre_turn` snapshot
    /// recorded on this turn (`TurnRecord::workspace_snapshots`), so the
    /// thread owns it (#6621). Absent when the turn recorded none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_snapshot_id: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// Identity of the tool call a set of refs is parsed for.
pub struct ToolArtifactContext<'a> {
    pub item_id: &'a str,
    pub tool_call_id: &'a str,
    pub tool_name: &'a str,
    /// The thread workspace every file path is confined to: as configured,
    /// and canonicalized when that differs (tools resolve against the
    /// configured path, which may traverse a symlink such as macOS `/var`).
    pub workspace_roots: &'a [PathBuf],
    /// The `tool` restore point recorded on this turn for this call (its
    /// tree id), when there is one. Only a receipt the thread owns is
    /// advertised.
    pub restore_snapshot_id: Option<&'a str>,
    pub recorded_at: DateTime<Utc>,
}

/// The stable ref id for a workspace file path.
pub fn file_artifact_id(path: &str) -> String {
    let digest = codewhale_runtime::hashing::sha256_hex(path.as_bytes());
    format!("file_{}", &digest[..32])
}

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Normalize a tool-supplied path into a workspace-relative display path.
///
/// Accepts a relative path or an absolute one inside any of the workspace's
/// root spellings. Refuses `..`, empty, non-UTF-8, anything outside, and
/// `.git` (never served).
pub fn confined_workspace_path(workspace_roots: &[PathBuf], raw: &str) -> Option<String> {
    let rel = workspace_roots
        .iter()
        .find_map(|root| codewhale_snapshot::workspace_relative_path(root, raw))?;
    relative_display(&rel)
}

fn relative_display(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in rel.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        if codewhale_util::git_metadata::is_git_metadata_name(name) {
            return None;
        }
        parts.push(name.to_str()?);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A session-relative artifact path: relative, `/`-separated, under
/// `artifacts/`, with no `.`/`..` components.
pub fn confined_session_artifact_path(raw: &str) -> Option<String> {
    if raw.contains('\\') {
        return None;
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return None;
    }
    let display = relative_display(&path)?;
    display
        .split('/')
        .next()
        .is_some_and(|first| first == codewhale_runtime::artifacts::ARTIFACTS_DIR_NAME)
        .then_some(display)
}

fn is_safe_artifact_id(id: &str) -> bool {
    id.starts_with("art_")
        && id.len() <= 200
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn change_kind(outcome: &str) -> Option<FileChangeKind> {
    match outcome {
        "created" => Some(FileChangeKind::Created),
        "updated" => Some(FileChangeKind::Updated),
        "deleted" => Some(FileChangeKind::Deleted),
        _ => None,
    }
}

fn written_facts(entry: &Value) -> (Option<u64>, Option<String>) {
    let size = entry.get("size").and_then(Value::as_u64);
    let revision = entry
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|digest| is_sha256_hex(digest))
        .map(str::to_owned);
    (size, revision)
}

/// Parse the artifact references a tool result's metadata describes.
///
/// Three receipts are read: `mutation.files[]`/`mutation.renames[]` (file
/// tools), the spill keys `artifact_id`/`artifact_session_id`/
/// `artifact_relative_path`/`artifact_byte_size`/`artifact_digest`, and
/// `tool_media[]`. Anything that is not confined — an absolute or `..`
/// path, a path outside the workspace, an invalid session id — yields no
/// reference rather than a reference a client could be misled by.
pub fn artifact_refs_from_tool_metadata(
    metadata: &Value,
    context: &ToolArtifactContext<'_>,
) -> Vec<TurnArtifactRef> {
    let mut refs = Vec::new();
    let base = |id: String, kind, path: String, source| TurnArtifactRef {
        id,
        kind,
        path,
        change: None,
        previous_path: None,
        size: None,
        revision: None,
        content_type: None,
        session_id: None,
        item_id: Some(context.item_id.to_owned()),
        tool_call_id: Some(context.tool_call_id.to_owned()),
        tool_name: Some(context.tool_name.to_owned()),
        source,
        restore_snapshot_id: None,
        recorded_at: context.recorded_at,
    };

    // The call's own `tool` restore point, from the receipt recorded on this
    // turn — never from the tool's result metadata, which a tool controls.
    let restore_snapshot_id = context
        .restore_snapshot_id
        .filter(|id| codewhale_snapshot::SnapshotId::is_well_formed(id))
        .map(str::to_owned);

    if let Some(mutation) = metadata.get("mutation") {
        for entry in mutation
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(change) = entry
                .get("outcome")
                .and_then(Value::as_str)
                .and_then(change_kind)
            else {
                continue;
            };
            let Some(path) = entry
                .get("path")
                .and_then(Value::as_str)
                .and_then(|raw| confined_workspace_path(context.workspace_roots, raw))
            else {
                continue;
            };
            let mut reference = base(
                file_artifact_id(&path),
                TurnArtifactKind::File,
                path,
                TurnArtifactSource::ToolMutation,
            );
            reference.change = Some(change);
            if change != FileChangeKind::Deleted {
                (reference.size, reference.revision) = written_facts(entry);
            }
            reference.restore_snapshot_id = restore_snapshot_id.clone();
            refs.push(reference);
        }
        for entry in mutation
            .get("renames")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let confine = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .and_then(|raw| confined_workspace_path(context.workspace_roots, raw))
            };
            let (Some(from), Some(to)) = (confine("from"), confine("to")) else {
                continue;
            };
            let mut reference = base(
                file_artifact_id(&to),
                TurnArtifactKind::File,
                to,
                TurnArtifactSource::ToolMutation,
            );
            reference.change = Some(FileChangeKind::Renamed);
            reference.previous_path = Some(from);
            (reference.size, reference.revision) = written_facts(entry);
            reference.restore_snapshot_id = restore_snapshot_id.clone();
            refs.push(reference);
        }
    }

    if let (Some(artifact_id), Some(session_id), Some(path)) = (
        metadata
            .get("artifact_id")
            .and_then(Value::as_str)
            .filter(|id| is_safe_artifact_id(id)),
        metadata
            .get("artifact_session_id")
            .and_then(Value::as_str)
            .filter(|id| codewhale_runtime::artifacts::is_valid_session_id(id)),
        metadata
            .get("artifact_relative_path")
            .and_then(Value::as_str)
            .and_then(confined_session_artifact_path),
    ) {
        let mut reference = base(
            artifact_id.to_owned(),
            TurnArtifactKind::ToolOutput,
            path,
            TurnArtifactSource::ToolOutputSpill,
        );
        reference.session_id = Some(session_id.to_owned());
        reference.size = metadata.get("artifact_byte_size").and_then(Value::as_u64);
        reference.revision = metadata
            .get("artifact_digest")
            .and_then(Value::as_str)
            .filter(|digest| is_sha256_hex(digest))
            .map(str::to_owned);
        refs.push(reference);
    }

    for media in metadata
        .get("tool_media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(handle) = media
            .get("artifact_id")
            .and_then(Value::as_str)
            .filter(|id| id.strip_prefix("art_image_").is_some_and(is_sha256_hex))
        else {
            continue;
        };
        let Some(session_id) = media
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| codewhale_runtime::artifacts::is_valid_session_id(id))
        else {
            continue;
        };
        let mut reference = base(
            handle.to_owned(),
            TurnArtifactKind::Media,
            format!(
                "{}/{handle}.image",
                codewhale_runtime::artifacts::ARTIFACTS_DIR_NAME
            ),
            TurnArtifactSource::ToolMedia,
        );
        reference.session_id = Some(session_id.to_owned());
        reference.size = media.get("byte_size").and_then(Value::as_u64);
        reference.revision = media
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|digest| is_sha256_hex(digest))
            .map(str::to_owned);
        reference.content_type = media
            .get("media_type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        refs.push(reference);
    }

    refs.truncate(MAX_ITEM_ARTIFACTS);
    refs
}

/// The legacy `artifact_refs: Vec<PathBuf>` projection.
///
/// Clients pinned to the older contract read these as workspace-relative
/// file paths (the desktop Preview does), so only existing workspace files
/// belong here: never a spill or media path, never a deleted file.
pub fn legacy_artifact_refs(refs: &[TurnArtifactRef]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for reference in refs {
        if reference.kind != TurnArtifactKind::File
            || reference.change == Some(FileChangeKind::Deleted)
        {
            continue;
        }
        let path = PathBuf::from(&reference.path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// Ceiling on a turn's aggregate. The cut is reported, never silent.
pub const MAX_TURN_ARTIFACTS: usize = 1_000;

/// Where the turn's workspace-level accounting stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnWorkspaceState {
    /// The turn ended; the post-turn snapshot or its diff is still running.
    /// `artifacts` holds the item-derived refs until `turn.artifacts` lands.
    Pending,
    /// The pre/post snapshot delta is merged into `artifacts`.
    Settled,
    /// No delta will come; `reason` says why. `artifacts` holds what the
    /// tool receipts recorded.
    Unavailable,
}

/// Why a turn has no workspace delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnWorkspaceReason {
    SnapshotsDisabled,
    WorkspaceTooLarge,
    TooManyFiles,
    UnsafeLocation,
    SnapshotFailed,
    /// The turn ran no snapshot pair: a compaction or purge operation, or a
    /// turn that ended before the engine reported one.
    NotCaptured,
    /// The Runtime restarted before the delta settled.
    RuntimeRestarted,
    /// The snapshots exist but diffing them failed.
    DeltaFailed,
}

/// The workspace half of a turn's artifact accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnWorkspaceArtifacts {
    pub state: TurnWorkspaceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TurnWorkspaceReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_turn_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_turn_snapshot_id: Option<String>,
    /// The aggregate was cut at `MAX_TURN_ARTIFACTS` (or the delta at its
    /// own bound); `omitted` counts what is not listed.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub omitted: u64,
}

impl TurnWorkspaceArtifacts {
    pub fn unavailable(reason: TurnWorkspaceReason) -> Self {
        Self {
            state: TurnWorkspaceState::Unavailable,
            reason: Some(reason),
            pre_turn_snapshot_id: None,
            post_turn_snapshot_id: None,
            truncated: false,
            omitted: 0,
        }
    }

    pub fn pending(pre_turn_snapshot_id: String) -> Self {
        Self {
            state: TurnWorkspaceState::Pending,
            reason: None,
            pre_turn_snapshot_id: Some(pre_turn_snapshot_id),
            post_turn_snapshot_id: None,
            truncated: false,
            omitted: 0,
        }
    }
}

/// A settled workspace delta, ready to merge.
pub struct WorkspaceDelta {
    pub refs: Vec<TurnArtifactRef>,
    /// Item-reported paths that either snapshot contains. An item path in
    /// neither snapshot is invisible to them (excluded or ignored), so the
    /// delta's silence about it proves nothing and its item ref is kept.
    pub tracked_item_paths: std::collections::HashSet<String>,
    pub truncated: bool,
    pub omitted: u64,
}

/// Convert a snapshot delta into turn refs. `restore_snapshot_id` is the
/// pre-turn restore point recorded on the turn, which file-revert accepts
/// for this thread.
pub fn delta_refs(
    delta: &codewhale_snapshot::SnapshotDelta,
    restore_snapshot_id: Option<&str>,
    recorded_at: DateTime<Utc>,
) -> Vec<TurnArtifactRef> {
    use codewhale_snapshot::DeltaChange;
    delta
        .entries
        .iter()
        .map(|entry| TurnArtifactRef {
            id: file_artifact_id(&entry.path),
            kind: TurnArtifactKind::File,
            path: entry.path.clone(),
            change: Some(match entry.change {
                DeltaChange::Created => FileChangeKind::Created,
                DeltaChange::Updated => FileChangeKind::Updated,
                DeltaChange::Deleted => FileChangeKind::Deleted,
                DeltaChange::Renamed => FileChangeKind::Renamed,
            }),
            previous_path: entry.previous_path.clone(),
            size: entry.size,
            revision: entry.sha256.clone(),
            content_type: None,
            session_id: None,
            item_id: None,
            tool_call_id: None,
            tool_name: None,
            source: TurnArtifactSource::WorkspaceChangedDuringTurn,
            restore_snapshot_id: restore_snapshot_id.map(str::to_owned),
            recorded_at,
        })
        .collect()
}

/// The merged turn aggregate.
pub struct MergedTurnArtifacts {
    pub artifacts: Vec<TurnArtifactRef>,
    pub truncated: bool,
    pub omitted: u64,
}

/// Fold one item file ref into the per-path net change.
fn compose_file(
    files: &mut std::collections::HashMap<String, TurnArtifactRef>,
    next: &TurnArtifactRef,
) {
    let mut merged = next.clone();
    if next.change == Some(FileChangeKind::Renamed) {
        let source = next
            .previous_path
            .as_ref()
            .and_then(|previous| files.remove(previous));
        if let Some(source) = source {
            merged.restore_snapshot_id = source.restore_snapshot_id.or(merged.restore_snapshot_id);
            match source.change {
                // A file this turn created and then moved is simply created.
                Some(FileChangeKind::Created) => {
                    merged.change = Some(FileChangeKind::Created);
                    merged.previous_path = None;
                }
                // Renamed twice: the net rename is from the first origin.
                Some(FileChangeKind::Renamed) => merged.previous_path = source.previous_path,
                _ => {}
            }
            if merged.previous_path.as_deref() == Some(merged.path.as_str()) {
                merged.change = Some(FileChangeKind::Updated);
                merged.previous_path = None;
            }
        }
        files.insert(merged.path.clone(), merged);
        return;
    }
    let Some(previous) = files.remove(&next.path) else {
        files.insert(merged.path.clone(), merged);
        return;
    };
    merged.restore_snapshot_id = previous
        .restore_snapshot_id
        .clone()
        .or(merged.restore_snapshot_id);
    match (previous.change, next.change) {
        // Created then deleted within the turn: nothing is left to show.
        (Some(FileChangeKind::Created), Some(FileChangeKind::Deleted)) => return,
        (Some(FileChangeKind::Created), _) => merged.change = Some(FileChangeKind::Created),
        (Some(FileChangeKind::Renamed), Some(FileChangeKind::Deleted)) => {
            // The file moved and then went away: its origin is what is gone.
            let origin = previous.previous_path.clone().unwrap_or(previous.path);
            merged.id = file_artifact_id(&origin);
            merged.path = origin;
            merged.previous_path = None;
        }
        (Some(FileChangeKind::Renamed), _) => {
            merged.change = Some(FileChangeKind::Renamed);
            merged.previous_path = previous.previous_path;
        }
        // Deleted then written again: the file existed before and still does.
        (Some(FileChangeKind::Deleted), Some(FileChangeKind::Created)) => {
            merged.change = Some(FileChangeKind::Updated);
        }
        _ => {}
    }
    files.insert(merged.path.clone(), merged);
}

/// The one place a turn's aggregate is computed.
///
/// Items are the authority for what each tool call produced. Tool output and
/// media refs are always kept. File refs compose by item order (last writer
/// wins; created+deleted drops, created+updated stays created, a rename
/// folds its origin). When a settled `delta` exists it is authoritative for
/// the net workspace change, size and revision of every path the snapshots
/// can see; item provenance is copied onto the delta ref for the same path,
/// and an item path the delta omits although the snapshots track it netted
/// to no change and is dropped. Most recent first; capped.
pub fn merge_turn_artifacts<'a>(
    item_refs: impl IntoIterator<Item = &'a TurnArtifactRef>,
    delta: Option<&WorkspaceDelta>,
) -> MergedTurnArtifacts {
    let mut outputs: Vec<TurnArtifactRef> = Vec::new();
    let mut files = std::collections::HashMap::new();
    for reference in item_refs {
        if reference.kind == TurnArtifactKind::File {
            compose_file(&mut files, reference);
        } else {
            outputs.retain(|existing| existing.id != reference.id);
            outputs.push(reference.clone());
        }
    }
    let (mut truncated, mut omitted) = (false, 0);
    let mut artifacts = outputs;
    match delta {
        Some(delta) => {
            truncated = delta.truncated;
            omitted = delta.omitted;
            for mut reference in delta.refs.iter().cloned() {
                if let Some(item) = files.remove(&reference.path) {
                    reference.item_id = item.item_id;
                    reference.tool_call_id = item.tool_call_id;
                    reference.tool_name = item.tool_name;
                    reference.source = TurnArtifactSource::ToolMutation;
                    reference.recorded_at = item.recorded_at;
                    reference.restore_snapshot_id =
                        reference.restore_snapshot_id.or(item.restore_snapshot_id);
                }
                artifacts.push(reference);
            }
            artifacts.extend(
                files
                    .into_values()
                    .filter(|item| !delta.tracked_item_paths.contains(&item.path)),
            );
        }
        None => artifacts.extend(files.into_values()),
    }
    artifacts.sort_by(|left, right| {
        right
            .recorded_at
            .cmp(&left.recorded_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    if artifacts.len() > MAX_TURN_ARTIFACTS {
        truncated = true;
        omitted += (artifacts.len() - MAX_TURN_ARTIFACTS) as u64;
        artifacts.truncate(MAX_TURN_ARTIFACTS);
    }
    MergedTurnArtifacts {
        artifacts,
        truncated,
        omitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SNAPSHOT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn context<'a>(workspace: &'a [PathBuf], restore: Option<&'a str>) -> ToolArtifactContext<'a> {
        ToolArtifactContext {
            item_id: "item_1",
            tool_call_id: "call_1",
            tool_name: "apply_patch",
            workspace_roots: workspace,
            restore_snapshot_id: restore,
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn mutation_receipt_yields_confined_file_refs_with_written_facts() {
        let workspace = tempfile::tempdir().unwrap();
        let digest = codewhale_runtime::hashing::sha256_hex(b"new\n");
        let absolute = workspace.path().join("src/abs.rs");
        let metadata = json!({
            "mutation": {
                "files": [
                    { "path": "src/lib.rs", "outcome": "updated", "size": 4, "sha256": digest },
                    { "path": absolute.to_str().unwrap(), "outcome": "created", "size": 4, "sha256": digest },
                    { "path": "gone.txt", "outcome": "deleted", "size": 9, "sha256": digest },
                    // Forged or unconfined paths never become refs.
                    { "path": "../escape.txt", "outcome": "created" },
                    { "path": "/etc/passwd", "outcome": "updated" },
                    { "path": ".git/config", "outcome": "updated" },
                    { "path": "odd.txt", "outcome": "exploded" },
                    { "path": "bad-digest.txt", "outcome": "created", "size": 1, "sha256": "sha256:nothex" }
                ],
                "renames": [{ "from": "old.txt", "to": "new.txt", "size": 4, "sha256": digest }]
            },
        });
        let refs = artifact_refs_from_tool_metadata(
            &metadata,
            &context(&[workspace.path().to_path_buf()], Some(SNAPSHOT)),
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/lib.rs",
                "src/abs.rs",
                "gone.txt",
                "bad-digest.txt",
                "new.txt"
            ]
        );
        let lib = &refs[0];
        assert_eq!(lib.kind, TurnArtifactKind::File);
        assert_eq!(lib.id, file_artifact_id("src/lib.rs"));
        assert_eq!(lib.change, Some(FileChangeKind::Updated));
        assert_eq!(lib.size, Some(4));
        assert_eq!(lib.revision.as_deref(), Some(digest.as_str()));
        assert_eq!(lib.source, TurnArtifactSource::ToolMutation);
        assert_eq!(lib.restore_snapshot_id.as_deref(), Some(SNAPSHOT));
        assert_eq!(lib.item_id.as_deref(), Some("item_1"));
        assert_eq!(lib.tool_call_id.as_deref(), Some("call_1"));
        // Deleted entries carry no size or revision even if a receipt did.
        assert_eq!(refs[2].change, Some(FileChangeKind::Deleted));
        assert_eq!((refs[2].size, refs[2].revision.as_deref()), (None, None));
        // A malformed digest is dropped, not passed through.
        assert_eq!(refs[3].revision, None);
        let renamed = &refs[4];
        assert_eq!(renamed.change, Some(FileChangeKind::Renamed));
        assert_eq!(renamed.previous_path.as_deref(), Some("old.txt"));

        // The legacy projection holds only live workspace paths.
        assert_eq!(
            legacy_artifact_refs(&refs),
            ["src/lib.rs", "src/abs.rs", "bad-digest.txt", "new.txt"]
                .map(PathBuf::from)
                .to_vec()
        );
    }

    #[test]
    fn restore_point_comes_only_from_the_recorded_receipt() {
        let workspace = tempfile::tempdir().unwrap();
        // A tool cannot name its own restore point: result metadata that
        // claims one is ignored (#6621).
        let metadata = json!({
            "mutation": { "files": [{ "path": "a.txt", "outcome": "created" }] },
            "restore_snapshot_id": SNAPSHOT,
            "restore_snapshot_session_id": "sess-1",
        });
        let restore = |recorded| {
            artifact_refs_from_tool_metadata(
                &metadata,
                &context(&[workspace.path().to_path_buf()], recorded),
            )[0]
            .restore_snapshot_id
            .clone()
        };
        let recorded = "fedcba9876543210fedcba9876543210fedcba98";
        assert_eq!(restore(Some(recorded)).as_deref(), Some(recorded));
        assert_eq!(restore(None), None);
        assert_eq!(restore(Some("not-a-snapshot")), None);
    }

    #[test]
    fn spill_and_media_receipts_yield_session_refs_and_forgeries_yield_none() {
        let workspace = tempfile::tempdir().unwrap();
        let digest = codewhale_runtime::hashing::sha256_hex(b"output");
        let handle = format!("art_image_{digest}");
        let metadata = json!({
            "artifact_id": "art_call_1",
            "artifact_session_id": "sess-abc",
            "artifact_relative_path": "artifacts/art_call_1.txt",
            "artifact_byte_size": 6,
            "artifact_digest": digest,
            "tool_media": [
                { "session_id": "sess-abc", "artifact_id": handle, "media_type": "image/png",
                  "byte_size": 10, "sha256": digest },
                { "session_id": "../x", "artifact_id": handle },
                { "session_id": "sess-abc", "artifact_id": "art_image_short" }
            ],
        });
        let refs = artifact_refs_from_tool_metadata(
            &metadata,
            &context(&[workspace.path().to_path_buf()], None),
        );
        assert_eq!(refs.len(), 2, "{refs:?}");
        let spill = &refs[0];
        assert_eq!(spill.kind, TurnArtifactKind::ToolOutput);
        assert_eq!(spill.id, "art_call_1");
        assert_eq!(spill.path, "artifacts/art_call_1.txt");
        assert_eq!(spill.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(spill.size, Some(6));
        assert_eq!(spill.revision.as_deref(), Some(digest.as_str()));
        assert_eq!(spill.source, TurnArtifactSource::ToolOutputSpill);
        let media = &refs[1];
        assert_eq!(media.kind, TurnArtifactKind::Media);
        assert_eq!(media.path, format!("artifacts/{handle}.image"));
        assert_eq!(media.content_type.as_deref(), Some("image/png"));
        assert!(legacy_artifact_refs(&refs).is_empty());

        for (key, forged) in [
            ("artifact_relative_path", json!("/abs/artifacts/x.txt")),
            ("artifact_relative_path", json!("artifacts/../../escape")),
            ("artifact_relative_path", json!("elsewhere/x.txt")),
            ("artifact_session_id", json!("../other")),
            ("artifact_id", json!("not-an-artifact")),
        ] {
            let mut value = metadata.clone();
            value[key] = forged.clone();
            value.as_object_mut().unwrap().remove("tool_media");
            assert!(
                artifact_refs_from_tool_metadata(
                    &value,
                    &context(&[workspace.path().to_path_buf()], None)
                )
                .is_empty(),
                "{key}={forged} must not become a ref"
            );
        }
    }

    fn file_ref(path: &str, change: FileChangeKind, revision: &str, at: i64) -> TurnArtifactRef {
        TurnArtifactRef {
            id: file_artifact_id(path),
            kind: TurnArtifactKind::File,
            path: path.to_string(),
            change: Some(change),
            previous_path: None,
            size: Some(revision.len() as u64),
            revision: Some(revision.to_string()),
            content_type: None,
            session_id: None,
            item_id: Some(format!("item_{at}")),
            tool_call_id: Some(format!("call_{at}")),
            tool_name: Some("write".to_string()),
            source: TurnArtifactSource::ToolMutation,
            restore_snapshot_id: Some(format!("snap_{at}")),
            recorded_at: DateTime::from_timestamp(at, 0).unwrap(),
        }
    }

    fn by_path(merged: &MergedTurnArtifacts) -> Vec<(&str, Option<FileChangeKind>, Option<&str>)> {
        let mut rows: Vec<_> = merged
            .artifacts
            .iter()
            .map(|r| (r.path.as_str(), r.change, r.revision.as_deref()))
            .collect();
        rows.sort_by_key(|row| row.0);
        rows
    }

    #[test]
    fn item_refs_compose_by_order_without_a_delta() {
        let mut renamed = file_ref("b.txt", FileChangeKind::Renamed, "r_b", 6);
        renamed.previous_path = Some("a.txt".into());
        let items = vec![
            file_ref("new.txt", FileChangeKind::Created, "r1", 1),
            file_ref("new.txt", FileChangeKind::Updated, "r2", 2),
            file_ref("tmp.txt", FileChangeKind::Created, "t1", 3),
            file_ref("tmp.txt", FileChangeKind::Deleted, "t2", 4),
            file_ref("old.txt", FileChangeKind::Updated, "o1", 5),
            file_ref("old.txt", FileChangeKind::Deleted, "o2", 5),
            renamed,
        ];
        let merged = merge_turn_artifacts(&items, None);
        assert_eq!(
            by_path(&merged),
            vec![
                ("b.txt", Some(FileChangeKind::Renamed), Some("r_b")),
                ("new.txt", Some(FileChangeKind::Created), Some("r2")),
                ("old.txt", Some(FileChangeKind::Deleted), Some("o2")),
            ]
        );
        let created = merged
            .artifacts
            .iter()
            .find(|r| r.path == "new.txt")
            .unwrap();
        // The earliest restore point survives; provenance is the last writer.
        assert_eq!(created.restore_snapshot_id.as_deref(), Some("snap_1"));
        assert_eq!(created.item_id.as_deref(), Some("item_2"));
        // Most recent first.
        assert_eq!(merged.artifacts[0].path, "b.txt");
        assert!(!merged.truncated);
    }

    #[test]
    fn a_settled_delta_is_authoritative_for_visible_paths() {
        let items = vec![
            file_ref("edited.txt", FileChangeKind::Updated, "item_rev", 1),
            // Written and then restored to its original bytes: net zero.
            file_ref("reverted.txt", FileChangeKind::Updated, "x", 2),
            // Under an excluded directory: the snapshots cannot see it.
            file_ref("dist/app.js", FileChangeKind::Created, "js", 3),
        ];
        let mut shell = file_ref("out.md", FileChangeKind::Created, "shell_rev", 9);
        shell.source = TurnArtifactSource::WorkspaceChangedDuringTurn;
        shell.item_id = None;
        shell.tool_call_id = None;
        shell.tool_name = None;
        let mut edited = file_ref("edited.txt", FileChangeKind::Updated, "delta_rev", 9);
        edited.source = TurnArtifactSource::WorkspaceChangedDuringTurn;
        edited.restore_snapshot_id = Some("pre".into());
        let delta = WorkspaceDelta {
            refs: vec![edited, shell],
            tracked_item_paths: ["edited.txt", "reverted.txt"].map(String::from).into(),
            truncated: false,
            omitted: 0,
        };
        let merged = merge_turn_artifacts(&items, Some(&delta));
        assert_eq!(
            by_path(&merged),
            vec![
                ("dist/app.js", Some(FileChangeKind::Created), Some("js")),
                (
                    "edited.txt",
                    Some(FileChangeKind::Updated),
                    Some("delta_rev")
                ),
                ("out.md", Some(FileChangeKind::Created), Some("shell_rev")),
            ]
        );
        let edited = merged
            .artifacts
            .iter()
            .find(|r| r.path == "edited.txt")
            .unwrap();
        assert_eq!(edited.source, TurnArtifactSource::ToolMutation);
        assert_eq!(edited.item_id.as_deref(), Some("item_1"));
        assert_eq!(edited.restore_snapshot_id.as_deref(), Some("pre"));
        let shell = merged
            .artifacts
            .iter()
            .find(|r| r.path == "out.md")
            .unwrap();
        assert_eq!(shell.source, TurnArtifactSource::WorkspaceChangedDuringTurn);
        assert_eq!(shell.item_id, None);
    }

    #[test]
    fn the_aggregate_is_capped_and_says_so() {
        let items: Vec<_> = (0..(MAX_TURN_ARTIFACTS as i64 + 5))
            .map(|n| file_ref(&format!("f{n}.txt"), FileChangeKind::Created, "r", n))
            .collect();
        let merged = merge_turn_artifacts(&items, None);
        assert_eq!(merged.artifacts.len(), MAX_TURN_ARTIFACTS);
        assert!(merged.truncated);
        assert_eq!(merged.omitted, 5);
        // The newest writes are the ones kept.
        assert_eq!(
            merged.artifacts[0].path,
            format!("f{}.txt", MAX_TURN_ARTIFACTS + 4)
        );
    }
}

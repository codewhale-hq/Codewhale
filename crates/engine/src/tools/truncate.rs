//! Tool-output spillover writer (#422).
//!
//! When a tool produces output that's too large to land in the model's
//! context budget, we want two things at once:
//!
//! 1. The transcript / tool-cell renders a bounded preview so the UI
//!    stays scannable.
//! 2. The full router input is preserved under its origin session so bounded
//!    retrieval and the raw-detail pager can inspect it without leaking a
//!    process-global filesystem path.
//!
//! The opt-in adaptive path writes immutable artifacts under
//! `~/.codewhale/sessions/<session>/artifacts/`. The historical
//! `~/.codewhale/tool_outputs/<sanitised-id>.txt` directory remains only for
//! classic-routing compatibility, protected by a digest-bound origin sidecar.
//!
//! Boot prune drops files whose mtime is older than [`SPILLOVER_MAX_AGE`]
//! (7 days). Prune failures are logged and never fatal — the user
//! shouldn't see startup wedge because of a stale tool-output file.
//!
//! ## Live callers
//!
//! * [`apply_spillover`] — invoked from the engine's tool-execution
//!   path (`turn_loop.rs`) so any successful tool result over
//!   [`SPILLOVER_THRESHOLD_BYTES`] spills to disk and the model
//!   receives a bounded plain preview: a [`SPILLOVER_HEAD_BYTES`] head,
//!   a short retained tail, and an honest footer naming the on-disk
//!   path of the full output plus a one-line recovery instruction.
//! * Boot prune in `main.rs` deletes files older than
//!   [`SPILLOVER_MAX_AGE`].
//!
//! UI-side rendering is owned by `tui/history.rs::render_spillover_annotation`;
//! it exposes a calm expand affordance and the tool-details shortcut opens the
//! full retained output.

use codewhale_tools::names::SPILLOVER_RECOVERY_HINT;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tools::spec::ToolResult;

/// Name of the spillover directory under the CodeWhale home.
pub const SPILLOVER_DIR_NAME: &str = "tool_outputs";

const LEGACY_SPILLOVER_OWNER_SCHEMA_VERSION: u32 = 1;

/// Session proof for compatibility payloads kept in the historical global
/// `tool_outputs/` directory.
///
/// The payload remains in its legacy location so classic-routing rollback and
/// existing detail pagers keep working, but model retrieval is authorized only
/// when this sidecar names the active origin session and still matches the
/// immutable bytes being returned.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LegacySpilloverOwnership {
    pub schema_version: u32,
    pub origin_session: String,
    pub digest: String,
    pub size_bytes: u64,
}

/// Default threshold above which a tool result is a candidate for
/// spillover. Mirrors the `MAX_MEMORY_SIZE` ceiling we use elsewhere
/// for "too large to inline" so the rules feel consistent. Wired
/// callers can pass a different value if a tool family has different
/// economics.
pub const SPILLOVER_THRESHOLD_BYTES: usize = 100 * 1024; // 100 KiB

/// Default boot-prune age. Older spillover files are deleted on
/// startup to keep `~/.codewhale/tool_outputs/` from growing without
/// bound. Mirrors the workspace-snapshot 7-day default.
pub const SPILLOVER_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[cfg(any(test, feature = "test-support"))]
static TEST_SPILLOVER_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
pub static TEST_SPILLOVER_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve `~/.codewhale/tool_outputs/`. Returns `None` if the home
/// directory can't be determined (CI containers occasionally hit
/// this). Callers should treat `None` as "spillover unavailable" and
/// degrade gracefully rather than fail the tool call.
#[must_use]
pub fn spillover_root() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TEST_SPILLOVER_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return Some(root);
    }

    let home = codewhale_paths::user_home()?;
    let primary = home.join(".codewhale").join(SPILLOVER_DIR_NAME);
    let legacy = home.join(".deepseek").join(SPILLOVER_DIR_NAME);
    if primary.exists() || !legacy.exists() {
        return Some(primary);
    }
    Some(legacy)
}

/// Override the spillover root for tests without mutating `$HOME`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_test_spillover_root(root: Option<PathBuf>) -> Option<PathBuf> {
    let mut guard = TEST_SPILLOVER_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *guard, root)
}

/// Resolve the spillover-file path for a tool call id. Sanitises the
/// id so that a hostile value can't escape the storage directory.
/// Returns `None` for empty / fully-invalid ids; the caller should
/// treat that as "spillover unavailable" and skip the write.
#[must_use]
pub fn spillover_path(id: &str) -> Option<PathBuf> {
    let sanitised = sanitise_id(id)?;
    Some(spillover_root()?.join(format!("{sanitised}.txt")))
}

#[must_use]
pub fn legacy_spillover_ownership_path(payload_path: &Path) -> PathBuf {
    payload_path.with_extension("owner.json")
}

/// Publish the proof needed to retrieve a legacy-global spillover safely.
///
/// Payload publication happens first. If this atomic sidecar write fails, the
/// payload is deliberately left unowned and therefore inaccessible through
/// `retrieve_tool_result`; callers must not advertise a retrieval hint.
pub fn publish_legacy_spillover_ownership(
    payload_path: &Path,
    session_id: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    if session_id.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "legacy spillover ownership requires a session id",
        ));
    }
    let ownership = LegacySpilloverOwnership {
        schema_version: LEGACY_SPILLOVER_OWNER_SCHEMA_VERSION,
        origin_session: session_id.to_string(),
        digest: codewhale_runtime::hashing::sha256_hex(bytes),
        size_bytes: bytes.len().try_into().unwrap_or(u64::MAX),
    };
    let sidecar = legacy_spillover_ownership_path(payload_path);
    let encoded = serde_json::to_vec_pretty(&ownership)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    codewhale_util::utils::write_atomic(&sidecar, &encoded)?;
    Ok(sidecar)
}

pub fn read_legacy_spillover_ownership(
    payload_path: &Path,
) -> io::Result<LegacySpilloverOwnership> {
    let sidecar = legacy_spillover_ownership_path(payload_path);
    if std::fs::symlink_metadata(&sidecar)?
        .file_type()
        .is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "legacy spillover ownership sidecar must not be a symlink",
        ));
    }
    let ownership = serde_json::from_slice::<LegacySpilloverOwnership>(&std::fs::read(sidecar)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if ownership.schema_version != LEGACY_SPILLOVER_OWNER_SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported legacy spillover ownership schema",
        ));
    }
    Ok(ownership)
}

/// Resolve the spillover-file path for a SHA256 content hash. Separate
/// namespace (`sha_<hex>.txt`) from the tool-call-id files so legacy
/// SHA-addressed evidence can be recognized without colliding with
/// tool-call references. Retrieval still requires matching ownership
/// metadata. `sha` must be the raw 64-char lowercase hex digest —
/// case-insensitive matching is done by the caller.
#[must_use]
pub fn sha_spillover_path(sha: &str) -> Option<PathBuf> {
    let sha = sha.trim().to_ascii_lowercase();
    if !is_valid_sha256(&sha) {
        return None;
    }
    Some(spillover_root()?.join(format!("sha_{sha}.txt")))
}

/// True when `s` is a 64-character lowercase ASCII hex string. Used
/// to detect bare SHA refs the model might pass to retrieval and to
/// validate input to [`sha_spillover_path`].
#[must_use]
pub fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Write a legacy SHA-addressed spillover fixture for ownership tests.
#[cfg(any(test, feature = "test-support"))]
pub fn write_sha_spillover(sha: &str, content: &str) -> io::Result<PathBuf> {
    let path = sha_spillover_path(sha).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "sha must be a 64-char lowercase hex digest",
        )
    })?;
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    codewhale_util::utils::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

/// Write `content` to the spillover file for `id`. Creates the
/// parent directory if needed. Returns the resolved path on success.
///
/// Atomic via `write` + filesystem rename guarantees from the
/// underlying OS — the file is created at a temp name first and
/// then renamed into place. Failures bubble up as `io::Error` so the
/// caller can decide whether to surface them.
pub fn write_spillover(id: &str, content: &str) -> io::Result<PathBuf> {
    let path = spillover_path(id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve spillover path (empty/invalid id or missing home directory)",
        )
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    codewhale_util::utils::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

/// Drop spillover files older than `max_age`. Returns the number of
/// files removed. Non-fatal: directory-missing returns 0; per-file
/// errors are logged and skipped. Mirrors
/// [`crate::session_manager::prune_workspace_snapshots`].
pub fn prune_older_than(max_age: Duration) -> io::Result<usize> {
    let Some(root) = spillover_root() else {
        return Ok(0);
    };
    if !root.exists() {
        return Ok(0);
    }
    let cutoff = SystemTime::now()
        .checked_sub(max_age)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut pruned = 0usize;
    for entry in fs::read_dir(&root)? {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(target: "spillover", ?err, "skipping unreadable dir entry");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(target: "spillover", ?err, ?path, "skipping unreadable mtime");
                continue;
            }
        };
        if modified < cutoff {
            if let Err(err) = fs::remove_file(&path) {
                tracing::warn!(target: "spillover", ?err, ?path, "spillover prune skipped a file");
                continue;
            }
            pruned += 1;
        }
    }
    Ok(pruned)
}

/// Convenience for the common "too long? spill it." pattern. If
/// `content` is at or below `threshold` bytes, returns `None` and the
/// caller keeps the inline content. Above the threshold, writes the
/// full content to the spillover file and returns
/// `Some((head, path))` where `head` is the leading slice the caller
/// can show inline. The trailing tail isn't returned — `path` is the
/// canonical reference.
///
/// `head_bytes` controls how much inline content the caller wants to
/// keep. Pass `threshold` for "preserve as much as fits inline" or
/// a smaller value (e.g. `4 * 1024`) for "show a peek".
pub fn maybe_spillover(
    id: &str,
    content: &str,
    threshold: usize,
    head_bytes: usize,
) -> io::Result<Option<(String, PathBuf)>> {
    if content.len() <= threshold {
        return Ok(None);
    }
    let path = write_spillover(id, content)?;
    // Don't slice mid-utf8: walk back to a char boundary if needed.
    let cut = head_bytes.min(content.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| content.is_char_boundary(i))
        .unwrap_or(0);
    Ok(Some((content[..cut].to_string(), path)))
}

/// Inline head retained when [`apply_spillover`] truncates a tool
/// result. 32 KiB is large enough for the model to keep meaningful
/// context (a long stack trace, a `git diff` head, a directory
/// listing of typical depth) without consuming the lion's share of
/// the per-turn context budget. The full output is preserved
/// internally and opens in the tool details view.
pub const SPILLOVER_HEAD_BYTES: usize = 32 * 1024;
/// Inline tail retained alongside the head so compiler summaries and final
/// test failures are not systematically hidden by truncation.
pub const SPILLOVER_TAIL_BYTES: usize = 8 * 1024;

/// Inline head/tail budgets for the adaptive evidence bands. Hybrid results
/// keep a generous 32 KiB head + 8 KiB tail so mid-size outputs stay mostly
/// readable; handle-only results keep a 16 KiB head + 4 KiB tail. The head
/// and tail windows never overlap ([`head_tail_windows`]).
const HYBRID_HEAD_BYTES: usize = 32 * 1024;
const HYBRID_TAIL_BYTES: usize = 8 * 1024;
const HANDLE_ONLY_HEAD_BYTES: usize = 16 * 1024;
const HANDLE_ONLY_TAIL_BYTES: usize = 4 * 1024;

/// Phrase used only by the TUI expand affordance and the UI-side detection of
/// historical truncated previews. Never emitted into model-facing content:
/// the model cannot open the tool details view, so the model-facing footer
/// carries the artifact path and a recovery instruction instead.
pub const SPILLOVER_PREVIEW_HINT: &str = "view full output in the tool details view";

/// Model-facing recovery instruction for a truncated tool result.
///
/// The previous text — "read it back with the read_file tool or with sed line
/// ranges" — named `read_file`, which is not model-visible at all (only `File`
/// is), and otherwise leaned on reaching the artifact by path. Reaching it by
/// path is *conditional*: `ToolContext::resolve_path` short-circuits under
/// trust mode, so `File action="read"` on an artifact under
/// `~/.codewhale/sessions/` succeeds in a trusted/auto session and is refused
/// as a path escape otherwise — and even when it succeeds it pages the file
/// rather than seeking the omitted range. Meanwhile `retrieve_tool_result` —
/// model-visible, purpose-built, unconditional, and already named correctly by
/// the web overflow path in `tools/web/overflow.rs` — went unmentioned.
/// `tests/adaptive_evidence_acceptance.rs` proves end to end that a model
/// handed one of these receipts can take the named ref and get the omitted
/// bytes back.
///
/// The distinction that matters is retrievability, not tidiness. An adaptive
/// session artifact carries an `art_<id>` the retrieval tool resolves, so name
/// it. A legacy global spillover is authorized by an ownership sidecar whose
/// write is allowed to fail (see [`publish_legacy_spillover_ownership`]), so
/// promising retrieval there would just be a fourth dead route; say plainly
/// that there is no tool call for it and name what does work instead.
pub fn spillover_recovery_instruction(retrieval_ref: Option<&str>) -> String {
    match retrieval_ref {
        Some(reference) => format!(
            "{SPILLOVER_RECOVERY_HINT} call retrieve_tool_result with ref=\"{reference}\" \
             (mode=\"tail\" for the end, mode=\"lines\" with lines=\"120-160\" for a range, \
             mode=\"query\" with query=\"…\" to search it)"
        ),
        None => format!(
            "{SPILLOVER_RECOVERY_HINT} no tool call reaches this copy — re-run the command \
             with narrower output (a tighter filter, or head/tail) if you need the rest"
        ),
    }
}

/// Model-facing footer for a truncated tool result. Names how much was
/// omitted (bytes and lines), where the complete output lives on disk, and
/// how the model can read the omitted range back.
fn spillover_preview_footer(
    omitted_bytes: usize,
    omitted_lines: usize,
    recovery_path: &str,
    retrieval_ref: Option<&str>,
) -> String {
    format!(
        "… {} of output omitted ({omitted_lines} lines) — full output at {recovery_path}; {}",
        codewhale_runtime::artifacts::format_byte_size(
            omitted_bytes.try_into().unwrap_or(u64::MAX)
        ),
        spillover_recovery_instruction(retrieval_ref)
    )
}

/// Split `content` into a head of at most `head_bytes` and a tail of at most
/// `tail_bytes` that never overlap: the tail window always starts at or after
/// the head window ends, so no byte of the output appears twice and the
/// omitted count is exact.
fn head_tail_windows(content: &str, head_bytes: usize, tail_bytes: usize) -> (&str, &str) {
    let head_end = (0..=head_bytes.min(content.len()))
        .rev()
        .find(|&index| content.is_char_boundary(index))
        .unwrap_or(0);
    let tail_floor = content.len().saturating_sub(tail_bytes).max(head_end);
    let tail_start = (tail_floor..=content.len())
        .find(|&index| content.is_char_boundary(index))
        .unwrap_or(content.len());
    (&content[..head_end], &content[tail_start..])
}

/// Build the model-facing preview for a truncated tool result: the head, an
/// honest footer naming how much was omitted and where the full output can be
/// read back, and a short retained tail. When the head and tail windows cover
/// the whole output (nothing was actually omitted), the content is returned
/// unchanged — the preview never claims a truncation that did not happen.
fn truncated_preview(
    head: &str,
    tail: &str,
    original: &str,
    recovery_path: &str,
    retrieval_ref: Option<&str>,
) -> String {
    let omitted = original.len().saturating_sub(head.len() + tail.len());
    if omitted == 0 {
        return original.to_string();
    }
    let omitted_lines = original[head.len()..original.len() - tail.len()]
        .lines()
        .count();
    format!(
        "{head}\n\n{}\n\n…\n{tail}",
        spillover_preview_footer(omitted, omitted_lines, recovery_path, retrieval_ref)
    )
}

/// Apply spillover to a tool result in place. If the result's
/// content exceeds [`SPILLOVER_THRESHOLD_BYTES`], writes the full
/// content to a sibling file under `~/.codewhale/tool_outputs/`,
/// replaces `result.content` with a [`SPILLOVER_HEAD_BYTES`] head
/// plus a footer naming the spillover path and how to read the
/// omitted range back, and stamps `metadata.spillover_path` so the
/// UI can render its expand annotation.
///
/// Returns the spillover path on success, `None` if no spillover
/// happened (content small enough, error result, write failure).
/// Failures are logged but never bubble up — a tool that produced a
/// result shouldn't be marked failed because the spillover writer
/// couldn't reach disk; a bounded preview then says the full output could
/// not be saved, without advertising a recovery path or artifact.
///
/// Error results (`success == false`) are skipped: error messages
/// are typically short, and turning them into a truncated preview
/// would just hide the error from the model's reasoning.
pub fn apply_spillover(result: &mut ToolResult, tool_id: &str) -> Option<PathBuf> {
    apply_spillover_inner(result, tool_id, None, false)
}

/// Apply spillover and publish session-scoped exact evidence.
///
/// The default (classic) path writes the full bytes under the origin session
/// and replaces oversized content with a bounded head/tail preview whose
/// footer names the artifact path and how to read the omitted range back.
/// The adaptive evidence lane is reachable only through the explicit
/// `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING` opt-in.
pub fn apply_spillover_with_artifact(
    result: &mut ToolResult,
    tool_id: &str,
    tool_name: &str,
    session_id: &str,
) -> Option<PathBuf> {
    apply_spillover_inner(
        result,
        tool_id,
        Some(ArtifactSpilloverContext {
            tool_name,
            session_id,
        }),
        false,
    )
}

/// [`apply_spillover_with_artifact`] for callers whose error payloads are
/// routinely as large as their successes — sub-agent tool output is often a
/// full build log, so the root loop's pass-errors-through rationale does not
/// hold there.
pub fn apply_spillover_with_artifact_including_errors(
    result: &mut ToolResult,
    tool_id: &str,
    tool_name: &str,
    session_id: &str,
) -> Option<PathBuf> {
    apply_spillover_inner(
        result,
        tool_id,
        Some(ArtifactSpilloverContext {
            tool_name,
            session_id,
        }),
        true,
    )
}

#[derive(Clone, Copy)]
struct ArtifactSpilloverContext<'a> {
    tool_name: &'a str,
    session_id: &'a str,
}

fn apply_spillover_inner(
    result: &mut ToolResult,
    tool_id: &str,
    artifact_context: Option<ArtifactSpilloverContext<'_>>,
    bound_errors: bool,
) -> Option<PathBuf> {
    if crate::tools::large_output_router::adaptive_output_routing_enabled()
        && let Some(context) = artifact_context
    {
        return apply_adaptive_evidence_inner(result, tool_id, context);
    }
    if !result.success && !bound_errors {
        return None;
    }
    if result.content.len() <= SPILLOVER_THRESHOLD_BYTES {
        return None;
    }
    let original_content = result.content.clone();
    let outcome = match maybe_spillover(
        tool_id,
        &original_content,
        SPILLOVER_THRESHOLD_BYTES,
        SPILLOVER_HEAD_BYTES,
    ) {
        Ok(Some(pair)) => pair,
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(
                target: "spillover",
                ?err,
                tool_id,
                "spillover write failed; retaining a bounded unsaved preview"
            );
            bound_unpersisted_output(result, SPILLOVER_HEAD_BYTES, SPILLOVER_TAIL_BYTES);
            return None;
        }
    };
    let (_head, path) = outcome;
    let (head, tail) = head_tail_windows(
        &original_content,
        SPILLOVER_HEAD_BYTES,
        SPILLOVER_TAIL_BYTES,
    );
    let digest = codewhale_runtime::hashing::sha256_hex(original_content.as_bytes());
    let path_str = path.display().to_string();

    // Keep publishing the legacy ownership proof even though the model-facing
    // footer no longer mentions retrieval: the tool-details pager authorizes
    // legacy spillover reads through this sidecar.
    if let Some(context) = artifact_context
        && let Err(err) = publish_legacy_spillover_ownership(
            &path,
            context.session_id,
            original_content.as_bytes(),
        )
    {
        tracing::warn!(
            target: "spillover",
            ?err,
                tool_id,
                "legacy spillover ownership publication failed"
        );
    }

    let mut artifact_path = None;
    if let Some(context) = artifact_context {
        let artifact_id = codewhale_runtime::artifacts::artifact_id_for_tool_call(tool_id);
        // Publish immutably, like adaptive evidence: a turn's artifact
        // reference names these bytes by digest, so a later call that reuses
        // the id must fail closed (legacy footer) instead of rewriting them.
        match codewhale_runtime::artifacts::write_session_artifact_immutable(
            context.session_id,
            &artifact_id,
            original_content.as_bytes(),
        ) {
            Ok((absolute_path, relative_path)) => {
                let record = codewhale_runtime::artifacts::record_tool_output_artifact(
                    context.session_id,
                    tool_id,
                    context.tool_name,
                    relative_path.clone(),
                    &original_content,
                );
                result.content = truncated_preview(
                    head,
                    tail,
                    &original_content,
                    &codewhale_runtime::artifacts::format_artifact_relative_path(&absolute_path),
                    Some(artifact_id.as_str()),
                );
                artifact_path = Some((absolute_path, relative_path, record));
            }
            Err(err) => {
                tracing::warn!(
                    target: "spillover",
                    ?err,
                    tool_id,
                    "session artifact write failed; falling back to legacy spillover footer"
                );
            }
        }
    }

    if artifact_path.is_none() {
        // Legacy fallback: no session artifact was written, so there is no
        // `art_<id>` ref to hand over — only the on-disk path.
        result.content = truncated_preview(head, tail, &original_content, &path_str, None);
    }

    let obj = metadata_object_mut(result);
    if let Some((absolute_path, relative_path, record)) = artifact_path.as_ref() {
        stamp_artifact_metadata(obj, absolute_path, relative_path, record);
        obj.insert(
            "legacy_spillover_path".into(),
            serde_json::Value::String(path_str),
        );
    } else {
        obj.insert("spillover_path".into(), serde_json::Value::String(path_str));
    }
    if let Some(obj) = result
        .metadata
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
    {
        obj.insert("truncated".into(), serde_json::Value::Bool(true));
        obj.insert(
            "content_digest".into(),
            serde_json::Value::String(format!("sha256:{digest}")),
        );
        if artifact_path.is_some() {
            // Plain hex, the same form the adaptive path and the workspace
            // read `revision` use; the turn artifact ref carries it verbatim.
            obj.insert(
                "artifact_digest".into(),
                serde_json::Value::String(digest.clone()),
            );
        }
        obj.insert(
            "original_byte_count".into(),
            serde_json::Value::Number(serde_json::Number::from(original_content.len() as u64)),
        );
        obj.insert(
            "retained_head_bytes".into(),
            serde_json::Value::Number(serde_json::Number::from(head.len() as u64)),
        );
        obj.insert(
            "retained_tail_bytes".into(),
            serde_json::Value::Number(serde_json::Number::from(tail.len() as u64)),
        );
        obj.insert(
            "original_line_count".into(),
            serde_json::Value::Number(serde_json::Number::from(
                original_content.lines().count() as u64
            )),
        );
    }
    artifact_path
        .map(|(absolute_path, _, _)| absolute_path)
        .or(Some(path))
}

/// The result's metadata as a JSON object, created when absent. Metadata that
/// is not an object is kept under `_prior` rather than dropped.
fn metadata_object_mut(result: &mut ToolResult) -> &mut serde_json::Map<String, serde_json::Value> {
    let metadata = result.metadata.get_or_insert_with(|| serde_json::json!({}));
    if !metadata.is_object() {
        let prior = std::mem::replace(metadata, serde_json::json!({}));
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert("_prior".into(), prior);
        }
    }
    metadata
        .as_object_mut()
        .expect("metadata was just made an object")
}

/// Bound a failed persistence attempt without turning its preview into exact
/// evidence. The same windows and footer authority serve both routing modes.
fn bound_unpersisted_output(result: &mut ToolResult, head_bytes: usize, tail_bytes: usize) {
    let original = &result.content;
    let (head, tail) = head_tail_windows(original, head_bytes, tail_bytes);
    let omitted = original.len().saturating_sub(head.len() + tail.len());
    if omitted == 0 {
        return;
    }
    let original_bytes = original.len();
    let original_lines = original.lines().count();
    let head_len = head.len();
    let tail_len = tail.len();
    let omitted_lines = original[head_len..original_bytes - tail_len]
        .lines()
        .count();
    let digest = codewhale_runtime::hashing::sha256_hex(original.as_bytes());
    result.content = format!(
        "{head}\n\n{}\n\n…\n{tail}",
        preview_footer(omitted, omitted_lines, None, None)
    );
    let object = metadata_object_mut(result);
    // A prior metadata envelope cannot claim that it retained these bytes.
    for key in [
        "spillover_path",
        "legacy_spillover_path",
        "artifact_path",
        "artifact_id",
        "artifact_session_id",
        "artifact_relative_path",
        "artifact_byte_size",
        "artifact_digest",
        "artifact_generation",
        "artifact_encoding",
        "artifact_retention_state",
        "artifact_preview",
        "artifact_record",
    ] {
        object.remove(key);
    }
    object.insert("evidence_available".into(), false.into());
    object.insert("output_persistence_failed".into(), true.into());
    object.insert("truncated".into(), true.into());
    object.insert("content_digest".into(), format!("sha256:{digest}").into());
    object.insert("original_byte_count".into(), original_bytes.into());
    object.insert("original_line_count".into(), original_lines.into());
    object.insert("retained_head_bytes".into(), head_len.into());
    object.insert("retained_tail_bytes".into(), tail_len.into());
}

/// Stamp the keys the TUI, receipts and `retrieve_tool_result` use to find a
/// session artifact that holds a tool call's full output.
fn stamp_artifact_metadata(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    absolute_path: &Path,
    relative_path: &Path,
    record: &codewhale_runtime::artifacts::ArtifactRecord,
) {
    let absolute = serde_json::Value::String(absolute_path.display().to_string());
    obj.insert("spillover_path".into(), absolute.clone());
    obj.insert("artifact_path".into(), absolute);
    obj.insert("artifact_id".into(), record.id.clone().into());
    obj.insert(
        "artifact_session_id".into(),
        record.session_id.clone().into(),
    );
    obj.insert(
        "artifact_relative_path".into(),
        codewhale_runtime::artifacts::format_artifact_relative_path(relative_path).into(),
    );
    obj.insert("artifact_byte_size".into(), record.byte_size.into());
    obj.insert("artifact_preview".into(), record.preview.clone().into());
}

/// Save a tool call's full output as a session artifact before the model's
/// view of it is cut to the inline budget (#6508).
///
/// The result's content is left as it is, so the UI cell still shows it; only
/// the metadata gains the artifact keys, so the model-context view can name a
/// ref `retrieve_tool_result` resolves. A result that already has an artifact
/// (spillover wrote one) is left alone. Returns whether an artifact exists
/// afterwards. Publication is immutable: a reused ID may name the same bytes,
/// but never replace earlier output. A failed write is logged and reported as
/// `false`; the caller's footer then says no tool call reaches the rest instead
/// of promising a ref.
pub fn preserve_full_output_for_model_context(
    result: &mut ToolResult,
    tool_id: &str,
    tool_name: &str,
    session_id: &str,
) -> bool {
    if result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("output_persistence_failed"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        // Persistence already lost the omitted bytes. Saving this preview as
        // the full output would manufacture an exact-evidence receipt.
        return false;
    }
    if result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("artifact_id"))
        .is_some()
    {
        return true;
    }
    let artifact_id = codewhale_runtime::artifacts::artifact_id_for_tool_call(tool_id);
    match codewhale_runtime::artifacts::write_session_artifact_immutable(
        session_id,
        &artifact_id,
        result.content.as_bytes(),
    ) {
        Ok((absolute_path, relative_path)) => {
            let record = codewhale_runtime::artifacts::record_tool_output_artifact(
                session_id,
                tool_id,
                tool_name,
                relative_path.clone(),
                &result.content,
            );
            let digest = codewhale_runtime::hashing::sha256_hex(result.content.as_bytes());
            let obj = metadata_object_mut(result);
            stamp_artifact_metadata(obj, &absolute_path, &relative_path, &record);
            obj.insert("content_digest".into(), format!("sha256:{digest}").into());
            true
        }
        Err(err) => {
            tracing::warn!(
                target: "spillover",
                ?err,
                tool_id,
                "could not save full tool output before cutting it for model context"
            );
            false
        }
    }
}

/// Bytes reserved beside the footer for the preview's blank lines and the
/// `…` separator, plus slack for the omitted-size text changing length.
const PREVIEW_FRAME_BYTES: usize = 32;

/// Footer for a preview whose full output lives at `recovery_path`, or, when
/// it is `None`, one that says plainly the full output could not be saved.
fn preview_footer(
    omitted_bytes: usize,
    omitted_lines: usize,
    recovery_path: Option<&str>,
    retrieval_ref: Option<&str>,
) -> String {
    match recovery_path {
        Some(path) => spillover_preview_footer(omitted_bytes, omitted_lines, path, retrieval_ref),
        None => format!(
            "… {} of output omitted ({omitted_lines} lines) — the full output could not be saved; {}",
            codewhale_runtime::artifacts::format_byte_size(
                omitted_bytes.try_into().unwrap_or(u64::MAX)
            ),
            spillover_recovery_instruction(None)
        ),
    }
}

/// Drop the path and preview before sacrificing the retrieval reference.
/// Budgets smaller than this complete instruction cannot contain a usable
/// receipt: keep it intact and let the wire's recovery-marker guard preserve it.
fn compact_recovery_footer(retrieval_ref: Option<&str>) -> String {
    match retrieval_ref {
        Some(reference) => {
            format!("{SPILLOVER_RECOVERY_HINT} retrieve_tool_result ref=\"{reference}\"")
        }
        None => format!("{SPILLOVER_RECOVERY_HINT} re-run with narrower output"),
    }
}

/// Fit `content` into `budget` characters for the model: nothing changes when
/// it already fits; otherwise a head (two thirds) and a tail (one third)
/// around one recovery footer. The cut is measured in bytes, so the result
/// never exceeds `budget` characters either, unless the budget cannot hold
/// even a compact recovery instruction (which must remain intact).
pub fn fit_to_inline_budget(
    content: &str,
    budget: usize,
    recovery_path: Option<&str>,
    retrieval_ref: Option<&str>,
) -> String {
    if content.chars().count() <= budget {
        return content.to_string();
    }
    let footer = preview_footer(
        content.len(),
        content.lines().count(),
        recovery_path,
        retrieval_ref,
    );
    if footer.len() + PREVIEW_FRAME_BYTES > budget {
        return compact_recovery_footer(retrieval_ref);
    }
    let room = budget.saturating_sub(footer.len() + PREVIEW_FRAME_BYTES);
    let head_bytes = room * 2 / 3;
    let (head, tail) = head_tail_windows(content, head_bytes, room - head_bytes);
    let omitted = content.len().saturating_sub(head.len() + tail.len());
    let omitted_lines = content[head.len()..content.len() - tail.len()]
        .lines()
        .count();
    format!(
        "{head}\n\n{}\n\n…\n{tail}",
        preview_footer(omitted, omitted_lines, recovery_path, retrieval_ref)
    )
}

/// Re-fit a spillover preview (head, footer, tail) to `budget` characters.
///
/// Spillover already saved the full output, so this writes nothing: it keeps
/// a shorter head and tail from the preview's own windows (found through the
/// `retained_head_bytes`/`retained_tail_bytes` metadata spillover stamped) and
/// re-emits one footer that names the same recovery ref. Returns `None` when
/// the metadata does not describe this preview, so the caller can fall back.
pub fn refit_spilled_preview(
    preview: &str,
    metadata: &serde_json::Value,
    budget: usize,
    recovery_path: Option<&str>,
    retrieval_ref: Option<&str>,
) -> Option<String> {
    if preview.chars().count() <= budget {
        return Some(preview.to_string());
    }
    let field = |key: &str| {
        metadata
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    };
    let head_len = field("retained_head_bytes")?;
    let tail_len = field("retained_tail_bytes")?;
    let original_len = field("original_byte_count")?;
    if head_len + tail_len > preview.len()
        || !preview.is_char_boundary(head_len)
        || !preview.is_char_boundary(preview.len() - tail_len)
    {
        return None;
    }
    let head_window = &preview[..head_len];
    let tail_window = &preview[preview.len() - tail_len..];

    let footer = preview_footer(
        original_len,
        field("original_line_count").unwrap_or(0),
        recovery_path,
        retrieval_ref,
    );
    if footer.len() + PREVIEW_FRAME_BYTES > budget {
        return Some(compact_recovery_footer(retrieval_ref));
    }
    let room = budget.saturating_sub(footer.len() + PREVIEW_FRAME_BYTES);
    let head_bytes = (room * 2 / 3).min(head_len);
    let tail_bytes = (room - head_bytes).min(tail_len);
    let (head, _) = head_tail_windows(head_window, head_bytes, 0);
    let (_, tail) = head_tail_windows(tail_window, 0, tail_bytes);
    let omitted = original_len.saturating_sub(head.len() + tail.len());
    let kept_lines = head.lines().count() + tail.lines().count();
    let omitted_lines = match field("original_line_count") {
        Some(total) => total.saturating_sub(kept_lines),
        // Older spills did not record a line count; count only the lines this
        // preview can see, which understates rather than invents.
        None => {
            head_window[head.len()..].lines().count()
                + tail_window[..tail_window.len() - tail.len()]
                    .lines()
                    .count()
        }
    };
    Some(format!(
        "{head}\n\n{}\n\n…\n{tail}",
        preview_footer(omitted, omitted_lines, recovery_path, retrieval_ref)
    ))
}

fn apply_adaptive_evidence_inner(
    result: &mut ToolResult,
    tool_id: &str,
    context: ArtifactSpilloverContext<'_>,
) -> Option<PathBuf> {
    use crate::tools::large_output_router::{
        DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS, EVIDENCE_RETENTION_SECS, EvidenceArtifact,
        EvidenceRetentionState, EvidenceRouting, estimate_tokens, publish_evidence_metadata,
        unix_millis_now,
    };

    let estimated_tokens = estimate_tokens(&result.content);
    let threshold = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("evidence_threshold_tokens"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS);
    let routing = result
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("evidence_routing"))
        .cloned()
        .and_then(|value| serde_json::from_value::<EvidenceRouting>(value).ok())
        .unwrap_or_else(|| EvidenceRouting::from_token_estimate(estimated_tokens, threshold));
    if routing == EvidenceRouting::Inline {
        return None;
    }

    let original = result.content.clone();
    let (head_bytes, tail_bytes) = if routing == EvidenceRouting::Hybrid {
        (HYBRID_HEAD_BYTES, HYBRID_TAIL_BYTES)
    } else {
        (HANDLE_ONLY_HEAD_BYTES, HANDLE_ONLY_TAIL_BYTES)
    };
    let (head, tail) = head_tail_windows(&original, head_bytes, tail_bytes);
    let omitted = original.len().saturating_sub(head.len() + tail.len());
    if omitted == 0 {
        // The whole output fits inside the preview budget: there is nothing
        // to recover, so publishing an artifact and claiming a truncation
        // would both be dishonest. Pass the content through unchanged.
        return None;
    }
    let head_len = head.len();
    let tail_len = tail.len();

    let artifact_id = codewhale_runtime::artifacts::artifact_id_for_tool_call(tool_id);
    let relative_path = codewhale_runtime::artifacts::session_artifact_relative_path(&artifact_id);
    let digest = codewhale_runtime::hashing::sha256_hex(original.as_bytes());
    let now_ms = unix_millis_now();
    let proposed_artifact = EvidenceArtifact {
        handle: artifact_id.clone(),
        digest: digest.clone(),
        size_bytes: original.len().try_into().unwrap_or(u64::MAX),
        content_type: if serde_json::from_str::<serde_json::Value>(&original).is_ok() {
            "application/json".to_string()
        } else {
            "text/plain".to_string()
        },
        tool_name: context.tool_name.to_string(),
        call_id: tool_id.to_string(),
        origin_session: context.session_id.to_string(),
        generation: 1,
        redacted: false,
        encoding: "utf-8".to_string(),
        retention_state: EvidenceRetentionState::Live,
        created_at_unix_ms: now_ms,
        retain_until_unix_ms: now_ms.saturating_add(EVIDENCE_RETENTION_SECS * 1_000),
        storage_path: relative_path.clone(),
    };
    let artifact = match crate::tools::large_output_router::read_evidence_metadata(
        context.session_id,
        &artifact_id,
    ) {
        Ok(existing)
            if existing.digest == proposed_artifact.digest
                && existing.size_bytes == proposed_artifact.size_bytes
                && existing.call_id == proposed_artifact.call_id
                && existing.origin_session == proposed_artifact.origin_session =>
        {
            existing
        }
        Ok(_) => {
            tracing::warn!(target: "evidence", tool_id, "adaptive evidence replay conflicts with immutable metadata");
            bound_unpersisted_output(result, head_bytes, tail_bytes);
            return None;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = publish_evidence_metadata(context.session_id, &proposed_artifact) {
                tracing::warn!(target: "evidence", ?err, tool_id, "adaptive evidence metadata publication failed");
                bound_unpersisted_output(result, head_bytes, tail_bytes);
                return None;
            }
            proposed_artifact
        }
        Err(err) => {
            tracing::warn!(target: "evidence", ?err, tool_id, "adaptive evidence metadata validation failed");
            bound_unpersisted_output(result, head_bytes, tail_bytes);
            return None;
        }
    };

    // Seal the ownership/integrity record before publishing predictable
    // `art_<call>.txt` bytes. If metadata publication fails, no payload exists
    // for a guessed handle to retrieve without the generation, redaction,
    // retention, size, and digest checks above. A metadata-only interruption
    // is safe: the handle is never advertised and a retry can idempotently
    // publish the matching bytes.
    let (absolute_path, relative_path) =
        match codewhale_runtime::artifacts::write_session_artifact_immutable(
            context.session_id,
            &artifact_id,
            original.as_bytes(),
        ) {
            Ok(paths) => paths,
            Err(err) => {
                tracing::warn!(target: "evidence", ?err, tool_id, "adaptive evidence content publication failed");
                bound_unpersisted_output(result, head_bytes, tail_bytes);
                return None;
            }
        };

    let record = codewhale_runtime::artifacts::record_tool_output_artifact(
        context.session_id,
        tool_id,
        context.tool_name,
        relative_path.clone(),
        &original,
    );
    result.content = truncated_preview(
        head,
        tail,
        &original,
        &codewhale_runtime::artifacts::format_artifact_relative_path(&absolute_path),
        Some(artifact_id.as_str()),
    );
    let metadata = result.metadata.get_or_insert_with(|| serde_json::json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "spillover_path".into(),
            absolute_path.display().to_string().into(),
        );
        object.insert("artifact_id".into(), artifact_id.into());
        object.insert("artifact_session_id".into(), context.session_id.into());
        object.insert(
            "artifact_relative_path".into(),
            codewhale_runtime::artifacts::format_artifact_relative_path(&relative_path).into(),
        );
        object.insert("artifact_byte_size".into(), artifact.size_bytes.into());
        object.insert("artifact_digest".into(), digest.into());
        object.insert("artifact_generation".into(), artifact.generation.into());
        object.insert("artifact_encoding".into(), artifact.encoding.into());
        object.insert("artifact_retention_state".into(), "live".into());
        object.insert("evidence_available".into(), true.into());
        object.insert("truncated".into(), true.into());
        object.insert("original_byte_count".into(), artifact.size_bytes.into());
        object.insert("retained_head_bytes".into(), head_len.into());
        object.insert("retained_tail_bytes".into(), tail_len.into());
        object.insert(
            "artifact_preview".into(),
            original.chars().take(200).collect::<String>().into(),
        );
        object.insert(
            "artifact_record".into(),
            serde_json::to_value(record).unwrap_or(serde_json::Value::Null),
        );
    }
    Some(absolute_path)
}

/// Sanitise a tool call id for use as a filename. Keeps ASCII
/// alphanumerics, `-`, and `_`; rejects `.` to keep `..` traversal
/// out, rejects empty results. Returns `None` if the input contains
/// no acceptable characters.
fn sanitise_id(id: &str) -> Option<String> {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Override the storage roots for tests so they don't pollute the
/// user's real `~/.codewhale/` directory. This uses explicit test hooks instead
/// of `$HOME` because Windows home-dir resolution can ignore environment
/// overrides and return the runner profile directory.
#[cfg(any(test, feature = "test-support"))]
pub fn with_test_home<F, R>(home: &Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _artifact_guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
        .lock()
        .unwrap_or_else(|err| err.into_inner());

    struct StorageRootOverride {
        prior_spillover: Option<PathBuf>,
        prior_artifacts: Option<PathBuf>,
    }

    impl Drop for StorageRootOverride {
        fn drop(&mut self) {
            set_test_spillover_root(self.prior_spillover.take());
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(
                self.prior_artifacts.take(),
            );
        }
    }

    // Tests in this module serialize spillover through `TEST_GUARD`; the
    // artifact guard above protects the session-artifact root shared with
    // artifacts.rs tests.
    let prior_spillover =
        set_test_spillover_root(Some(home.join(".codewhale").join(SPILLOVER_DIR_NAME)));
    let prior_artifacts = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
        home.join(".codewhale").join("sessions"),
    ));
    let _restore = StorageRootOverride {
        prior_spillover,
        prior_artifacts,
    };
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn tiny_inline_budgets_keep_the_complete_retrieval_reference() {
        let content = "你好 output\n".repeat(500);
        let path = format!(
            "/very-long-workspace/{}/art_call-1.txt",
            "nested/".repeat(100)
        );
        let reference = "art_12345678-1234-1234-1234-123456789abc";
        let metadata = serde_json::json!({
            "retained_head_bytes": 1000,
            "retained_tail_bytes": 1000,
            "original_byte_count": content.len(),
            "original_line_count": content.lines().count(),
        });
        let preview = format!(
            "{}\n\n{}\n\n…\n{}",
            "h".repeat(1000),
            spillover_preview_footer(content.len() - 2000, 300, &path, Some(reference)),
            "t".repeat(1000),
        );
        for budget in [120, 160, 200, 1200] {
            for fitted in [
                fit_to_inline_budget(&content, budget, Some(&path), Some(reference)),
                refit_spilled_preview(&preview, &metadata, budget, Some(&path), Some(reference))
                    .unwrap(),
            ] {
                assert!(fitted.len() <= budget, "{budget}: {fitted}");
                assert!(fitted.contains(&format!("ref=\"{reference}\"")), "{fitted}");
                assert!(fitted.contains("retrieve_tool_result"), "{fitted}");
                assert!(fitted.contains(SPILLOVER_RECOVERY_HINT), "{fitted}");
            }
        }
        // Even an impossible allowance must not amputate a reference. The
        // marker tells the wire backstop to preserve this complete receipt.
        let fitted = fit_to_inline_budget(&content, 1, Some(&path), Some(reference));
        assert!(fitted.contains(reference));
        assert!(fitted.contains(SPILLOVER_RECOVERY_HINT));
        let unsaved = fit_to_inline_budget(&content, 120, None, None);
        assert!(unsaved.len() <= 120);
        assert!(!unsaved.contains("retrieve_tool_result"));
    }

    #[test]
    fn model_context_archive_reuse_keeps_original_bytes_and_reports_conflicts() {
        let _guard = setup();
        let home = tempdir().unwrap();
        with_test_home(home.path(), || {
            let provider = codewhale_config::ProviderKind::Deepseek;
            let model = "deepseek-v3.2-128k";
            let budget =
                crate::route_budget::route_inline_char_budget_for_route(provider, model, None);
            let original = "a".repeat(budget + 1_000);
            let prior = serde_json::json!({"prior_key": "prior_value"});
            let mut first = ToolResult::success(original.clone()).with_metadata(prior.clone());
            assert!(preserve_full_output_for_model_context(
                &mut first,
                "reused-call",
                "exec_shell",
                "session-inline",
            ));
            let metadata = first.metadata.clone().unwrap();
            let path = PathBuf::from(metadata["artifact_path"].as_str().unwrap());
            assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
            assert_eq!(first.content, original);
            assert_eq!(metadata["prior_key"], prior["prior_key"]);

            // An identical replay still names the original immutable bytes.
            let mut identical = ToolResult::success(original.clone()).with_metadata(prior.clone());
            assert!(preserve_full_output_for_model_context(
                &mut identical,
                "reused-call",
                "exec_shell",
                "session-inline",
            ));
            assert_eq!(identical.metadata.as_ref(), Some(&metadata));

            let changed = "b".repeat(original.len());
            let mut conflict = ToolResult::success(changed.clone()).with_metadata(prior.clone());
            assert!(!preserve_full_output_for_model_context(
                &mut conflict,
                "reused-call",
                "exec_shell",
                "session-inline",
            ));
            assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
            assert_eq!(first.metadata.as_ref(), Some(&metadata));
            assert_eq!(conflict.content, changed);
            assert_eq!(conflict.metadata.as_ref(), Some(&prior));

            // Use the real model-context projection, not a fabricated footer.
            let view = crate::core::engine::compact_tool_result_for_route(
                provider,
                model,
                None,
                "exec_shell",
                &conflict,
            );
            assert!(view.contains("the full output could not be saved"));
            assert!(view.contains("no tool call reaches this copy"));
            assert!(!view.contains("retrieve_tool_result"));
            assert!(!view.contains("art_reused-call"));
        });
    }

    /// Tests in this module serialize through this guard because they mutate
    /// process-global test storage roots. Without it, cargo's parallel runner
    /// would observe interleaved overrides.
    fn setup() -> std::sync::MutexGuard<'static, ()> {
        super::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Run the adaptive evidence lane directly. These cases exercise it
    /// without the `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING` process opt-in so
    /// parallel tests keep a deterministic routing decision.
    fn adaptive_spillover(
        result: &mut ToolResult,
        tool_id: &str,
        tool_name: &str,
        session_id: &str,
    ) -> Option<PathBuf> {
        apply_adaptive_evidence_inner(
            result,
            tool_id,
            ArtifactSpilloverContext {
                tool_name,
                session_id,
            },
        )
    }

    fn assert_unsaved_preview(result: &mut ToolResult, original: &str, success: bool) {
        assert_eq!(result.success, success);
        assert!(result.content.len() < SPILLOVER_HEAD_BYTES + SPILLOVER_TAIL_BYTES + 1024);
        assert!(result.content.starts_with("first 🐳\n"));
        assert!(result.content.ends_with("last 🐳\n"));
        assert!(
            result
                .content
                .contains("the full output could not be saved")
        );
        assert!(
            result
                .content
                .contains("re-run the command with narrower output")
        );
        assert!(!result.content.contains("full output at"));
        assert!(!result.content.contains("retrieve_tool_result"));
        let metadata = result.metadata.as_ref().unwrap();
        assert_eq!(metadata["original_byte_count"], original.len());
        assert_eq!(metadata["evidence_available"], false);
        assert_eq!(metadata["output_persistence_failed"], true);
        assert_eq!(metadata["truncated"], true);
        assert_eq!(metadata["fixture"], "preserved");
        assert_eq!(
            metadata["content_digest"],
            format!(
                "sha256:{}",
                codewhale_runtime::hashing::sha256_hex(original.as_bytes())
            )
        );
        assert!(metadata.get("artifact_id").is_none());
        assert!(metadata.get("spillover_path").is_none());
        assert!(!preserve_full_output_for_model_context(
            result,
            "retry",
            "exec_shell",
            "retry-session"
        ));
    }

    #[test]
    fn classic_spillover_write_failure_bounds_success_and_error_without_fake_evidence() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let root = spillover_root().unwrap();
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(&root, "keep this file").unwrap();
            let original = format!("first 🐳\n{}last 🐳\n", "middle 🐳\n".repeat(20_000));
            for success in [true, false] {
                let mut result = ToolResult::success(original.clone());
                result.success = success;
                result.metadata = Some(serde_json::json!({"fixture": "preserved"}));
                assert!(apply_spillover_inner(&mut result, "call-failed", None, true).is_none());
                assert_unsaved_preview(&mut result, &original, success);
            }
            assert_eq!(fs::read_to_string(root).unwrap(), "keep this file");
        });
    }

    #[test]
    fn adaptive_persistence_failures_bound_output_and_keep_existing_evidence_immutable() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let original = format!("first 🐳\n{}last 🐳\n", "middle 🐳\n".repeat(20_000));
            for failure in [
                "metadata-write",
                "metadata-invalid",
                "metadata-conflict",
                "content-conflict",
            ] {
                for success in [true, false] {
                    let session = format!("{failure}-{success}");
                    let directory = tmp
                        .path()
                        .join(".codewhale/sessions")
                        .join(&session)
                        .join("artifacts");
                    fs::create_dir_all(directory.parent().unwrap()).unwrap();
                    let canary = match failure {
                        "metadata-write" => directory.clone(),
                        "metadata-invalid" | "metadata-conflict" => {
                            directory.join("art_call-failed.evidence.json")
                        }
                        _ => directory.join("art_call-failed.txt"),
                    };
                    if failure != "metadata-write" {
                        fs::create_dir_all(&directory).unwrap();
                    }
                    if failure == "metadata-conflict" {
                        let mut retained =
                            ToolResult::success("retained evidence\n".repeat(10_000));
                        adaptive_spillover(&mut retained, "call-failed", "exec_shell", &session)
                            .unwrap();
                    } else {
                        fs::write(&canary, "keep this file").unwrap();
                    }
                    let canary_before = fs::read(&canary).unwrap();
                    let mut result = ToolResult::success(original.clone());
                    result.success = success;
                    result.metadata = Some(serde_json::json!({
                        "fixture": "preserved",
                        "artifact_id": "stale",
                        "spillover_path": "/stale",
                        "evidence_available": true,
                    }));
                    assert!(
                        adaptive_spillover(&mut result, "call-failed", "exec_shell", &session)
                            .is_none()
                    );
                    assert_unsaved_preview(&mut result, &original, success);
                    assert_eq!(fs::read(&canary).unwrap(), canary_before);
                    if failure == "metadata-conflict" {
                        assert_eq!(
                            fs::read_to_string(directory.join("art_call-failed.txt")).unwrap(),
                            "retained evidence\n".repeat(10_000)
                        );
                    }
                }
            }
            assert!(
                !tmp.path()
                    .join(".codewhale/sessions/retry-session")
                    .exists()
            );
        });
    }

    /// The old hint named `read_file`, which is not registered for the model
    /// at all, and otherwise pointed at routes that only reach the artifact
    /// under trust mode (see [`spillover_recovery_instruction`]), while never
    /// naming `retrieve_tool_result` — model-visible, unconditional, and built
    /// for exactly this.
    #[test]
    fn truncation_footer_names_a_recovery_route_that_works() {
        let footer = spillover_preview_footer(
            4096,
            120,
            "/tmp/artifacts/art_call-1.txt",
            Some("art_call-1"),
        );

        assert!(footer.contains("retrieve_tool_result"), "{footer}");
        assert!(footer.contains("ref=\"art_call-1\""), "{footer}");
        assert!(!footer.contains("read_file"), "{footer}");
        assert!(!footer.contains("sed"), "{footer}");
    }

    /// The legacy global copy has no guaranteed authorization sidecar, so the
    /// footer must not invent a fourth dead route — but it still must not name
    /// the three it used to.
    #[test]
    fn truncation_footer_without_an_artifact_promises_nothing_it_cannot_deliver() {
        let footer = spillover_preview_footer(4096, 120, "/tmp/tool_outputs/call-1.txt", None);

        assert!(
            footer.contains("no tool call reaches this copy"),
            "{footer}"
        );
        assert!(!footer.contains("retrieve_tool_result"), "{footer}");
        assert!(!footer.contains("read_file"), "{footer}");
        assert!(!footer.contains("sed"), "{footer}");
    }

    /// The TUI keys its "this preview was truncated" detection off the shared
    /// constant, so it has to stay a literal substring of every variant.
    #[test]
    fn every_footer_variant_carries_the_ui_detection_marker() {
        for reference in [Some("art_call-1"), None] {
            let footer = spillover_preview_footer(4096, 120, "/tmp/x.txt", reference);
            assert!(footer.contains(SPILLOVER_RECOVERY_HINT), "{footer}");
        }
    }

    #[test]
    fn with_test_home_overrides_storage_roots_without_home_resolution() {
        let _g = setup();
        let tmp = tempdir().unwrap();

        with_test_home(tmp.path(), || {
            assert_eq!(
                spillover_root().as_deref(),
                Some(tmp.path().join(".codewhale").join("tool_outputs").as_path())
            );
            assert_eq!(
                codewhale_runtime::artifacts::session_artifact_absolute_path(
                    "session-123",
                    &PathBuf::from("artifacts").join("art_call-big.txt")
                )
                .as_deref(),
                Some(
                    tmp.path()
                        .join(".codewhale")
                        .join("sessions")
                        .join("session-123")
                        .join("artifacts")
                        .join("art_call-big.txt")
                        .as_path()
                )
            );
        });
    }

    #[test]
    fn sanitise_id_keeps_safe_chars_and_drops_dangerous() {
        assert_eq!(super::sanitise_id("abc-123_x"), Some("abc-123_x".into()));
        // `.` is dropped to keep `..` out of the path.
        assert_eq!(super::sanitise_id("../etc"), Some("etc".into()));
        assert_eq!(super::sanitise_id("/etc/passwd"), Some("etcpasswd".into()));
        // Empty-after-sanitise → None.
        assert!(super::sanitise_id("...").is_none());
        assert!(super::sanitise_id("").is_none());
    }

    #[test]
    fn write_spillover_creates_directory_and_writes_file() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let path = write_spillover("call-abc", "hello world").expect("write");
            assert!(path.exists(), "{path:?} missing");
            let body = fs::read_to_string(&path).unwrap();
            assert_eq!(body, "hello world");
            // Directory landed under `<HOME>/.codewhale/tool_outputs/`.
            // Compare components instead of a substring on `to_string_lossy`
            // — Windows uses `\` as the separator so a `/` substring match
            // would falsely fail there.
            let components: Vec<&str> = path
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect();
            assert!(
                components.contains(&".codewhale") && components.contains(&"tool_outputs"),
                "spillover path missing expected `.codewhale/tool_outputs/...` segments: {path:?}"
            );
        });
    }

    #[test]
    fn write_spillover_rejects_empty_id() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let err = write_spillover("...", "x").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        });
    }

    #[test]
    fn maybe_spillover_returns_none_below_threshold() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let out = maybe_spillover("call-1", "tiny content", 100 * 1024, 4 * 1024).expect("ok");
            assert!(out.is_none());
        });
    }

    #[test]
    fn maybe_spillover_writes_and_returns_head_above_threshold() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // Content larger than the threshold.
            let big = "A".repeat(2_000);
            let (head, path) = maybe_spillover("call-2", &big, 1_000, 256)
                .expect("ok")
                .expect("should have spilled");
            // Head is bounded.
            assert_eq!(head.len(), 256);
            // Full content on disk.
            let body = fs::read_to_string(&path).unwrap();
            assert_eq!(body.len(), 2_000);
        });
    }

    #[test]
    fn maybe_spillover_does_not_split_inside_a_codepoint() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // 4 byte chars; ask for 3 bytes of head → walks back to
            // the previous char boundary (0).
            let s = "🐳🐳🐳🐳"; // 4 × 4-byte codepoints
            assert_eq!(s.len(), 16);
            let (head, _) = maybe_spillover("call-3", s, 1, 3)
                .expect("ok")
                .expect("spilled");
            // 3 isn't a char boundary in this string; walk back → 0.
            assert_eq!(head, "");
            // Asking for 4 bytes lands on the first char boundary.
            let (head, _) = maybe_spillover("call-3b", s, 1, 4)
                .expect("ok")
                .expect("spilled");
            assert_eq!(head, "🐳");
        });
    }

    #[test]
    fn prune_older_than_handles_missing_root() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // Nothing has ever written; root doesn't exist; that's fine.
            let count = prune_older_than(SPILLOVER_MAX_AGE).expect("ok");
            assert_eq!(count, 0);
        });
    }

    // The mtime backdate uses utimensat (Unix-only). On Windows the
    // filetime_set_modified helper is a no-op, so the prune wouldn't see
    // any stale files. Gate the whole test on `cfg(unix)` instead of
    // testing a no-op path that can't fail meaningfully.
    #[test]
    #[cfg(unix)]
    fn prune_older_than_keeps_fresh_files_drops_stale_ones() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let fresh = write_spillover("fresh", "x").unwrap();
            let stale = write_spillover("stale", "y").unwrap();

            // Backdate `stale` to 30 days ago.
            let thirty_days = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
            filetime_set_modified(&stale, thirty_days);

            let pruned = prune_older_than(SPILLOVER_MAX_AGE).unwrap();
            assert_eq!(pruned, 1);
            assert!(fresh.exists());
            assert!(!stale.exists());
        });
    }

    /// Set the mtime on a file. The workspace doesn't pull the
    /// `filetime` crate, so we reach for `utimensat` directly on
    /// Unix. Windows is a no-op — the prune semantics are the same
    /// and the per-cycle stress test lives on the Unix path.
    #[cfg(unix)]
    fn filetime_set_modified(path: &Path, when: SystemTime) {
        let secs = when
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path_c is a valid CString; times is a 2-element array
        // matching utimensat's signature.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, path_c.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(
            rc,
            0,
            "utimensat failed: {}",
            std::io::Error::last_os_error()
        );
    }

    // Windows stub removed in v0.8.8 — the only caller of
    // `filetime_set_modified` is `prune_older_than_keeps_fresh_files_drops_stale_ones`,
    // which is now `#[cfg(unix)]` because mtime backdating requires
    // `utimensat` and a Windows no-op stub can't make the assertion pass
    // anyway. Keeping the stub triggered `-D dead-code` on Windows builds
    // (the prune test was the only caller) and broke `Test (windows-latest)`.

    #[test]
    fn apply_spillover_is_noop_below_threshold() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let mut result = ToolResult::success("small payload");
            let path = apply_spillover(&mut result, "call-small");
            assert!(path.is_none());
            assert_eq!(result.content, "small payload");
            assert!(result.metadata.is_none());
        });
    }

    #[test]
    fn apply_spillover_is_noop_for_error_results() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // Even very large error messages are passed through —
            // truncating an error would hide it from the model.
            let big_err = "boom\n".repeat(50_000);
            let mut result = ToolResult::error(big_err.clone());
            let path = apply_spillover(&mut result, "call-err");
            assert!(path.is_none());
            assert_eq!(result.content, big_err);
        });
    }

    #[test]
    fn apply_spillover_truncates_and_stamps_metadata_above_threshold() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // 200 KiB body — well above the 100 KiB threshold.
            let big = "X".repeat(200 * 1024);
            let mut result = ToolResult::success(big.clone());
            let path = apply_spillover(&mut result, "call-big").expect("should spill");

            // Inline content shrunk to head + honest preview footer.
            assert!(result.content.len() < big.len());
            assert!(
                !result.content.contains(SPILLOVER_PREVIEW_HINT),
                "the tool-details phrase is a UI affordance, not model-facing"
            );
            assert!(
                result.content.contains("of output omitted"),
                "footer missing: {}",
                &result.content[result.content.len().saturating_sub(200)..]
            );
            // The footer tells the model where the full output lives and how
            // to read the omitted range back.
            assert!(result.content.contains("full output at"));
            assert!(result.content.contains(&path.display().to_string()));
            assert!(result.content.contains(SPILLOVER_RECOVERY_HINT));
            assert!(
                !result.content.contains("retrieve_tool_result"),
                "legacy spillover ownership can fail to publish; promising \
                 retrieval here would be another dead route"
            );
            assert!(!result.content.contains("read_file"));
            assert!(!result.content.contains("sed"));

            // Full bytes are on disk at the returned path.
            assert!(path.exists(), "spillover file missing: {path:?}");
            let body = fs::read_to_string(&path).unwrap();
            assert_eq!(body.len(), 200 * 1024);

            // metadata.spillover_path stamped for the UI to find.
            let metadata = result.metadata.expect("metadata stamped");
            let stamped = metadata
                .get("spillover_path")
                .and_then(serde_json::Value::as_str)
                .expect("spillover_path key present");
            assert_eq!(stamped, path.display().to_string());
            assert_eq!(metadata["truncated"], true);
            assert_eq!(metadata["original_byte_count"], 200 * 1024);
            assert_eq!(metadata["retained_head_bytes"], SPILLOVER_HEAD_BYTES);
            assert_eq!(metadata["retained_tail_bytes"], SPILLOVER_TAIL_BYTES);
            assert!(
                metadata["content_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
            );
        });
    }

    #[test]
    fn apply_spillover_with_artifact_writes_session_file_and_plain_preview() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let big = "checking crate ... error[E0425]: cannot find value\n".repeat(4_000);
            let mut result = ToolResult::success(big.clone());
            let path = adaptive_spillover(&mut result, "call-big", "exec_shell", "session-123")
                .expect("should spill");

            let session_artifact = tmp
                .path()
                .join(".codewhale")
                .join("sessions")
                .join("session-123")
                .join("artifacts")
                .join("art_call-big.txt");
            assert_eq!(path, session_artifact);
            assert_eq!(fs::read_to_string(&session_artifact).unwrap(), big);
            assert!(
                !tmp.path()
                    .join(".codewhale/tool_outputs/call-big.txt")
                    .exists(),
                "adaptive evidence stores one exact origin-session copy"
            );
            // The model sees a bounded preview with an honest footer: the
            // artifact path plus the retrieval call that actually resolves it.
            assert!(!result.content.contains(SPILLOVER_PREVIEW_HINT));
            assert!(result.content.contains("\n…\n"));
            assert!(result.content.contains("of output omitted"));
            assert!(result.content.contains("full output at"));
            assert!(result.content.contains(SPILLOVER_RECOVERY_HINT));
            assert!(
                result.content.contains("art_call-big.txt"),
                "footer must name the artifact path so the model can recover the output"
            );
            assert!(!result.content.contains("Exact evidence retained"));
            assert!(
                result.content.contains("retrieve_tool_result"),
                "a session artifact is retrievable; the footer must say so: {}",
                result.content
            );
            assert!(
                result.content.contains("ref=\"art_call-big\""),
                "the footer must hand over a ref that resolves: {}",
                result.content
            );
            assert!(
                session_artifact
                    .with_file_name("art_call-big.evidence.json")
                    .exists()
            );

            let metadata = result.metadata.expect("metadata stamped");
            assert_eq!(
                metadata
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str),
                Some("art_call-big")
            );
            assert_eq!(
                metadata
                    .get("artifact_relative_path")
                    .and_then(serde_json::Value::as_str),
                Some("artifacts/art_call-big.txt")
            );
            assert_eq!(
                metadata
                    .get("artifact_session_id")
                    .and_then(serde_json::Value::as_str),
                Some("session-123")
            );
            assert_eq!(metadata["original_byte_count"], big.len());
            assert!(metadata["retained_head_bytes"].as_u64().unwrap_or(0) <= 16 * 1024);
            assert!(metadata["retained_tail_bytes"].as_u64().unwrap_or(0) <= 4 * 1024);
        });
    }

    #[test]
    fn adaptive_evidence_keeps_success_and_failure_exact_distinct_and_out_of_context() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let sentinel = "DEEP_RAW_SENTINEL";
            // Payloads must exceed the 32_768-token (≈96 KiB) handle-only
            // threshold so adaptive routing actually spills them.
            let success_raw = format!(
                "{}{}{}",
                "head\n".repeat(30_000),
                sentinel,
                "tail\n".repeat(30_000)
            );
            let failure_raw = format!("{}{}", "failure\n".repeat(30_000), "FAILURE_END");
            let mut success = ToolResult::success(success_raw.clone());
            let mut failure = ToolResult::error(failure_raw.clone());

            let success_path =
                adaptive_spillover(&mut success, "call-success", "exec_shell", "session-a")
                    .expect("success evidence");
            let failure_path =
                adaptive_spillover(&mut failure, "call-failure", "mcp_fixture", "session-a")
                    .expect("failure evidence");

            assert_ne!(success_path, failure_path);
            assert_eq!(
                std::fs::read(&success_path).unwrap(),
                success_raw.as_bytes()
            );
            assert_eq!(
                std::fs::read(&failure_path).unwrap(),
                failure_raw.as_bytes()
            );
            assert!(!success.content.contains(sentinel));
            // Handle-only preview: 16 KiB head + 4 KiB tail + footer.
            assert!(success.content.len() < 21 * 1024);
            let success_meta = success.metadata.as_ref().unwrap();
            let failure_meta = failure.metadata.as_ref().unwrap();
            assert_ne!(
                success_meta["artifact_digest"],
                failure_meta["artifact_digest"]
            );
            assert_eq!(success_meta["artifact_session_id"], "session-a");
            assert_eq!(failure_meta["artifact_session_id"], "session-a");

            let mut replay = ToolResult::success(success_raw);
            let replay_path =
                adaptive_spillover(&mut replay, "call-success", "exec_shell", "session-a")
                    .expect("idempotent replay");
            assert_eq!(replay_path, success_path);
        });
    }

    #[test]
    fn adaptive_evidence_publication_failure_emits_no_handle_or_details_hint() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let session_dir = tmp
                .path()
                .join(".codewhale")
                .join("sessions")
                .join("session-blocked");
            std::fs::create_dir_all(&session_dir).unwrap();
            std::fs::write(session_dir.join("artifacts"), b"block artifact directory").unwrap();

            let raw = format!(
                "{}{}{}",
                "publication failure head\n".repeat(1_500),
                "DEEP_FAILURE_SENTINEL",
                "publication failure tail\n".repeat(1_500),
            );
            let mut result = ToolResult::error(raw.clone());
            let path = adaptive_spillover(
                &mut result,
                "call-failed-publish",
                "mcp_fixture",
                "session-blocked",
            );

            assert!(path.is_none());
            assert!(!result.success);
            assert!(result.content.len() < SPILLOVER_HEAD_BYTES + SPILLOVER_TAIL_BYTES + 1024);
            assert!(!result.content.contains("DEEP_FAILURE_SENTINEL"));
            assert!(
                result
                    .content
                    .contains("the full output could not be saved")
            );
            assert!(!result.content.contains(SPILLOVER_PREVIEW_HINT));
            assert!(!result.content.contains("retrieve_tool_result"));
            assert_eq!(
                result
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("evidence_available")),
                Some(&serde_json::Value::Bool(false))
            );
            assert!(
                !session_dir
                    .join("artifacts/art_call-failed-publish.txt")
                    .exists()
            );
        });
    }

    #[test]
    fn adaptive_evidence_metadata_atomic_failure_leaves_payload_unadvertised() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let artifact_dir = tmp
                .path()
                .join(".codewhale")
                .join("sessions")
                .join("session-metadata-blocked")
                .join("artifacts");
            std::fs::create_dir_all(artifact_dir.join("art_call-failed-metadata.evidence.json"))
                .unwrap();

            let raw = format!(
                "{}{}{}",
                "metadata failure head\n".repeat(1_500),
                "DEEP_METADATA_FAILURE_SENTINEL",
                "metadata failure tail\n".repeat(1_500),
            );
            let mut result = ToolResult::success(raw.clone());
            let path = adaptive_spillover(
                &mut result,
                "call-failed-metadata",
                "exec_shell",
                "session-metadata-blocked",
            );

            assert!(path.is_none());
            assert!(result.success);
            assert!(result.content.len() < SPILLOVER_HEAD_BYTES + SPILLOVER_TAIL_BYTES + 1024);
            assert!(!result.content.contains("DEEP_METADATA_FAILURE_SENTINEL"));
            assert!(
                result
                    .content
                    .contains("the full output could not be saved")
            );
            assert_eq!(
                result.metadata.as_ref().unwrap()["evidence_available"],
                false
            );
            assert!(!result.content.contains(SPILLOVER_PREVIEW_HINT));
            assert!(!result.content.contains("retrieve_tool_result"));
            assert!(
                !artifact_dir.join("art_call-failed-metadata.txt").exists(),
                "metadata failure must leave no payload behind a guessable handle"
            );
        });
    }

    #[test]
    fn registry_results_spill_to_artifacts_like_every_other_tool() {
        // The Registry bypass is gone: an oversized payload (which today can
        // only be a bug, since the tool caps model-visible matches at eight)
        // takes the same artifact path as any other tool result.
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let original = "registry-entry\n".repeat(10_000);
            assert!(original.len() > SPILLOVER_THRESHOLD_BYTES);
            let mut result = ToolResult::success(original);

            let path = apply_spillover_with_artifact(
                &mut result,
                "call-registry",
                "registry_sync",
                "session-registry",
            );

            assert!(path.is_some(), "oversized registry payload must spill");
        });
    }

    #[test]
    fn apply_spillover_preserves_existing_metadata() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let big = "Y".repeat(200 * 1024);
            let mut result = ToolResult::success(big)
                .with_metadata(serde_json::json!({"prior_key": "prior_value"}));
            let path = apply_spillover(&mut result, "call-meta").expect("should spill");

            let metadata = result.metadata.expect("metadata present");
            // Prior keys survive.
            assert_eq!(
                metadata
                    .get("prior_key")
                    .and_then(serde_json::Value::as_str),
                Some("prior_value")
            );
            // New key added alongside.
            assert_eq!(
                metadata
                    .get("spillover_path")
                    .and_then(serde_json::Value::as_str),
                Some(path.display().to_string().as_str())
            );
        });
    }

    #[test]
    fn apply_spillover_wraps_non_object_metadata_under_prior_key() {
        // Defends against a tool whose `metadata` is something
        // other than a JSON object (rare — most use the `json!({})`
        // pattern — but legal per `serde_json::Value`). The
        // spillover writer must add `spillover_path` without losing
        // the prior payload.
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let big = "Z".repeat(200 * 1024);
            let mut result = ToolResult::success(big).with_metadata(serde_json::json!([
                "unexpected",
                "array",
                "payload"
            ]));
            let path = apply_spillover(&mut result, "call-arr").expect("should spill");

            let metadata = result.metadata.expect("metadata stamped");
            // Prior payload re-homed under `_prior`.
            let prior = metadata.get("_prior").expect("_prior wrap key present");
            assert_eq!(
                prior,
                &serde_json::json!(["unexpected", "array", "payload"]),
                "prior array should round-trip under _prior"
            );
            // New key alongside.
            assert_eq!(
                metadata
                    .get("spillover_path")
                    .and_then(serde_json::Value::as_str),
                Some(path.display().to_string().as_str())
            );
        });
    }

    // ── Honest-truncation regressions (v0.9.4) ─────────────────────────────

    #[test]
    fn truncated_preview_returns_content_unchanged_when_nothing_omitted() {
        let original = "line one\nline two\nline three\n";
        let preview = truncated_preview(original, "", original, "/tmp/artifact.txt", None);
        assert_eq!(preview, original);
        assert!(
            !preview.contains("of output omitted"),
            "must never claim a truncation that did not happen"
        );
    }

    #[test]
    fn head_tail_windows_never_overlap() {
        // Content smaller than head + tail budgets: the tail window shrinks
        // so it starts exactly where the head ends — no byte appears twice.
        let content = "x".repeat(10_000);
        let (head, tail) = head_tail_windows(&content, 8 * 1024, 4 * 1024);
        assert_eq!(head.len(), 8 * 1024);
        assert_eq!(tail.len(), 10_000 - 8 * 1024);
        assert!(head.len() + tail.len() <= content.len());

        // Content larger than both budgets: full windows, exact omission.
        let big = "y".repeat(100_000);
        let (head, tail) = head_tail_windows(&big, 32 * 1024, 8 * 1024);
        assert_eq!(head.len(), 32 * 1024);
        assert_eq!(tail.len(), 8 * 1024);

        // UTF-8 codepoints are never split at either window edge.
        let emoji = "🐳".repeat(5_000); // 20_000 bytes, 4 per codepoint
        let (head, tail) = head_tail_windows(&emoji, 8 * 1024 + 1, 4 * 1024 + 2);
        assert!(emoji.is_char_boundary(head.len()));
        assert!(emoji.is_char_boundary(emoji.len() - tail.len()));
        assert!(head.len() + tail.len() <= emoji.len());
    }

    #[test]
    fn adaptive_evidence_passes_through_when_preview_budget_covers_output() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // 30_000 bytes → 10_000 estimated tokens → Hybrid band under the
            // 32_768-token default, but the 32 KiB + 8 KiB preview budget
            // covers the whole output, so nothing is actually omitted.
            let raw = "mid\n".repeat(7_500);
            assert_eq!(raw.len(), 30_000);
            let mut result = ToolResult::success(raw.clone());
            let path =
                adaptive_spillover(&mut result, "call-covered", "exec_shell", "session-covered");
            assert!(path.is_none(), "no artifact when nothing is omitted");
            assert_eq!(result.content, raw);
            assert!(!result.content.contains("of output omitted"));
            assert!(
                !tmp.path()
                    .join(".codewhale/sessions/session-covered/artifacts/art_call-covered.txt")
                    .exists()
            );
        });
    }

    #[test]
    fn adaptive_evidence_footer_names_artifact_path_and_recovery() {
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            // 120_000 bytes → 40_000 estimated tokens → handle-only band.
            let raw = "entry\n".repeat(20_000);
            assert_eq!(raw.len(), 120_000);
            let mut result = ToolResult::success(raw);
            let path =
                adaptive_spillover(&mut result, "call-honest", "exec_shell", "session-honest")
                    .expect("should spill");

            // Footer: omitted size + line count, artifact path, recovery line.
            assert!(result.content.contains("of output omitted ("));
            assert!(result.content.contains(" lines)"));
            assert!(result.content.contains("full output at"));
            assert!(
                result
                    .content
                    .contains(&codewhale_runtime::artifacts::format_artifact_relative_path(&path))
            );
            assert!(result.content.contains(SPILLOVER_RECOVERY_HINT));
            assert!(!result.content.contains(SPILLOVER_PREVIEW_HINT));

            // Head and tail do not overlap: 16 KiB + 4 KiB handle-only
            // windows over a 120_000-byte output.
            let metadata = result.metadata.expect("metadata stamped");
            assert_eq!(metadata["retained_head_bytes"], 16 * 1024);
            assert_eq!(metadata["retained_tail_bytes"], 4 * 1024);
        });
    }

    #[test]
    fn apply_spillover_with_artifact_defaults_to_classic_head_tail_spillover() {
        // 120_000 bytes exceeds the 100 KiB classic threshold. Without the
        // `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING` opt-in the public entry point
        // keeps a 32 KiB head + 8 KiB tail and writes the session artifact —
        // the adaptive handle-only windows and evidence metadata are opt-in.
        let _g = setup();
        let tmp = tempdir().unwrap();
        with_test_home(tmp.path(), || {
            let raw = "entry\n".repeat(20_000);
            assert_eq!(raw.len(), 120_000);
            let mut result = ToolResult::success(raw.clone());
            let path = apply_spillover_with_artifact(
                &mut result,
                "call-classic",
                "exec_shell",
                "session-classic",
            )
            .expect("should spill");

            let metadata = result.metadata.expect("metadata stamped");
            assert_eq!(metadata["retained_head_bytes"], 32 * 1024);
            assert_eq!(metadata["retained_tail_bytes"], 8 * 1024);
            assert!(result.content.contains(SPILLOVER_RECOVERY_HINT));
            assert!(
                !tmp.path()
                    .join(
                        ".codewhale/sessions/session-classic/artifacts/art_call-classic.evidence.json"
                    )
                    .exists(),
                "classic lane publishes no adaptive evidence metadata"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            // The classic lane names its bytes by digest, like the adaptive
            // lane, so a turn artifact ref can carry a revision.
            assert_eq!(
                metadata["artifact_digest"],
                codewhale_runtime::hashing::sha256_hex(raw.as_bytes())
            );
            let artifact = tmp
                .path()
                .join(".codewhale/sessions/session-classic/artifacts/art_call-classic.txt");
            assert_eq!(std::fs::read_to_string(&artifact).unwrap(), raw);

            // The published handle is immutable: a different payload under
            // the same call id never rewrites the bytes a ref names.
            let mut replay = ToolResult::success("other\n".repeat(20_000));
            apply_spillover_with_artifact(
                &mut replay,
                "call-classic",
                "exec_shell",
                "session-classic",
            )
            .expect("replay still spills to the legacy footer");
            assert_eq!(std::fs::read_to_string(&artifact).unwrap(), raw);
            let replay_metadata = replay.metadata.expect("metadata stamped");
            assert!(replay_metadata.get("artifact_id").is_none());
            assert!(replay_metadata.get("artifact_digest").is_none());
        });
    }
}

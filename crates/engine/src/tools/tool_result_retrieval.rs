//! `retrieve_tool_result` - selective retrieval for spilled tool outputs.
//!
//! Exact tool evidence is retained under its origin session. Historical
//! payloads in the global `tool_outputs/` compatibility directory are readable
//! only when a digest-bound ownership sidecar proves they belong to the active
//! session.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str, optional_u64,
    required_str,
};

const DEFAULT_MAX_BYTES: usize = 8 * 1024;
const HARD_MAX_BYTES: usize = 128 * 1024;
const DEFAULT_LINE_COUNT: usize = 40;
const HARD_LINE_COUNT: usize = 500;
const DEFAULT_MAX_MATCHES: usize = 20;
const HARD_MAX_MATCHES: usize = 100;
const DEFAULT_CONTEXT_LINES: usize = 1;
const HARD_CONTEXT_LINES: usize = 5;
const MAX_SCAN_BYTES: u64 = 512 * 1024 * 1024;
const WRAP_LINE_BYTES: usize = 2 * 1024;
const MAX_JSON_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SELECTION_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_REFERENCE_BYTES: usize = 32 * 1024;

/// Retrieve summaries or slices of a prior spilled tool result.
pub struct RetrieveToolResultTool;

#[async_trait]
impl ToolSpec for RetrieveToolResultTool {
    fn name(&self) -> &'static str {
        "retrieve_tool_result"
    }

    fn description(&self) -> &'static str {
        "Inspect retained tool evidence with strict session ownership and bounds. Accepts an artifact id, validated session-relative path, or an ownership-proven legacy call/SHA reference. Unowned legacy-global evidence fails closed. Modes: metadata, summary, head, tail, lines, query, json, bytes. Lines longer than 2 KiB are split into numbered segments. json parses the evidence (JSON text inside it, such as an MCP text block, included) and returns the `jsonpath` selection, cut to max_bytes as valid JSON. bytes returns a bounded base64 slice for exact text or binary recovery."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Session-owned artifact id (`art_<id>`) or validated artifact-relative path. Legacy call-id/SHA references work only when origin-session ownership was recorded."
                },
                "mode": {
                    "type": "string",
                    "enum": ["metadata", "summary", "head", "tail", "lines", "query", "json", "bytes"],
                    "description": "Retrieval mode. Defaults to summary."
                },
                "query": {
                    "type": "string",
                    "description": "Case-insensitive substring to search for when mode=query."
                },
                "jsonpath": {
                    "type": "string",
                    "description": "JSONPath subset for mode=json: $, .field, [index], [*], and ['field']. Defaults to $."
                },
                "lines": {
                    "type": "string",
                    "description": "Line selector for mode=lines, e.g. \"10\" or \"10-40\"."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based first line for mode=lines."
                },
                "end_line": {
                    "type": "integer",
                    "description": "1-based final line for mode=lines."
                },
                "line_count": {
                    "type": "integer",
                    "description": "Number of lines for head/tail modes. Default 40, hard cap 500."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum bytes of excerpt text returned. Default 8192, hard cap 131072."
                },
                "max_matches": {
                    "type": "integer",
                    "description": "Maximum query matches or signal lines returned. Default 20, hard cap 100."
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Extra lines around each query match. Default 1, hard cap 5."
                },
                "generation": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional expected evidence generation; mismatches fail closed."
                },
                "expected_sha256": {
                    "type": "string",
                    "description": "Expected SHA256 from an earlier verified window; a changed snapshot is refused."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based byte offset for mode=bytes."
                },
                "length": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Byte count for mode=bytes, capped at max_bytes."
                }
            },
            "required": ["ref"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let reference = required_str(&input, "ref")?;
        if reference.len() > MAX_REFERENCE_BYTES {
            return Err(ToolError::invalid_input(
                "ref exceeds the 32768-byte inspection limit",
            ));
        }
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(ToolError::invalid_input("ref cannot be empty"));
        }

        let mode = optional_str(&input, "mode")?.unwrap_or("summary");
        if mode.len() > 16 {
            return Err(ToolError::invalid_input(
                "mode exceeds the inspection limit",
            ));
        }
        let mode = mode.trim().to_ascii_lowercase();
        if !matches!(
            mode.as_str(),
            "metadata" | "summary" | "head" | "tail" | "lines" | "query" | "json" | "bytes"
        ) {
            return Err(ToolError::invalid_input(format!(
                "unsupported mode `{mode}` (expected metadata, summary, head, tail, lines, query, json, or bytes)"
            )));
        }
        let max_bytes = clamp_u64(
            optional_u64(&input, "max_bytes", DEFAULT_MAX_BYTES as u64)?,
            1,
            HARD_MAX_BYTES,
        );
        let expected_digest = optional_str(&input, "expected_sha256")?
            .map(|digest| {
                if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(ToolError::invalid_input(
                        "expected_sha256 must be a SHA256 digest",
                    ));
                }
                Ok(digest.to_ascii_lowercase())
            })
            .transpose()?;
        let jsonpath = optional_str(&input, "jsonpath")?
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or("$")
            .to_string();
        if jsonpath.len() > crate::tools::handle::JSONPATH_MAX_PATH_BYTES {
            return Err(ToolError::invalid_input(
                "jsonpath exceeds the 4096-byte inspection limit",
            ));
        }
        let inspection = if matches!(mode.as_str(), "metadata" | "bytes" | "json") {
            None
        } else {
            Some(TextInspection::new(&mode, &input, max_bytes)?)
        };
        let owned_reference = reference.to_string();
        let session_id = context.state_namespace.clone();
        let (resolved, legacy_ownership, file, initial_size) =
            evidence_owner_work(context, move |cancel| {
                let resolved = resolve_spillover_reference(&owned_reference, &session_id)?;
                check_evidence_cancelled(cancel)?;
                let legacy_ownership = if resolved.kind == ResolvedReferenceKind::LegacyGlobal {
                    Some(authorize_legacy_spillover(&resolved.path, &session_id)?)
                } else {
                    None
                };
                check_evidence_cancelled(cancel)?;
                let relative = resolved.path.strip_prefix(&resolved.root).map_err(|_| {
                    ToolError::permission_denied("evidence path is not owned by the active session")
                })?;
                let file = match resolved.kind {
                    ResolvedReferenceKind::ActiveSession => {
                        codewhale_runtime::artifacts::open_session_relative(
                            &session_id,
                            &std::path::Path::new(codewhale_runtime::artifacts::ARTIFACTS_DIR_NAME)
                                .join(relative),
                            false,
                        )
                    }
                    ResolvedReferenceKind::LegacyGlobal => {
                        codewhale_util::files::WorkspaceFile::open_delivery(
                            &resolved.root,
                            relative,
                        )
                    }
                }
                .and_then(|file| file.open_file())
                .map_err(|_| {
                    ToolError::execution_failed("evidence is missing or no longer retained")
                })?;
                check_evidence_cancelled(cancel)?;
                let initial_size = file
                    .metadata()
                    .map_err(|_| {
                        ToolError::execution_failed("evidence is missing or no longer retained")
                    })?
                    .len();
                Ok((resolved, legacy_ownership, file, initial_size))
            })
            .await?;
        let snapshot = stream_evidence(
            file,
            initial_size,
            &input,
            &mode,
            max_bytes,
            inspection,
            context,
        )
        .await?;
        if expected_digest
            .as_ref()
            .is_some_and(|digest| digest != &snapshot.digest)
        {
            return Err(ToolError::execution_failed(
                "evidence changed since the previous window",
            ));
        }
        if let Some(ownership) = legacy_ownership
            && (ownership.size_bytes != snapshot.size_bytes || ownership.digest != snapshot.digest)
        {
            return Err(ToolError::execution_failed(
                "legacy evidence content is corrupt",
            ));
        }
        let owned_reference = reference.to_string();
        let session_id = context.state_namespace.clone();
        let size_bytes = snapshot.size_bytes;
        let digest = snapshot.digest.clone();
        let generation = input.get("generation").and_then(Value::as_u64);
        let evidence = evidence_owner_work(context, move |_| {
            validate_evidence_if_present(
                &owned_reference,
                &resolved.path,
                size_bytes,
                &digest,
                &session_id,
                generation,
            )
        })
        .await?;
        if mode == "metadata" {
            return ToolResult::json(&json!({
                "ref": reference,
                "available": true,
                "total_bytes": snapshot.size_bytes,
                "sha256": snapshot.digest,
                "evidence": evidence,
            }))
            .map_err(|err| ToolError::execution_failed(err.to_string()));
        }
        if mode == "bytes" {
            use base64::Engine as _;
            let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let slice = &snapshot.byte_slice;
            return ToolResult::json(&json!({
                "ref": reference,
                "mode": "bytes",
                "offset": offset,
                "returned_bytes": slice.len(),
                "total_bytes": snapshot.size_bytes,
                "encoding": "base64",
                "sha256": snapshot.digest,
                "data": base64::engine::general_purpose::STANDARD.encode(slice),
            }))
            .map_err(|err| ToolError::execution_failed(err.to_string()));
        }
        if mode == "json" {
            let bytes = snapshot.byte_slice;
            let total_bytes = snapshot.size_bytes;
            let path = jsonpath.clone();
            let owned_reference = reference.to_string();
            let payload = evidence_owner_work(context, move |cancel| {
                check_evidence_cancelled(cancel)?;
                let (selection, match_cap_hit) = select_json(&bytes, &path)?;
                check_evidence_cancelled(cancel)?;
                let matches = selection.len();
                let (values, omitted) =
                    crate::tools::truncate::shrink_json(Value::Array(selection), max_bytes);
                let truncated = omitted > 0 || match_cap_hit;
                let mut payload = json!({
                    "ref": owned_reference,
                    "mode": "json",
                    "jsonpath": path,
                    "total_bytes": total_bytes,
                    "matches": matches,
                    "values": values,
                    "truncated": truncated,
                });
                if truncated {
                    payload["hint"] = json!(
                        "values keep their leading elements; narrow jsonpath (for example $.items[10] or $.items[*].id) or raise max_bytes (up to 131072)"
                    );
                }
                Ok(payload)
            })
            .await?;
            return ToolResult::json(&payload)
                .map_err(|err| ToolError::execution_failed(err.to_string()));
        }
        let inspection = snapshot
            .inspection?
            .ok_or_else(|| ToolError::execution_failed("text inspection was unavailable"))?;
        let payload = inspection.payload(reference, snapshot.size_bytes);

        ToolResult::json(&payload).map_err(|err| {
            ToolError::execution_failed(format!("failed to serialize result: {err}"))
        })
    }
}

async fn evidence_owner_work<T: Send + 'static>(
    context: &ToolContext,
    work: impl FnOnce(&tokio_util::sync::CancellationToken) -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    let cancel = context
        .cancel_token
        .as_ref()
        .map(tokio_util::sync::CancellationToken::child_token)
        .unwrap_or_default();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let worker_cancel = cancel.clone();
    #[cfg(any(test, feature = "test-support"))]
    let ticket = crate::test_support::env_scope_ticket();
    let worker = codewhale_runtime::runtime_control::owner_work(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = crate::test_support::join_env_scope(ticket);
        let result = (|| {
            check_evidence_cancelled(&worker_cancel)?;
            let result = work(&worker_cancel);
            check_evidence_cancelled(&worker_cancel)?;
            result
        })();
        Ok(result)
    });
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ToolError::cancelled("Evidence inspection cancelled")),
        result = worker => result.map_err(|_| ToolError::execution_failed("evidence inspection preparation failed"))?,
    }
}

fn check_evidence_cancelled(cancel: &tokio_util::sync::CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        return Err(ToolError::cancelled("Evidence inspection cancelled"));
    }
    Ok(())
}

fn validate_evidence_if_present(
    reference: &str,
    path: &std::path::Path,
    size_bytes: u64,
    digest: &str,
    session_id: &str,
    generation: Option<u64>,
) -> Result<Option<crate::tools::large_output_router::EvidenceArtifact>, ToolError> {
    let handle = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| stem.starts_with("art_"))
        .or_else(|| {
            reference
                .trim()
                .starts_with("art_")
                .then(|| reference.trim())
        });
    let Some(handle) = handle else {
        return Ok(None);
    };
    let metadata =
        match crate::tools::large_output_router::read_evidence_metadata(session_id, handle) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(ToolError::permission_denied(
                    "evidence belongs to another session",
                ));
            }
            Err(_) => return Err(ToolError::execution_failed("evidence metadata is corrupt")),
        };
    if metadata.origin_session != session_id || metadata.handle != handle {
        return Err(ToolError::permission_denied(
            "evidence belongs to another session",
        ));
    }
    if metadata.redacted {
        return Err(ToolError::permission_denied("evidence has been redacted"));
    }
    if crate::tools::large_output_router::evidence_is_expired(
        &metadata,
        crate::tools::large_output_router::unix_millis_now(),
    ) {
        return Err(ToolError::execution_failed(
            "evidence retention has expired",
        ));
    }
    if generation.is_some_and(|generation| generation != u64::from(metadata.generation)) {
        return Err(ToolError::execution_failed(
            "evidence generation does not match",
        ));
    }
    if metadata.size_bytes != size_bytes || metadata.digest != digest {
        return Err(ToolError::execution_failed("evidence content is corrupt"));
    }
    Ok(Some(metadata))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedReferenceKind {
    ActiveSession,
    LegacyGlobal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSpilloverReference {
    path: PathBuf,
    root: PathBuf,
    kind: ResolvedReferenceKind,
}

fn authorize_legacy_spillover(
    path: &std::path::Path,
    session_id: &str,
) -> Result<crate::tools::truncate::LegacySpilloverOwnership, ToolError> {
    if session_id.trim().is_empty() {
        return Err(ToolError::permission_denied(
            "legacy evidence has no verifiable session owner",
        ));
    }
    let ownership = match crate::tools::truncate::read_legacy_spillover_ownership(path) {
        Ok(ownership) => ownership,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::permission_denied(
                "legacy evidence has no verifiable session owner",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ToolError::permission_denied(
                "legacy evidence ownership proof is invalid",
            ));
        }
        Err(_) => {
            return Err(ToolError::execution_failed(
                "legacy evidence ownership metadata is corrupt",
            ));
        }
    };
    if ownership.origin_session != session_id {
        return Err(ToolError::permission_denied(
            "legacy evidence belongs to another session",
        ));
    }
    Ok(ownership)
}

/// Resolve a tool-result ref without weakening its ownership boundary.
///
/// Current-session artifacts always win over same-named global compatibility
/// files. A legacy call-id, SHA, relative path, or absolute path can resolve,
/// but the caller must validate its ownership sidecar before reading bytes.
fn resolve_spillover_reference(
    reference: &str,
    session_id: &str,
) -> Result<ResolvedSpilloverReference, ToolError> {
    let root = crate::tools::truncate::spillover_root()
        .ok_or_else(|| ToolError::execution_failed("retained evidence storage is unavailable"))?;
    let root_canonical = root.canonicalize().ok();

    // Resolve the session's `artifacts/` directory.
    // `session_artifact_absolute_path(sid, p)` returns
    // `~/.codewhale/sessions/<sid>/<p>` — so passing the literal
    // `ARTIFACTS_DIR_NAME` ("artifacts") gets us the real artifacts
    // root. An earlier draft passed `Path::new(".")` and took
    // `.parent()`, which landed one directory too high (`<sid>` instead
    // of `<sid>/artifacts`) and silently broke every bare `art_<id>`
    // ref — only the legacy-spillover fallback survived. The test
    // `resolves_art_prefix_to_legacy_spillover_id` masked it because
    // it ONLY wrote a legacy spillover file. The new test
    // `resolves_art_prefix_via_session_artifacts` exercises the real
    // path.
    let session_artifacts_root = if !session_id.is_empty() {
        codewhale_runtime::artifacts::session_artifact_absolute_path(
            session_id,
            std::path::Path::new(codewhale_runtime::artifacts::ARTIFACTS_DIR_NAME),
        )
    } else {
        None
    };
    let session_artifacts_root_canonical = session_artifacts_root
        .as_ref()
        .and_then(|p| p.canonicalize().ok());

    let trimmed = reference.trim();
    let stripped = trimmed
        .strip_prefix("tool_result:")
        .unwrap_or(trimmed)
        .trim();

    let mut tried = 0_usize;
    let try_path = |candidate: PathBuf, tried: &mut usize| -> Option<ResolvedSpilloverReference> {
        *tried = (*tried).saturating_add(1);

        // Reject symlinks at the leaf BEFORE canonicalizing so an
        // attacker who can write under `<sid>/artifacts/` cannot
        // plant a symlink to `/etc/passwd` and read it back through
        // `retrieve_tool_result`. canonicalize() would happily
        // follow such a link and then pass the `starts_with(root)`
        // check because of the resolved-then-compare order. Both session and
        // compatibility roots reject leaf symlinks. Legacy
        // compatibility files also need a separate ownership sidecar below.
        if let Ok(meta) = std::fs::symlink_metadata(&candidate)
            && meta.file_type().is_symlink()
        {
            return None;
        }

        let canonical = candidate.canonicalize().ok()?;
        if !canonical.is_file() {
            return None;
        }
        let inside_legacy = root_canonical
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root));
        let inside_session = session_artifacts_root_canonical
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root));
        if inside_session {
            Some(ResolvedSpilloverReference {
                path: canonical,
                root: session_artifacts_root_canonical.as_ref()?.clone(),
                kind: ResolvedReferenceKind::ActiveSession,
            })
        } else if inside_legacy {
            Some(ResolvedSpilloverReference {
                path: canonical,
                root: root_canonical.as_ref()?.clone(),
                kind: ResolvedReferenceKind::LegacyGlobal,
            })
        } else {
            None
        }
    };

    // Form 1/3: absolute path. Validate it lives under one of the allowed roots.
    let raw_path = PathBuf::from(stripped);
    if raw_path.is_absolute() {
        if let Some(found) = try_path(raw_path, &mut tried) {
            return Ok(found);
        }
        return Err(ToolError::permission_denied(
            "evidence path is not owned by the active session",
        ));
    }

    // Session artifact paths take priority over legacy-global lookups.
    let looks_like_path = stripped.ends_with(".txt")
        || stripped.contains('/')
        || (std::path::MAIN_SEPARATOR != '/' && stripped.contains(std::path::MAIN_SEPARATOR));
    if looks_like_path {
        if let Some(sa_root) = session_artifacts_root.as_ref() {
            let rel = stripped.strip_prefix("artifacts/").unwrap_or(stripped);
            if let Some(found) = try_path(sa_root.join(rel), &mut tried) {
                return Ok(found);
            }
        }
        if let Some(found) = try_path(root.join(stripped), &mut tried) {
            return Ok(found);
        }
        return Err(not_found(reference, tried));
    }

    if let Some(sa_root) = session_artifacts_root.as_ref() {
        let file_name = if stripped.starts_with("art_") {
            format!("{stripped}.txt")
        } else {
            format!("art_{stripped}.txt")
        };
        if let Some(found) = try_path(sa_root.join(file_name), &mut tried) {
            return Ok(found);
        }
    }

    // `sha:<hex>` or bare 64-hex resolves only as legacy-global evidence and
    // therefore still requires an ownership sidecar in the caller.
    let sha_candidate = stripped
        .strip_prefix("sha:")
        .or_else(|| stripped.strip_prefix("sha_"))
        .unwrap_or(stripped)
        .trim();
    if crate::tools::truncate::is_valid_sha256(&sha_candidate.to_ascii_lowercase())
        && let Some(p) = crate::tools::truncate::sha_spillover_path(sha_candidate)
        && let Some(found) = try_path(p, &mut tried)
    {
        return Ok(found);
    }

    // Compatibility lookup: `art_<id>` may name the historical `<id>.txt`.
    if let Some(stripped_art) = stripped.strip_prefix("art_")
        && let Some(p) = crate::tools::truncate::spillover_path(stripped_art)
        && let Some(found) = try_path(p, &mut tried)
    {
        return Ok(found);
    }

    if let Some(path) = crate::tools::truncate::spillover_path(stripped)
        && let Some(found) = try_path(path, &mut tried)
    {
        return Ok(found);
    }

    Err(not_found(reference, tried))
}

/// Missing evidence is distinct without revealing storage roots or generated
/// session identifiers to the model.
fn not_found(reference: &str, tried: usize) -> ToolError {
    ToolError::execution_failed(format!(
        "retained evidence `{reference}` was not found for the active session \
         ({tried} bounded candidate forms checked). Use the session-owned \
         `art_<id>` handle from the original receipt."
    ))
}

#[derive(Default)]
struct TextInspection {
    mode: String,
    max_bytes: usize,
    line_count: usize,
    max_matches: usize,
    context_lines: usize,
    range: (usize, usize),
    query: String,
    query_lower: String,
    wrap_tail: String,
    total_lines: usize,
    wrapped_segments: usize,
    non_empty_lines: usize,
    matched_lines: usize,
    retained_bytes: usize,
    leading_bytes: usize,
    leading: Vec<(usize, String)>,
    trailing: std::collections::VecDeque<(usize, String)>,
    signal_lines: Vec<Value>,
    previous: std::collections::VecDeque<(usize, String)>,
    matches: Vec<(usize, Vec<(usize, String)>)>,
    active_matches: std::collections::VecDeque<(usize, usize)>,
}

impl TextInspection {
    fn new(mode: &str, input: &Value, max_bytes: usize) -> Result<Self, ToolError> {
        let mut state = Self {
            mode: mode.to_string(),
            max_bytes,
            ..Self::default()
        };
        match mode {
            "summary" | "query" => {
                state.max_matches = clamp_u64(
                    optional_u64(input, "max_matches", DEFAULT_MAX_MATCHES as u64)?,
                    1,
                    HARD_MAX_MATCHES,
                );
                if mode == "summary" {
                    state.line_count = DEFAULT_LINE_COUNT;
                } else {
                    let raw = optional_str(input, "query")?.ok_or_else(|| {
                        ToolError::invalid_input("query is required when mode=query")
                    })?;
                    if raw.len() > MAX_QUERY_BYTES {
                        return Err(ToolError::invalid_input(
                            "query exceeds the 4096-byte inspection limit; use a shorter substring",
                        ));
                    }
                    let query = raw.trim();
                    if query.is_empty() {
                        return Err(ToolError::invalid_input(
                            "query is required when mode=query",
                        ));
                    }
                    state.query = query.to_string();
                    state.query_lower = query.to_lowercase();
                    state.context_lines = clamp_u64(
                        optional_u64(input, "context_lines", DEFAULT_CONTEXT_LINES as u64)?,
                        0,
                        HARD_CONTEXT_LINES,
                    );
                }
            }
            "head" | "tail" => {
                state.line_count = clamp_u64(
                    optional_u64(input, "line_count", DEFAULT_LINE_COUNT as u64)?,
                    1,
                    HARD_LINE_COUNT,
                );
            }
            "lines" => state.range = parse_line_selector(input)?,
            _ => return Err(ToolError::invalid_input("unsupported text inspection mode")),
        }
        Ok(state)
    }

    fn retain(
        &mut self,
        line_no: usize,
        line: &str,
        limit: usize,
    ) -> Result<(usize, String), ToolError> {
        let mut end = line.len().min(limit.saturating_add(4));
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let size = end.saturating_add(std::mem::size_of::<(usize, String)>());
        if self.retained_bytes.saturating_add(size) > MAX_SELECTION_BYTES {
            return Err(ToolError::execution_failed(
                "text selection exceeds the 8 MiB inspection memory limit; reduce line_count, context_lines or max_bytes, or use mode=bytes",
            ));
        }
        self.retained_bytes += size;
        Ok((line_no, line[..end].to_string()))
    }

    fn release(&mut self, record: &(usize, String)) {
        self.retained_bytes = self.retained_bytes.saturating_sub(
            record
                .1
                .len()
                .saturating_add(std::mem::size_of::<(usize, String)>()),
        );
    }

    fn push_line(&mut self, line: &str, wrapped: bool) -> Result<(), ToolError> {
        self.total_lines += 1;
        let line_no = self.total_lines;
        if !line.trim().is_empty() {
            self.non_empty_lines += 1;
        }
        let budget = if self.mode == "summary" {
            self.max_bytes / 2
        } else {
            self.max_bytes
        };
        if (self.mode == "summary" || self.mode == "head")
            && self.leading.len() < self.line_count
            && self.leading_bytes <= budget
            || self.mode == "lines"
                && line_no >= self.range.0
                && line_no <= self.range.1
                && self.leading_bytes <= budget
        {
            let record = self.retain(line_no, line, budget)?;
            self.leading_bytes = self
                .leading_bytes
                .saturating_add(line_no.to_string().len())
                .saturating_add(record.1.len())
                .saturating_add(3);
            self.leading.push(record);
        }
        if self.mode == "summary" || self.mode == "tail" {
            if self.trailing.len() == self.line_count
                && let Some(record) = self.trailing.pop_front()
            {
                self.release(&record);
            }
            let record = self.retain(line_no, line, budget)?;
            self.trailing.push_back(record);
        }
        if self.mode == "summary"
            && self.signal_lines.len() < self.max_matches
            && is_signal_line(line)
        {
            self.signal_lines.push(json!({
                "line": line_no,
                "text": truncate_line(line.trim(), 300),
            }));
        }
        if self.mode != "query" {
            return Ok(());
        }
        let budget = self.max_bytes / self.max_matches;
        while self
            .active_matches
            .front()
            .is_some_and(|(_, end)| *end < line_no)
        {
            self.active_matches.pop_front();
        }
        for position in 0..self.active_matches.len() {
            let index = self.active_matches[position].0;
            let record = self.retain(line_no, line, budget)?;
            self.matches[index].1.push(record);
        }
        let mut matched = line.to_lowercase().contains(&self.query_lower);
        if !matched && !self.wrap_tail.is_empty() {
            let combined = format!("{}{}", self.wrap_tail, line);
            let combined_lower = combined.to_lowercase();
            let boundary = self.wrap_tail.to_lowercase().len();
            matched = combined_lower
                .rfind(&self.query_lower)
                .is_some_and(|pos| pos + self.query_lower.len() > boundary);
        }
        if matched {
            self.matched_lines += 1;
            if self.matches.len() < self.max_matches {
                let mut window = Vec::new();
                let before: Vec<(usize, String)> = self.previous.iter().cloned().collect();
                for (number, text) in before {
                    window.push(self.retain(number, &text, budget)?);
                }
                window.push(self.retain(line_no, line, budget)?);
                let index = self.matches.len();
                self.matches.push((line_no, window));
                self.active_matches
                    .push_back((index, line_no.saturating_add(self.context_lines)));
            }
        }
        if wrapped {
            let keep_chars = self.query_lower.chars().count().saturating_sub(1);
            if keep_chars == 0 {
                self.wrap_tail.clear();
            } else {
                let line_chars = line.chars().count();
                if line_chars >= keep_chars {
                    let start = line
                        .char_indices()
                        .nth(line_chars - keep_chars)
                        .map_or(line.len(), |(idx, _)| idx);
                    self.wrap_tail = line[start..].to_string();
                } else {
                    let tail_need = keep_chars - line_chars;
                    let tail_chars = self.wrap_tail.chars().count();
                    let skip = tail_chars.saturating_sub(tail_need);
                    let start = self
                        .wrap_tail
                        .char_indices()
                        .nth(skip)
                        .map_or(self.wrap_tail.len(), |(idx, _)| idx);
                    self.wrap_tail = format!("{}{}", &self.wrap_tail[start..], line);
                }
            }
        } else {
            self.wrap_tail.clear();
        }
        if self.context_lines > 0 && self.matches.len() < self.max_matches {
            if self.previous.len() == self.context_lines
                && let Some(record) = self.previous.pop_front()
            {
                self.release(&record);
            }
            let record = self.retain(line_no, line, budget)?;
            self.previous.push_back(record);
        }
        Ok(())
    }

    fn payload(self, reference: &str, total_bytes: u64) -> Value {
        let numbered = |records: &[(usize, String)], budget| {
            render_numbered_lines(
                records
                    .iter()
                    .map(|(number, text)| (*number, text.as_str())),
                budget,
            )
        };
        let wrapped_segments = self.wrapped_segments;
        let mut payload = match self.mode.as_str() {
            "summary" => json!({
                "ref": reference,
                "mode": "summary",
                "total_bytes": total_bytes,
                "total_lines": self.total_lines,
                "non_empty_lines": self.non_empty_lines,
                "signal_lines": self.signal_lines,
                "head": numbered(&self.leading, self.max_bytes / 2),
                "tail": render_numbered_lines(self.trailing.iter().map(|(n, text)| (*n, text.as_str())), self.max_bytes / 2),
                "hint": "Use mode=head, tail, lines, or query to retrieve a narrower slice."
            }),
            "head" | "tail" => json!({
                "ref": reference,
                "mode": self.mode,
                "total_lines": self.total_lines,
                "line_count": self.line_count,
                "excerpt": if self.mode == "head" { numbered(&self.leading, self.max_bytes) } else { render_numbered_lines(self.trailing.iter().map(|(n, text)| (*n, text.as_str())), self.max_bytes) },
            }),
            "lines" => json!({
                "ref": reference,
                "mode": "lines",
                "total_lines": self.total_lines,
                "start_line": self.range.0,
                "end_line": self.range.1.min(self.total_lines),
                "excerpt": numbered(&self.leading, self.max_bytes),
            }),
            "query" => json!({
                "ref": reference,
                "mode": "query",
                "query": self.query,
                "total_lines": self.total_lines,
                "matched_lines": self.matched_lines,
                "matches_returned": self.matches.len(),
                "results": self.matches.iter().map(|(line, window)| json!({
                    "line": line,
                    "excerpt": numbered(window, self.max_bytes / self.max_matches),
                })).collect::<Vec<_>>(),
            }),
            _ => unreachable!(),
        };
        if wrapped_segments > 0 {
            payload["long_lines_split_at_bytes"] = json!(WRAP_LINE_BYTES);
            payload["split_segments"] = json!(wrapped_segments);
        }
        payload
    }
}

struct EvidenceSnapshot {
    size_bytes: u64,
    digest: String,
    byte_slice: Vec<u8>,
    inspection: Result<Option<TextInspection>, ToolError>,
}

async fn stream_evidence(
    source: std::fs::File,
    initial_size: u64,
    input: &Value,
    mode: &str,
    max_bytes: usize,
    inspection: Option<TextInspection>,
    context: &ToolContext,
) -> Result<EvidenceSnapshot, ToolError> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let limit_error = || {
        ToolError::execution_failed(
            "evidence exceeds the 512 MiB verification scan limit; export a smaller artifact through the owning session",
        )
    };
    if initial_size > MAX_SCAN_BYTES {
        return Err(limit_error());
    }
    let mut file = tokio::fs::File::from_std(source);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let length = input
        .get("length")
        .and_then(Value::as_u64)
        .unwrap_or(max_bytes as u64)
        .min(max_bytes as u64);
    let slice_end = offset.saturating_add(length);
    let mut state = inspection;
    let mut text_error = None;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    let mut line = Vec::new();
    let mut byte_slice = Vec::new();
    loop {
        if context
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(ToolError::cancelled("Evidence inspection cancelled"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ToolError::execution_failed(
                "evidence verification exceeded the 20-second scan limit; export a smaller artifact through the owning session",
            ));
        }
        let read = async {
            loop {
                let result = match context.cancel_token.as_ref() {
                    Some(token) => tokio::select! {
                        biased;
                        () = token.cancelled() => return Err(ToolError::cancelled("Evidence inspection cancelled")),
                        result = file.read(&mut chunk) => result,
                    },
                    None => file.read(&mut chunk).await,
                };
                match result {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => {
                        return result.map_err(|_| {
                            ToolError::execution_failed("evidence is missing or no longer retained")
                        });
                    }
                }
            }
        };
        let n = tokio::time::timeout_at(deadline, read).await
            .map_err(|_| ToolError::execution_failed("evidence verification exceeded the 20-second scan limit; export a smaller artifact through the owning session"))??;
        if n == 0 {
            break;
        }
        let start = total;
        total = total.saturating_add(n as u64);
        if total > MAX_SCAN_BYTES {
            return Err(limit_error());
        }
        hasher.update(&chunk[..n]);
        if mode == "json" {
            if total > MAX_JSON_BYTES {
                return Err(ToolError::execution_failed(
                    "evidence exceeds the 32 MiB json inspection limit; use mode=query or mode=lines",
                ));
            }
            byte_slice.extend_from_slice(&chunk[..n]);
        }
        if mode == "bytes" {
            let first = offset.max(start);
            let last = slice_end.min(total);
            if first < last {
                byte_slice
                    .extend_from_slice(&chunk[(first - start) as usize..(last - start) as usize]);
            }
        }
        if state.is_some() {
            for piece in chunk[..n].split_inclusive(|byte| *byte == b'\n') {
                line.extend_from_slice(piece);
                let terminated = piece.ends_with(b"\n");
                let mut result = Ok(());
                loop {
                    let body = if terminated {
                        let raw = line.strip_suffix(b"\n").unwrap_or(&line);
                        raw.strip_suffix(b"\r").unwrap_or(raw)
                    } else {
                        line.strip_suffix(b"\r").unwrap_or(&line)
                    };
                    if body.len() <= WRAP_LINE_BYTES {
                        if terminated {
                            result = feed_line(state.as_mut().unwrap(), body, false);
                            line.clear();
                        }
                        break;
                    }
                    let mut cut = WRAP_LINE_BYTES;
                    while cut > 0 && body[cut] & 0xC0 == 0x80 {
                        cut -= 1;
                    }
                    let cut = if cut == 0 { WRAP_LINE_BYTES } else { cut };
                    result = feed_line(state.as_mut().unwrap(), &body[..cut], true);
                    if result.is_err() {
                        break;
                    }
                    line.drain(..cut);
                }
                if let Err(error) = result {
                    text_error = Some(error);
                    state = None;
                    line.clear();
                    break;
                }
            }
        }
    }
    if let Some(state) = state.as_mut()
        && !line.is_empty()
        && let Err(error) = feed_line(state, &line, false)
    {
        text_error = Some(error);
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(ToolError::execution_failed(
            "evidence verification exceeded the 20-second scan limit; export a smaller artifact through the owning session",
        ));
    }
    Ok(EvidenceSnapshot {
        size_bytes: total,
        digest: codewhale_runtime::hashing::hex_bytes(hasher.finalize()),
        byte_slice,
        inspection: match text_error {
            Some(error) => Err(error),
            None => Ok(state),
        },
    })
}

fn feed_line(state: &mut TextInspection, raw: &[u8], wrapped: bool) -> Result<(), ToolError> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        ToolError::execution_failed(
            "evidence encoding is binary; bounded text inspection is unavailable",
        )
    })?;
    if wrapped {
        state.wrapped_segments += 1;
    }
    state.push_line(text, wrapped)
}

fn select_json(bytes: &[u8], path: &str) -> Result<(Vec<Value>, bool), ToolError> {
    fn expand(value: &mut Value) {
        match value {
            Value::String(text) => {
                let trimmed = text.trim_start();
                if (trimmed.starts_with('{') || trimmed.starts_with('['))
                    && let Ok(mut parsed) = serde_json::from_str::<Value>(text)
                {
                    expand(&mut parsed);
                    *value = parsed;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(expand),
            Value::Object(map) => map.values_mut().for_each(expand),
            _ => {}
        }
    }
    let mut root: Value = serde_json::from_slice(bytes).map_err(|error| {
        ToolError::execution_failed(format!(
            "evidence is not JSON ({error}); use mode=summary, query or lines"
        ))
    })?;
    expand(&mut root);
    crate::tools::handle::query_jsonpath_capped(&root, path).map_err(ToolError::invalid_input)
}

fn parse_line_selector(input: &Value) -> Result<(usize, usize), ToolError> {
    let explicit_start = input.get("start_line").and_then(Value::as_u64);
    let explicit_end = input.get("end_line").and_then(Value::as_u64);
    if explicit_start.is_some() || explicit_end.is_some() {
        let start = explicit_start.ok_or_else(|| {
            ToolError::invalid_input("start_line is required when end_line is supplied")
        })?;
        let end = explicit_end.unwrap_or(start);
        let start = usize::try_from(start)
            .map_err(|_| ToolError::invalid_input("start_line exceeds the platform range"))?;
        let end = usize::try_from(end)
            .map_err(|_| ToolError::invalid_input("end_line exceeds the platform range"))?;
        return validate_line_range(start, end);
    }

    let raw = optional_str(input, "lines")?;
    if raw.is_some_and(|value| value.len() > 64) {
        return Err(ToolError::invalid_input(
            "lines selector exceeds the 64-byte inspection limit",
        ));
    }
    let spec = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ToolError::invalid_input(
                "mode=lines requires `lines` (for example \"10-40\") or start_line/end_line",
            )
        })?;

    if let Some((start, end)) = spec.split_once('-') {
        let start = parse_positive_line(start.trim(), "lines start")?;
        let end = parse_positive_line(end.trim(), "lines end")?;
        validate_line_range(start, end)
    } else {
        let line = parse_positive_line(spec, "lines")?;
        validate_line_range(line, line)
    }
}

fn validate_line_range(start: usize, end: usize) -> Result<(usize, usize), ToolError> {
    if start == 0 || end == 0 {
        return Err(ToolError::invalid_input("line numbers are 1-based"));
    }
    if end < start {
        return Err(ToolError::invalid_input(
            "end_line must be greater than or equal to start_line",
        ));
    }
    Ok((start, end))
}

fn parse_positive_line(raw: &str, field: &str) -> Result<usize, ToolError> {
    raw.parse::<usize>().map_err(|_| {
        ToolError::invalid_input(format!("{field} must be a positive integer line number"))
    })
}

fn is_signal_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "warning",
        "exception",
        "traceback",
        "assertion",
        "exit code",
        "test result",
        "thread '",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn render_numbered_lines<'a>(
    lines: impl IntoIterator<Item = (usize, &'a str)>,
    max_bytes: usize,
) -> String {
    let mut rendered = String::new();
    for (line_no, line) in lines {
        rendered.push_str(&format!("{line_no}: {line}\n"));
        if rendered.len() > max_bytes {
            break;
        }
    }
    truncate_text(&rendered, max_bytes)
}

fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.trim_end_matches('\n').to_string();
    }
    let note = "\n[truncated to max_bytes]";
    let budget = max_bytes.saturating_sub(note.len()).max(1);
    let cut = (0..=budget)
        .rev()
        .find(|idx| text.is_char_boundary(*idx))
        .unwrap_or(0);
    format!("{}{}", text[..cut].trim_end_matches('\n'), note)
}

fn truncate_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn clamp_u64(value: u64, min: usize, max: usize) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::MutexGuard;
    use tempfile::tempdir;

    struct SpilloverRootGuard {
        prior: Option<PathBuf>,
    }

    impl Drop for SpilloverRootGuard {
        fn drop(&mut self) {
            crate::tools::truncate::set_test_spillover_root(self.prior.take());
        }
    }

    fn set_spillover_root(path: PathBuf) -> SpilloverRootGuard {
        let prior = crate::tools::truncate::set_test_spillover_root(Some(path));
        SpilloverRootGuard { prior }
    }

    fn context() -> ToolContext {
        let tmp = tempdir().unwrap();
        ToolContext::new(tmp.path())
    }

    fn test_lock() -> MutexGuard<'static, ()> {
        crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn execute_tool(input: Value) -> Result<ToolResult, ToolError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(RetrieveToolResultTool.execute(input, &context()))
    }

    fn execute_tool_in_session(input: Value, session_id: &str) -> Result<ToolResult, ToolError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut context = context();
        context.state_namespace = session_id.to_string();
        runtime.block_on(RetrieveToolResultTool.execute(input, &context))
    }

    fn publish_test_evidence(
        session_id: &str,
        handle: &str,
        bytes: &[u8],
        expired: bool,
    ) -> crate::tools::large_output_router::EvidenceArtifact {
        let relative = codewhale_runtime::artifacts::session_artifact_relative_path(handle);
        codewhale_runtime::artifacts::write_session_relative_immutable(
            session_id, &relative, bytes,
        )
        .unwrap();
        let now = crate::tools::large_output_router::unix_millis_now();
        let artifact = crate::tools::large_output_router::EvidenceArtifact {
            handle: handle.to_string(),
            digest: codewhale_runtime::hashing::sha256_hex(bytes),
            size_bytes: bytes.len() as u64,
            content_type: "application/octet-stream".to_string(),
            tool_name: "exec_shell".to_string(),
            call_id: handle.trim_start_matches("art_").to_string(),
            origin_session: session_id.to_string(),
            generation: 1,
            redacted: false,
            encoding: "binary".to_string(),
            retention_state: if expired {
                crate::tools::large_output_router::EvidenceRetentionState::Expired
            } else {
                crate::tools::large_output_router::EvidenceRetentionState::Live
            },
            created_at_unix_ms: now,
            retain_until_unix_ms: now.saturating_add(60_000),
            storage_path: relative,
        };
        crate::tools::large_output_router::publish_evidence_metadata(session_id, &artifact)
            .unwrap();
        artifact
    }

    fn write_owned_legacy(id: &str, content: &str, session_id: &str) -> PathBuf {
        let path = crate::tools::truncate::write_spillover(id, content).unwrap();
        crate::tools::truncate::publish_legacy_spillover_ownership(
            &path,
            session_id,
            content.as_bytes(),
        )
        .unwrap();
        path
    }

    fn write_owned_sha(content: &str, session_id: &str) -> (String, PathBuf) {
        let sha = codewhale_runtime::hashing::sha256_hex(content.as_bytes());
        let path = crate::tools::truncate::write_sha_spillover(&sha, content).unwrap();
        crate::tools::truncate::publish_legacy_spillover_ownership(
            &path,
            session_id,
            content.as_bytes(),
        )
        .unwrap();
        (sha, path)
    }

    #[test]
    fn summary_reads_spillover_by_tool_call_id() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let session_id = "session-legacy-summary";
        write_owned_legacy(
            "call-abc",
            "checking crate\nerror[E0425]: missing value\nwarning: unused import\nfinished",
            session_id,
        );

        let result = execute_tool_in_session(json!({"ref": "call-abc"}), session_id).unwrap();

        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["mode"], "summary");
        assert!(body["signal_lines"].to_string().contains("error[E0425]"));
        assert!(body["signal_lines"].to_string().contains("warning"));
    }

    #[test]
    fn adaptive_evidence_binary_bytes_are_exact_and_bounded() {
        let _spill = test_lock();
        let _artifact = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempdir().unwrap();
        let _root = set_spillover_root(tmp.path().join("tool_outputs"));
        let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
            tmp.path().join("sessions"),
        ));
        struct Restore(Option<PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = Restore(prior);
        let bytes = b"\0\xffbinary\nDEEP_SENTINEL\x80tail";
        publish_test_evidence("session-a", "art_call-binary", bytes, false);

        let result = execute_tool_in_session(
            json!({"ref": "art_call-binary", "mode": "bytes", "offset": 0, "length": 1024}),
            "session-a",
        )
        .unwrap();
        let body: Value = serde_json::from_str(&result.content).unwrap();
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, bytes);
        assert_eq!(body["total_bytes"], bytes.len());
    }

    #[test]
    fn adaptive_evidence_retrieves_after_restart_without_memory_state() {
        let _spill = test_lock();
        let _artifact = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempdir().unwrap();
        let _root = set_spillover_root(tmp.path().join("tool_outputs"));
        let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
            tmp.path().join("sessions"),
        ));
        struct Restore(Option<PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = Restore(prior);

        let bytes = b"restart-proof\nDEEP_RESTART_SENTINEL\nend";
        publish_test_evidence("session-restart", "art_call-restart", bytes, false);

        // Construct two independent contexts to model process teardown and
        // resume. Retrieval must depend only on the sealed session artifact
        // and metadata, never an in-memory routing table from publication.
        let first = execute_tool_in_session(
            json!({"ref": "art_call-restart", "mode": "metadata"}),
            "session-restart",
        )
        .unwrap();
        drop(first);
        let resumed = execute_tool_in_session(
            json!({"ref": "art_call-restart", "mode": "bytes", "length": 4096}),
            "session-restart",
        )
        .unwrap();
        let body: Value = serde_json::from_str(&resumed.content).unwrap();
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, bytes);
        assert_eq!(body["total_bytes"], bytes.len());
    }

    #[test]
    fn adaptive_evidence_distinguishes_corrupt_expired_and_generation_mismatch() {
        let _spill = test_lock();
        let _artifact = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempdir().unwrap();
        let _root = set_spillover_root(tmp.path().join("tool_outputs"));
        let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
            tmp.path().join("sessions"),
        ));
        struct Restore(Option<PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = Restore(prior);

        publish_test_evidence("session-a", "art_call-expired", b"expired", true);
        let expired = execute_tool_in_session(json!({"ref": "art_call-expired"}), "session-a")
            .unwrap_err()
            .to_string();
        assert!(expired.contains("expired"), "{expired}");

        let artifact = publish_test_evidence("session-a", "art_call-corrupt", b"original", false);
        let absolute = codewhale_runtime::artifacts::session_artifact_absolute_path(
            "session-a",
            &artifact.storage_path,
        )
        .unwrap();
        std::fs::write(absolute, b"changed").unwrap();
        let corrupt = execute_tool_in_session(json!({"ref": "art_call-corrupt"}), "session-a")
            .unwrap_err()
            .to_string();
        assert!(corrupt.contains("corrupt"), "{corrupt}");

        publish_test_evidence("session-a", "art_call-generation", b"stable", false);
        let mismatch = execute_tool_in_session(
            json!({"ref": "art_call-generation", "generation": 2}),
            "session-a",
        )
        .unwrap_err()
        .to_string();
        assert!(mismatch.contains("generation"), "{mismatch}");
    }

    #[test]
    fn query_returns_matching_line_with_context() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let session_id = "session-legacy-query";
        write_owned_legacy(
            "call-query",
            "one\ntwo before\nneedle here\nafter\nlast",
            session_id,
        );

        let result = execute_tool_in_session(
            json!({
                "ref": "tool_result:call-query",
                "mode": "query",
                "query": "needle",
                "context_lines": 1
            }),
            session_id,
        )
        .unwrap();

        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["matched_lines"], 1);
        let rendered = body["results"].to_string();
        assert!(rendered.contains("2: two before"));
        assert!(rendered.contains("3: needle here"));
        assert!(rendered.contains("4: after"));
    }

    #[test]
    fn single_line_mcp_json_pages_by_segment_and_jsonpath() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let session_id = "session-legacy-json";
        let runs: Vec<Value> = (0..30)
            .map(|id| json!({"id": id, "conclusion": "success", "repository": {"description": "r".repeat(12_000)}}))
            .collect();
        let inner = json!({"total_count": 40_000, "workflow_runs": runs}).to_string();
        let envelope = json!({"content": [{"type": "text", "text": inner}]}).to_string();
        assert!(envelope.len() > 300_000 && !envelope.contains('\n'));
        write_owned_legacy("call-json", &envelope, session_id);

        let summary =
            execute_tool_in_session(json!({"ref": "call-json", "mode": "summary"}), session_id)
                .unwrap();
        let summary: Value = serde_json::from_str(&summary.content).unwrap();
        assert!(summary["total_lines"].as_u64().unwrap() > 100, "{summary}");
        assert_eq!(summary["long_lines_split_at_bytes"], WRAP_LINE_BYTES);
        assert!(summary["head"].as_str().unwrap().len() <= DEFAULT_MAX_BYTES / 2);

        let ids = execute_tool_in_session(
            json!({"ref": "call-json", "mode": "json", "jsonpath": "$.content[0].text.workflow_runs[*].id"}),
            session_id,
        )
        .unwrap();
        let ids: Value = serde_json::from_str(&ids.content).unwrap();
        assert_eq!(ids["matches"], 30);
        assert_eq!(ids["values"], json!((0..30).collect::<Vec<_>>()));
        assert_eq!(ids["truncated"], false);

        let whole =
            execute_tool_in_session(json!({"ref": "call-json", "mode": "json"}), session_id)
                .unwrap();
        assert!(
            whole.content.len() < 2 * DEFAULT_MAX_BYTES,
            "{}",
            whole.content.len()
        );
        let whole: Value = serde_json::from_str(&whole.content).unwrap();
        assert_eq!(whole["truncated"], true);
        assert_eq!(
            whole["values"][0]["content"][0]["text"]["total_count"],
            40_000
        );

        let large_runs: Vec<Value> = (0..1_050)
            .map(|id| json!({"id": id, "repository": {"description": "r".repeat(1_100)}}))
            .collect();
        let large_body = json!({"total_count": 1_050, "workflow_runs": large_runs}).to_string();
        assert!(large_body.len() > 1_100_000);
        write_owned_legacy("call-json-large", &large_body, session_id);
        let large_whole = execute_tool_in_session(
            json!({"ref": "call-json-large", "mode": "json"}),
            session_id,
        )
        .unwrap();
        let large_whole: Value = serde_json::from_str(&large_whole.content).unwrap();
        assert_eq!(large_whole["truncated"], true);
        assert_eq!(large_whole["values"][0]["total_count"], 1_050);

        let capped = execute_tool_in_session(
            json!({"ref": "call-json-large", "mode": "json", "jsonpath": "$.workflow_runs[*].id"}),
            session_id,
        )
        .unwrap();
        let capped: Value = serde_json::from_str(&capped.content).unwrap();
        assert_eq!(capped["matches"], 1_024);
        assert_eq!(capped["truncated"], true);
    }

    #[test]
    fn query_matches_needle_straddling_split_boundary() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let session_id = "session-query-straddle";
        let body = format!("{}NEEDLE_XYZ{}\n", "a".repeat(2_040), "b".repeat(3_000));
        write_owned_legacy("call-straddle", &body, session_id);

        let out = execute_tool_in_session(
            json!({"ref": "call-straddle", "mode": "query", "query": "needle_xyz"}),
            session_id,
        )
        .unwrap();
        let out: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(out["matched_lines"], 1, "{out}");

        let long_query = format!("{}NEEDLE_XYZ{}", "a".repeat(1_500), "b".repeat(1_500));
        let long_out = execute_tool_in_session(
            json!({"ref": "call-straddle", "mode": "query", "query": long_query}),
            session_id,
        )
        .unwrap();
        let long_out: Value = serde_json::from_str(&long_out.content).unwrap();
        assert_eq!(long_out["matched_lines"], 1, "{long_out}");
    }

    #[test]
    fn lines_mode_accepts_filename_inside_spillover_root() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("tool_outputs");
        let _guard = set_spillover_root(root.clone());
        let session_id = "session-legacy-lines";
        write_owned_legacy("call-lines", "a\nb\nc\nd", session_id);

        let result = execute_tool_in_session(
            json!({
                "ref": "call-lines.txt",
                "mode": "lines",
                "lines": "2-3"
            }),
            session_id,
        )
        .unwrap();

        let body: Value = serde_json::from_str(&result.content).unwrap();
        let excerpt = body["excerpt"].as_str().unwrap();
        assert!(excerpt.contains("2: b"));
        assert!(excerpt.contains("3: c"));
        assert!(!excerpt.contains("1: a"));
        assert!(!excerpt.contains("4: d"));
    }

    #[test]
    fn rejects_path_outside_spillover_root() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("tool_outputs");
        fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let _guard = set_spillover_root(root);

        let err = execute_tool(json!({"ref": outside.display().to_string()})).unwrap_err();

        // Unauthorized is distinct but non-leaking: no outside path detail is
        // echoed beyond the caller-supplied ref.
        let msg = err.to_string();
        assert!(
            msg.contains("authorize") && msg.contains("active session"),
            "expected non-leaking authorization diagnostic, got: {msg}"
        );
    }

    #[test]
    fn resolves_sha_reference_from_wire_dedup() {
        // A SHA-keyed lookup — emulates what happens when the model
        // sees a `<TOOL_RESULT_REF sha="..." />` block and passes the
        // SHA to retrieve_tool_result.
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let body = "checking crate ... error[E0425]: cannot find value\n".repeat(80);
        let session_id = "session-legacy-sha";
        let (sha, _) = write_owned_sha(&body, session_id);

        // Form: `sha:<hex>`
        let result =
            execute_tool_in_session(json!({"ref": format!("sha:{sha}")}), session_id).unwrap();
        assert!(result.success, "sha:<hex> form should resolve");

        // Form: bare 64-hex
        let result = execute_tool_in_session(json!({"ref": &sha}), session_id).unwrap();
        assert!(result.success, "bare 64-hex form should resolve");
    }

    #[test]
    fn resolves_art_prefix_to_legacy_spillover_id() {
        // The model commonly sees `id: art_call_xyz` in artifact
        // ref blocks. retrieve_tool_result should strip the `art_`
        // prefix and find the legacy `<id>.txt` file if no
        // session-artifact equivalent exists.
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let session_id = "session-legacy-art-prefix";
        write_owned_legacy("call_xyz", "line1\nline2\nline3", session_id);

        let result = execute_tool_in_session(json!({"ref": "art_call_xyz"}), session_id).unwrap();
        assert!(result.success, "art_ prefix should resolve to legacy id");
    }

    #[test]
    fn unowned_and_foreign_legacy_spillovers_fail_closed_without_leaking_content() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let sentinel = "SESSION_A_PRIVATE_SENTINEL";

        crate::tools::truncate::write_spillover("call-unowned", sentinel).unwrap();
        let unowned =
            execute_tool_in_session(json!({"ref": "call-unowned", "mode": "bytes"}), "session-b")
                .unwrap_err()
                .to_string();
        assert!(unowned.contains("no verifiable session owner"), "{unowned}");
        assert!(!unowned.contains(sentinel), "{unowned}");
        assert!(
            !unowned.contains(tmp.path().to_string_lossy().as_ref()),
            "{unowned}"
        );

        write_owned_legacy("call-foreign", sentinel, "session-a");
        let foreign =
            execute_tool_in_session(json!({"ref": "call-foreign", "mode": "bytes"}), "session-b")
                .unwrap_err()
                .to_string();
        assert!(foreign.contains("another session"), "{foreign}");
        assert!(!foreign.contains(sentinel), "{foreign}");
        assert!(
            !foreign.contains(tmp.path().to_string_lossy().as_ref()),
            "{foreign}"
        );
    }

    #[test]
    fn owned_legacy_digest_mismatch_is_distinct_from_unauthorized() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let path = write_owned_legacy("call-corrupt-owned", "original", "session-a");
        std::fs::write(path, "changed").unwrap();

        let error = execute_tool_in_session(json!({"ref": "call-corrupt-owned"}), "session-a")
            .unwrap_err()
            .to_string();
        assert!(error.contains("content is corrupt"), "{error}");
        assert!(!error.contains("another session"), "{error}");
    }

    #[test]
    fn not_found_error_lists_tried_candidates_and_accepted_forms() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        fs::create_dir_all(tmp.path().join("tool_outputs")).unwrap();

        let err = execute_tool(json!({"ref": "definitely_missing_id"})).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains("active session"), "got: {msg}");
        assert!(msg.contains("art_<id>"), "got: {msg}");
        assert!(!msg.contains("tool_outputs"), "storage root leaked: {msg}");
        assert!(
            !msg.contains(tmp.path().to_string_lossy().as_ref()),
            "path leaked: {msg}"
        );
    }

    #[test]
    fn resolves_art_prefix_via_session_artifacts() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let _art_guard = {
            let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            ));
            scopeguard_for_test(prior)
        };
        let session_id = "session-abc";
        let body = "this is the canonical session artifact body, not a legacy file";
        codewhale_runtime::artifacts::write_session_artifact(session_id, "art_call_real", body)
            .unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace(session_id);
        let result = runtime
            .block_on(RetrieveToolResultTool.execute(json!({"ref": "art_call_real"}), &ctx))
            .expect("art_<id> should resolve via session artifacts");
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert!(
            payload
                .to_string()
                .contains("canonical session artifact body"),
            "summary should pull from session artifact, got: {payload}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_inside_session_artifacts() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let _art_guard = {
            let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            ));
            scopeguard_for_test(prior)
        };
        let session_id = "session-xyz";
        // Plant a sensitive file outside the artifact dir.
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "do not leak").unwrap();
        // Create the artifact dir, then drop a symlink inside it
        // pointing at the secret.
        let art_dir = tmp
            .path()
            .join("sessions")
            .join(session_id)
            .join("artifacts");
        fs::create_dir_all(&art_dir).unwrap();
        std::os::unix::fs::symlink(&secret, art_dir.join("art_evil.txt")).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace(session_id);
        let result =
            runtime.block_on(RetrieveToolResultTool.execute(json!({"ref": "art_evil"}), &ctx));
        let err = result.expect_err("symlink artifact must not resolve");
        assert!(
            err.to_string().contains("not found"),
            "expected `not found`, got: {err}"
        );
    }

    struct ArtifactRootGuard {
        prior: Option<PathBuf>,
    }
    impl Drop for ArtifactRootGuard {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.prior.take());
        }
    }
    fn scopeguard_for_test(prior: Option<PathBuf>) -> ArtifactRootGuard {
        ArtifactRootGuard { prior }
    }
}

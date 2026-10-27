//! Patch tools: `apply_patch` for unified diff patching
//!
//! This tool provides precise file modifications using unified diff format,
//! supporting multi-hunk patches and fuzzy matching.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use codewhale_tools::param_alias::{PATH_ALIASES, apply_param_aliases};
use codewhale_tools::patch_preflight::{
    ApplyPatchPreflight, ApplyPatchPreflightKind, FilePatch, Hunk, HunkLine,
    NormalizedApplyPatchInput, format_file_list, normalize_apply_patch_input,
    preflight_apply_patch_plan, push_unique,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use super::diff_format::make_unified_diff;
use super::file::{EXPECTED_HASH_DESCRIPTION, PATCH_PARAMS, content_hash};
use super::rust_format::normalize_edit;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    lsp_diagnostics_for_paths, optional_bool, optional_str, optional_u64,
};
use super::syntax_check::guard_edit;

/// Maximum lines of context for fuzzy matching (increased for better tolerance)
const MAX_FUZZ: usize = 50;
/// Default fuzz when the caller does not specify one. Matches the tool schema's
/// documented default. Previously the default was `MAX_FUZZ` (50), so a hunk
/// with no `fuzz` argument could silently apply up to 50 lines from its stated
/// position — landing in the wrong region of a file with repeated blocks.
const DEFAULT_FUZZ: usize = 3;

/// Minimum number of expected lines (context + removed) required before a hunk
/// may be relocated to a unique whole-file context match when its stated line
/// numbers are stale (#5003). Short anchors — a lone `}` or a 1-2 line snippet
/// — appear in too many places to relocate safely.
const MIN_ANCHOR_LINES: usize = 4;

/// Reassemble hunk-processed logical lines back into file content, preserving
/// the base file's line-ending style (CRLF vs LF) and its trailing-newline
/// state. Processing round-trips through `str::lines()`, which strips both the
/// trailing `\n` and any `\r`; naively `join("\n")`-ing would silently delete
/// the file's final newline and flip a CRLF file to LF on every patch.
fn reassemble_preserving_newlines(lines: &[String], base_content: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let terminator = if base_content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    // A newly created file (empty base) gets a conventional trailing newline;
    // an existing file preserves whether it had one.
    let trailing = base_content.is_empty() || base_content.ends_with('\n');
    let mut out = lines.join(terminator);
    if trailing {
        out.push_str(terminator);
    }
    out
}
/// Limit how much context we print in error messages.
const HUNK_PREVIEW_LINES: usize = 4;
const SNIPPET_RADIUS: usize = 2;

// === Types ===

/// Result of applying a patch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchResult {
    pub success: bool,
    pub files_applied: usize,
    pub files_total: usize,
    pub hunks_applied: usize,
    pub hunks_total: usize,
    pub fuzz_used: usize,
    #[serde(default)]
    pub hunks_with_fuzz: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hunks_relocated: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_summaries: Vec<FileSummary>,
    pub message: String,
}

/// Per-file summary for patch application output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
    pub path: String,
    pub hunks: usize,
    pub hunks_applied: usize,
    pub fuzz_used: usize,
    pub hunks_with_fuzz: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hunks_relocated: usize,
    pub created: bool,
    pub deleted: bool,
}

/// Tool for applying unified diff patches to files
pub struct ApplyPatchTool;

#[derive(Debug, Clone)]
struct PendingWrite {
    path: PathBuf,
    content: Option<String>,
    original: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct PatchStats {
    files_applied: usize,
    files_total: usize,
    hunks_applied: usize,
    hunks_total: usize,
    fuzz_used: usize,
    hunks_with_fuzz: usize,
    hunks_relocated: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Default, Clone)]
struct PatchStatsExt {
    stats: PatchStats,
    touched_files: Vec<String>,
    file_summaries: Vec<FileSummary>,
    header_path_mismatch: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct HunkApplyStats {
    hunks_applied: usize,
    fuzz_used: usize,
    hunks_with_fuzz: usize,
    hunks_relocated: usize,
}

/// Result of applying a single hunk: how much positional fuzz was used, and
/// whether the hunk had to be relocated to a unique whole-file context match
/// (stale line numbers after earlier edits, #5003).
#[derive(Debug, Default, Clone, Copy)]
struct HunkApplyOutcome {
    fuzz_used: usize,
    relocated: bool,
}

// === Errors ===

#[derive(Debug, Error)]
enum ApplyHunkError {
    #[error(
        "Failed to find matching location for hunk (expected at line {expected_line}, adjusted to {adjusted_line} with offset {offset:+})"
    )]
    NoMatch {
        expected_line: usize,
        adjusted_line: usize,
        offset: isize,
    },
    #[error(
        "Hunk context is ambiguous: matches at multiple locations {candidate_lines:?}, expected around line {expected_line}"
    )]
    ContextAmbiguous {
        expected_line: usize,
        candidate_lines: Vec<usize>,
    },
}

#[async_trait]
impl ToolSpec for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn model_visible(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Apply a transactional unified-diff patch across one or more files, with fuzzy context matching and a rendered diff."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to patch (relative to workspace)"
                },
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch content"
                },
                "replace": {
                    "type": "array",
                    "description": "Optional full file replacements (path + content).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }
                },
                "changes": {
                    "type": "array",
                    "description": "Deprecated compatibility alias for `replace` (full file replacements by path + content).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }
                },
                "fuzz": {
                    "type": "integer",
                    "description": "Maximum fuzz factor for fuzzy matching (default: 3)"
                },
                "create_if_missing": {
                    "type": "boolean",
                    "description": "Create the file if it doesn't exist (for new file patches)"
                },
                "expected_hash": {
                    "type": "string",
                    "description": format!(
                        "{EXPECTED_HASH_DESCRIPTION} Verifies the patch target — the `path` argument when given, otherwise the first file the patch touches; other files in a multi-file patch are not hash-checked."
                    )
                }
            },
            "oneOf": [
                { "required": ["patch"] },
                { "required": ["replace"] },
                { "required": ["changes"] }
            ]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Suggest
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File patch")?;
        PATCH_PARAMS.reject_unknown(&input)?;
        let input = input;

        let fuzz = optional_u64(&input, "fuzz", DEFAULT_FUZZ as u64)?.min(MAX_FUZZ as u64);
        let fuzz = usize::try_from(fuzz).unwrap_or(DEFAULT_FUZZ);
        let normalized = normalize_apply_patch_input(&input)?;
        let create_if_missing = optional_bool(&input, "create_if_missing", false)?;
        let preflight = preflight_apply_patch_plan(&input, normalized)?;
        verify_patch_expected_hash(&input, &preflight.summary, context)?;

        if let NormalizedApplyPatchInput::Replacement {
            entries,
            source_field,
        } = normalized
        {
            let entries = entries.to_vec();
            let build_context = context.clone();
            let (mut pending, stats) = patch_blocking(move || {
                build_pending_writes_from_replace(&entries, source_field, &build_context)
            })
            .await?;
            normalize_pending_rust(&mut pending).await;
            let pending = apply_pending_writes_blocking(pending).await?;
            for entry in pending.iter().filter(|p| p.content.is_some()) {
                context.note_file_read(&entry.path);
            }
            // Resolve absolute paths for LSP diagnostics query.
            let abs_paths: Vec<PathBuf> = pending.iter().map(|p| p.path.clone()).collect();
            let diag_block = lsp_diagnostics_for_paths(context, &abs_paths).await;
            let result = PatchResult {
                success: true,
                files_applied: stats.stats.files_applied,
                files_total: stats.stats.files_total,
                hunks_applied: stats.stats.hunks_applied,
                hunks_total: stats.stats.hunks_total,
                fuzz_used: stats.stats.fuzz_used,
                hunks_with_fuzz: stats.stats.hunks_with_fuzz,
                hunks_relocated: stats.stats.hunks_relocated,
                touched_files: stats.touched_files.clone(),
                file_summaries: stats.file_summaries.clone(),
                message: build_summary_message(&stats),
            };
            let mut tool_result = ToolResult::json(&result)
                .map_err(|e| ToolError::execution_failed(e.to_string()))?;
            tool_result = tool_result.with_metadata(apply_patch_result_metadata(
                &preflight.summary,
                &pending,
                &stats,
            ));
            if !diag_block.is_empty() {
                tool_result.content.push('\n');
                tool_result.content.push_str(&diag_block);
            }
            return Ok(tool_result);
        }

        let file_patches = match preflight.kind {
            ApplyPatchPreflightKind::Replace => {
                unreachable!("replace input returned before patch execution")
            }
            ApplyPatchPreflightKind::PathOverride {
                path,
                hunks,
                creates_new_file,
            } => vec![FilePatch {
                path,
                hunks,
                delete_after: false,
                create_if_missing,
                creates_new_file,
            }],
            ApplyPatchPreflightKind::FilePatches(file_patches) => file_patches,
        };

        let build_context = context.clone();
        let (mut pending, mut stats) = patch_blocking(move || {
            build_pending_writes_from_patches(file_patches, &build_context, fuzz)
        })
        .await?;
        stats.header_path_mismatch = preflight.summary.header_path_mismatch.clone();
        normalize_pending_rust(&mut pending).await;
        let pending = apply_pending_writes_blocking(pending).await?;
        for entry in pending.iter().filter(|p| p.content.is_some()) {
            context.note_file_read(&entry.path);
        }
        // Resolve absolute paths for LSP diagnostics query.
        let abs_paths: Vec<PathBuf> = pending
            .iter()
            .filter(|p| p.content.is_some()) // skip deleted files
            .map(|p| p.path.clone())
            .collect();
        let diag_block = lsp_diagnostics_for_paths(context, &abs_paths).await;
        let result = PatchResult {
            success: true,
            files_applied: stats.stats.files_applied,
            files_total: stats.stats.files_total,
            hunks_applied: stats.stats.hunks_applied,
            hunks_total: stats.stats.hunks_total,
            fuzz_used: stats.stats.fuzz_used,
            hunks_with_fuzz: stats.stats.hunks_with_fuzz,
            hunks_relocated: stats.stats.hunks_relocated,
            touched_files: stats.touched_files.clone(),
            file_summaries: stats.file_summaries.clone(),
            message: build_summary_message(&stats),
        };
        let mut tool_result =
            ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))?;
        tool_result = tool_result.with_metadata(apply_patch_result_metadata(
            &preflight.summary,
            &pending,
            &stats,
        ));
        if !diag_block.is_empty() {
            tool_result.content.push('\n');
            tool_result.content.push_str(&diag_block);
        }
        Ok(tool_result)
    }
}

/// Enforce the optional `expected_hash` precondition for a patch (#3979).
///
/// **This is a whole-patch precondition on a single target file, not a per-file
/// guard.** A unified diff carries no place to attach a hash per file section,
/// and `File action="patch"` flattens its arguments into one object, so there
/// is no clean parameter shape for a hash-per-path map without inventing a
/// syntax the model has never seen. The guarded file is therefore the patch's
/// target: the explicit `path` argument when one is given, otherwise the first
/// file the patch touches. A multi-file patch still verifies only that one
/// file; for the rest, the existing hunk-context matching remains the check
/// that a stale patch fails on.
///
/// Runs before any pending write is built or applied, so a mismatch leaves
/// every file in the patch untouched.
fn verify_patch_expected_hash(
    input: &Value,
    summary: &ApplyPatchPreflight,
    context: &ToolContext,
) -> Result<(), ToolError> {
    let Some(expected) = optional_str(input, "expected_hash")? else {
        return Ok(());
    };

    let Some(target) = summary
        .path_override
        .as_deref()
        .or_else(|| summary.touched_files.first().map(String::as_str))
    else {
        return Err(ToolError::execution_failed(
            "File `patch` refused: expected_hash was supplied but the patch names no target file to verify it against, so nothing was written.".to_string(),
        ));
    };

    let resolved = context.resolve_path(target)?;
    if !resolved.exists() {
        // Fail closed, matching `write`: a hash describes a file that was
        // read, so a missing target means the guard cannot be honored.
        return Err(ToolError::execution_failed(format!(
            "File `patch` refused: expected_hash was supplied but {target} does not exist, so there is no snapshot to verify and nothing was written. Recovery: drop `expected_hash` when creating files."
        )));
    }

    let current = fs::read(&resolved).map_err(|e| {
        ToolError::execution_failed(format!(
            "File `patch` refused: could not read {target} to verify expected_hash ({e}); nothing was written."
        ))
    })?;
    let actual = content_hash(&current);
    if actual == expected {
        return Ok(());
    }
    Err(ToolError::execution_failed(format!(
        "File `patch` refused: {target} changed since it was read. \
         expected_hash was {expected} but the file is now {actual}, so nothing was written. \
         Recovery: call File with action=\"read\" path=\"{target}\" to get the current contents \
         and its content_hash, then rebuild the patch against them."
    )))
}

fn apply_patch_result_metadata(
    preflight: &ApplyPatchPreflight,
    pending: &[PendingWrite],
    stats: &PatchStatsExt,
) -> Value {
    let mut metadata =
        serde_json::to_value(preflight).expect("ApplyPatchPreflight should serialize");
    if let Some(object) = metadata.as_object_mut() {
        object.insert("event".to_string(), json!("apply_patch.preflight"));
        object.insert(
            "mutation".to_string(),
            build_mutation_metadata(pending, &stats.file_summaries),
        );
    }
    metadata
}

/// Preserve the exact applied before/after diff independently from approval
/// presentation. The TUI consumes this success-only metadata for its calm
/// File receipt; the normal model-facing result remains compact JSON.
fn build_mutation_metadata(pending: &[PendingWrite], summaries: &[FileSummary]) -> Value {
    let mut matched = HashSet::new();
    let mut renames = Vec::new();

    for (delete_index, (deleted, delete_summary)) in pending.iter().zip(summaries).enumerate() {
        if !delete_summary.deleted || matched.contains(&delete_index) {
            continue;
        }
        let Some(old_content) = deleted.original.as_deref() else {
            continue;
        };
        let Some((create_index, (created, create_summary))) = pending
            .iter()
            .zip(summaries)
            .enumerate()
            .find(|(index, (created, summary))| {
                !matched.contains(index)
                    && summary.created
                    && created.content.as_deref() == Some(old_content)
            })
        else {
            continue;
        };
        matched.insert(delete_index);
        matched.insert(create_index);
        // The destination's bytes are the ones this call wrote, so a rename
        // carries the same size/sha256 facts as a created or updated file.
        let mut rename = json!({
            "from": delete_summary.path,
            "to": create_summary.path,
        });
        if let Some(content) = created.content.as_deref() {
            rename["size"] = json!(content.len());
            rename["sha256"] = json!(codewhale_runtime::hashing::sha256_hex(content.as_bytes()));
        }
        renames.push(rename);
    }

    let mut files = Vec::new();
    for (index, summary) in summaries.iter().enumerate() {
        if matched.contains(&index) {
            continue;
        }
        let outcome = if summary.created {
            "created"
        } else if summary.deleted {
            "deleted"
        } else {
            "updated"
        };
        // Deleted entries carry neither size nor sha256: no bytes were written.
        let written = pending
            .get(index)
            .and_then(|write| write.content.as_deref())
            .filter(|_| !summary.deleted)
            .map(str::as_bytes);
        files.push(crate::tools::file::mutation_file_entry(
            &summary.path,
            outcome,
            written,
        ));
    }

    let mut diff_parts = Vec::new();
    for rename in &renames {
        let from = rename["from"].as_str().unwrap_or("<file>");
        let to = rename["to"].as_str().unwrap_or("<file>");
        diff_parts.push(format!(
            "diff --git a/{from} b/{to}\nsimilarity index 100%\nrename from {from}\nrename to {to}\n"
        ));
    }
    for (index, (write, summary)) in pending.iter().zip(summaries).enumerate() {
        if matched.contains(&index) {
            continue;
        }
        let old = write.original.as_deref().unwrap_or("");
        let new = write.content.as_deref().unwrap_or("");
        let diff = make_unified_diff(&summary.path, old, new);
        if !diff.is_empty() {
            diff_parts.push(format!(
                "diff --git a/{path} b/{path}\n{diff}",
                path = summary.path
            ));
        }
    }

    json!({
        "diff": diff_parts.join("\n"),
        "files": files,
        "renames": renames,
    })
}

fn build_summary_message(stats: &PatchStatsExt) -> String {
    let mut parts = Vec::new();
    if stats.stats.hunks_total > 0 {
        parts.push(format!(
            "Applied {}/{} hunks across {} file(s).",
            stats.stats.hunks_applied, stats.stats.hunks_total, stats.stats.files_applied
        ));
    } else {
        parts.push(format!(
            "Applied {} file change(s).",
            stats.stats.files_applied
        ));
    }

    if !stats.touched_files.is_empty() {
        parts.push(format!(
            "Files: {}.",
            format_file_list(&stats.touched_files)
        ));
    }

    if stats.stats.fuzz_used > 0 {
        parts.push(format!(
            "Fuzz used on {} hunk(s) (total fuzz: {}).",
            stats.stats.hunks_with_fuzz, stats.stats.fuzz_used
        ));
    }

    if stats.stats.hunks_relocated > 0 {
        parts.push(format!(
            "{} hunk(s) applied with stale line numbers (auto-relocated to unique context).",
            stats.stats.hunks_relocated
        ));
    }

    if let Some(note) = stats.header_path_mismatch.as_deref() {
        parts.push(note.to_string());
    }

    parts.join(" ")
}

fn build_pending_writes_from_replace(
    changes: &[Value],
    source_field: &str,
    context: &ToolContext,
) -> Result<(Vec<PendingWrite>, PatchStatsExt), ToolError> {
    let mut pending = Vec::new();
    let mut stats = PatchStatsExt::default();
    for change in changes {
        let path = change
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field(format!("{source_field}[].path")))?;
        let content = change
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field(format!("{source_field}[].content")))?;

        let resolved = context.resolve_path(path)?;
        if resolved.exists() {
            context.require_fresh_read_before_write(&resolved, path)?;
        }
        let original = if resolved.exists() {
            Some(read_file_content(&resolved)?)
        } else {
            None
        };
        let created = original.is_none();

        pending.push(PendingWrite {
            path: resolved,
            content: Some(content.to_string()),
            original,
        });

        stats.stats.files_total += 1;
        stats.stats.files_applied += 1;
        push_unique(&mut stats.touched_files, path.to_string());
        stats.file_summaries.push(FileSummary {
            path: path.to_string(),
            hunks: 0,
            hunks_applied: 0,
            fuzz_used: 0,
            hunks_with_fuzz: 0,
            hunks_relocated: 0,
            created,
            deleted: false,
        });
    }

    Ok((pending, stats))
}

fn build_pending_writes_from_patches(
    file_patches: Vec<FilePatch>,
    context: &ToolContext,
    fuzz: usize,
) -> Result<(Vec<PendingWrite>, PatchStatsExt), ToolError> {
    let mut pending: Vec<PendingWrite> = Vec::new();
    // Where each file's write already sits in `pending`: a second section for
    // the same file applies on top of the first instead of re-reading the
    // disk and silently dropping the earlier hunks (B7).
    let mut pending_index: std::collections::HashMap<PathBuf, usize> =
        std::collections::HashMap::new();
    let mut stats = PatchStatsExt::default();
    stats.stats.files_total = file_patches.len();

    for file_patch in file_patches {
        if file_patch.hunks.is_empty() {
            return Err(ToolError::invalid_input(format!(
                "Patch section for `{}` has no hunks (`@@ ... @@`).",
                file_patch.path
            )));
        }

        let resolved = context.resolve_path(&file_patch.path)?;
        if let Some(&index) = pending_index.get(&resolved) {
            let Some(current) = pending[index].content.clone() else {
                return Err(ToolError::invalid_input(format!(
                    "Patch has a section for `{}` after a section that deletes it; merge them into one section.",
                    file_patch.path
                )));
            };
            if file_patch.creates_new_file && !current.is_empty() {
                return Err(file_already_exists_error(&file_patch.path, &resolved));
            }
            let mut lines: Vec<String> = current.lines().map(String::from).collect();
            let apply_stats =
                apply_hunks_to_lines(&mut lines, &file_patch.hunks, fuzz, &file_patch.path)?;
            if file_patch.delete_after {
                ensure_delete_consumes_file(&lines, &file_patch.path)?;
            }
            stats.stats.hunks_applied += apply_stats.hunks_applied;
            stats.stats.hunks_total += file_patch.hunks.len();
            stats.stats.fuzz_used += apply_stats.fuzz_used;
            stats.stats.hunks_with_fuzz += apply_stats.hunks_with_fuzz;
            stats.stats.hunks_relocated += apply_stats.hunks_relocated;
            if let Some(summary) = stats
                .file_summaries
                .iter_mut()
                .rev()
                .find(|summary| summary.path == file_patch.path)
            {
                summary.hunks += file_patch.hunks.len();
                summary.hunks_applied += apply_stats.hunks_applied;
                summary.fuzz_used += apply_stats.fuzz_used;
                summary.hunks_with_fuzz += apply_stats.hunks_with_fuzz;
                summary.hunks_relocated += apply_stats.hunks_relocated;
                summary.deleted |= file_patch.delete_after;
                summary.created &= !file_patch.delete_after;
            }
            pending[index].content = (!file_patch.delete_after)
                .then(|| reassemble_preserving_newlines(&lines, &current));
            // One file, however many sections: count it once.
            stats.stats.files_total = stats.stats.files_total.saturating_sub(1);
            continue;
        }
        let original = if resolved.exists() {
            Some(read_file_content(&resolved)?)
        } else {
            None
        };

        if original.is_none() && !file_patch.create_if_missing {
            return Err(ToolError::execution_failed(format!(
                "File `{}` does not exist at `{}`. Set create_if_missing=true for new files or include headers for file creation.",
                file_patch.path,
                resolved.display(),
            )));
        }

        if file_patch.delete_after && original.is_none() {
            return Err(ToolError::execution_failed(format!(
                "File `{}` does not exist at `{}` to delete.",
                file_patch.path,
                resolved.display(),
            )));
        }

        // A `--- /dev/null` section has no old lines to anchor on, so on an
        // existing file it would splice the "new" content onto the top of
        // the old one and report success. `git apply` refuses this too.
        if file_patch.creates_new_file && original.as_deref().is_some_and(|c| !c.is_empty()) {
            return Err(file_already_exists_error(&file_patch.path, &resolved));
        }

        let base_content = original.clone().unwrap_or_default();
        let mut lines: Vec<String> = if base_content.is_empty() {
            Vec::new()
        } else {
            base_content.lines().map(String::from).collect()
        };

        let apply_stats =
            apply_hunks_to_lines(&mut lines, &file_patch.hunks, fuzz, &file_patch.path)?;
        if file_patch.delete_after {
            ensure_delete_consumes_file(&lines, &file_patch.path)?;
        }
        stats.stats.hunks_applied += apply_stats.hunks_applied;
        stats.stats.hunks_total += file_patch.hunks.len();
        stats.stats.fuzz_used += apply_stats.fuzz_used;
        stats.stats.hunks_with_fuzz += apply_stats.hunks_with_fuzz;
        stats.stats.hunks_relocated += apply_stats.hunks_relocated;
        stats.stats.files_applied += 1;
        push_unique(&mut stats.touched_files, file_patch.path.clone());
        stats.file_summaries.push(FileSummary {
            path: file_patch.path.clone(),
            hunks: file_patch.hunks.len(),
            hunks_applied: apply_stats.hunks_applied,
            fuzz_used: apply_stats.fuzz_used,
            hunks_with_fuzz: apply_stats.hunks_with_fuzz,
            hunks_relocated: apply_stats.hunks_relocated,
            created: original.is_none() && !file_patch.delete_after,
            deleted: file_patch.delete_after,
        });

        pending_index.insert(resolved.clone(), pending.len());
        if file_patch.delete_after {
            pending.push(PendingWrite {
                path: resolved,
                content: None,
                original,
            });
        } else {
            let new_content = reassemble_preserving_newlines(&lines, &base_content);
            pending.push(PendingWrite {
                path: resolved,
                content: Some(new_content),
                original,
            });
        }
    }

    Ok((pending, stats))
}

fn file_already_exists_error(label: &str, resolved: &Path) -> ToolError {
    ToolError::execution_failed(format!(
        "Patch creates `{label}` (`--- /dev/null`), but it already exists at `{}`. Nothing was changed. To change the existing file, send a patch with context lines from its current contents; to replace it wholesale, use a write/replace action.",
        resolved.display(),
    ))
}

/// A `+++ /dev/null` section deletes the file, so its hunks must remove every
/// line. Deleting after a partial match would silently drop the rest.
fn ensure_delete_consumes_file(remaining: &[String], label: &str) -> Result<(), ToolError> {
    if remaining.is_empty() {
        return Ok(());
    }
    Err(ToolError::execution_failed(format!(
        "Patch deletes `{label}` (`+++ /dev/null`), but its hunks leave {} line(s) of the file unaccounted for. Nothing was changed. Include every line of the file as `-` lines to delete it, or send a patch with a real `+++` path to edit it.",
        remaining.len(),
    )))
}

/// Normalize the Rust files a patch rewrites (#6205), before the write and
/// before the result's diff is built, so the rendered diff and the bytes on
/// disk are the same text and the model's next anchor matches reality.
async fn normalize_pending_rust(pending: &mut [PendingWrite]) {
    for entry in pending.iter_mut() {
        let (Some(content), Some(original)) = (entry.content.as_ref(), entry.original.as_ref())
        else {
            continue;
        };
        if let Some(normalized) = normalize_edit(&entry.path, original, content).await {
            entry.content = Some(normalized);
        }
    }
}

/// Run the patch's synchronous file work (reads, the syntax gate, writes and
/// rollback) on the blocking pool rather than an async worker (#6559 D01-06).
async fn patch_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ToolError::execution_failed(format!("patch task failed: {error}")))?
}

/// [`apply_pending_writes`] on the blocking pool, handing `pending` back for
/// the result's diagnostics and metadata.
async fn apply_pending_writes_blocking(
    pending: Vec<PendingWrite>,
) -> Result<Vec<PendingWrite>, ToolError> {
    patch_blocking(move || apply_pending_writes(&pending).map(|()| pending)).await
}

fn apply_pending_writes(pending: &[PendingWrite]) -> Result<(), ToolError> {
    // Syntax gate (#6204) ahead of the first write, not per file: a patch is
    // transactional, so one unparseable result must leave every file in the
    // patch untouched rather than half-applied and rolled back.
    for entry in pending {
        if let Some(content) = entry.content.as_ref() {
            let display = entry.path.display().to_string();
            guard_edit(&entry.path, &display, entry.original.as_deref(), content)?;
        }
    }

    let mut applied = Vec::new();

    for entry in pending {
        let result = if let Some(content) = entry.content.as_ref() {
            let parent_result = if let Some(parent) = entry.path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    ToolError::execution_failed(format!(
                        "Failed to create directory {}: {}",
                        parent.display(),
                        e
                    ))
                })
            } else {
                Ok(())
            };

            parent_result.and_then(|()| {
                codewhale_util::utils::write_atomic_workspace(&entry.path, content.as_bytes())
                    .map_err(|e| {
                        ToolError::execution_failed(format!(
                            "Failed to write {}: {}",
                            entry.path.display(),
                            e
                        ))
                    })
            })
        } else if entry.path.exists() {
            fs::remove_file(&entry.path).map_err(|e| {
                ToolError::execution_failed(format!(
                    "Failed to delete {}: {}",
                    entry.path.display(),
                    e
                ))
            })
        } else {
            Ok(())
        };

        if let Err(err) = result {
            let failures = rollback_pending_writes(&applied);
            if failures.is_empty() {
                return Err(err);
            }
            // A failed restore leaves the patch partly applied; say which
            // files still hold it rather than implying nothing changed.
            return Err(ToolError::execution_failed(format!(
                "{err}. Rolling back the files already written also failed, so the patch is \
                 partly applied: {}",
                failures.join("; ")
            )));
        }

        applied.push(entry.clone());
    }

    Ok(())
}

/// Restore every already-applied entry, newest first. Returns one line per
/// entry that could not be restored (#6559 D01-06: these were discarded).
fn rollback_pending_writes(applied: &[PendingWrite]) -> Vec<String> {
    let mut failures = Vec::new();
    for entry in applied.iter().rev() {
        let restored = match entry.original.as_ref() {
            Some(content) => {
                codewhale_util::utils::write_atomic_workspace(&entry.path, content.as_bytes())
            }
            None => match fs::remove_file(&entry.path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(error) = restored {
            failures.push(format!("{} ({error})", entry.path.display()));
        }
    }
    failures
}

fn read_file_content(path: &PathBuf) -> Result<String, ToolError> {
    fs::read_to_string(path).map_err(|e| {
        ToolError::execution_failed(format!("Failed to read {}: {}", path.display(), e))
    })
}

fn preview_expected_lines(hunk: &Hunk, limit: usize) -> Vec<String> {
    let mut preview = Vec::new();
    for line in hunk.lines.iter().filter_map(|line| match line {
        HunkLine::Context(s) => Some((" ", s)),
        HunkLine::Remove(s) => Some(("-", s)),
        HunkLine::Add(_) => None,
    }) {
        if preview.len() >= limit {
            break;
        }
        preview.push(format!("  {}{}", line.0, line.1));
    }
    if preview.is_empty() {
        preview.push("  <no context lines in hunk>".to_string());
    }
    preview
}

fn snippet_around(lines: &[String], line_1_based: usize, radius: usize) -> Vec<String> {
    if lines.is_empty() {
        return vec!["  <empty file>".to_string()];
    }

    let center = line_1_based
        .saturating_sub(1)
        .min(lines.len().saturating_sub(1));
    let start = center.saturating_sub(radius);
    let end = (center + radius).min(lines.len().saturating_sub(1));

    lines[start..=end]
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let line_no = start + idx + 1;
            format!("  {line_no:>4}: {line}")
        })
        .collect()
}

fn format_hunk_no_match_error(
    lines: &[String],
    hunk: &Hunk,
    err: &ApplyHunkError,
    max_fuzz: usize,
) -> String {
    match err {
        ApplyHunkError::NoMatch {
            expected_line,
            adjusted_line,
            offset,
        } => {
            let expected_preview = preview_expected_lines(hunk, HUNK_PREVIEW_LINES).join("\n");
            let file_preview = snippet_around(lines, *adjusted_line, SNIPPET_RADIUS).join("\n");
            format!(
                "could not find matching context near line {expected_line} (searched around line {adjusted_line} with offset {offset:+} and fuzz up to {max_fuzz}). Expected context preview:\n{expected_preview}\nFile snippet near line {adjusted_line}:\n{file_preview}\nHints: the line numbers may be stale after earlier edits — call File with action=\"read\" to re-check the current contents, ensure the patch matches the file, increase `fuzz`, or regenerate the patch."
            )
        }
        ApplyHunkError::ContextAmbiguous {
            expected_line,
            candidate_lines,
        } => {
            let candidates = candidate_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "could not find matching context near line {expected_line}: the hunk's context appears at multiple locations (lines {candidates}), and the line numbers may be stale after earlier edits, so it is not safe to relocate automatically. Hints: call File with action=\"read\" to inspect the candidate locations above, then regenerate the patch with more surrounding context lines that uniquely identify the target block."
            )
        }
    }
}

fn apply_hunks_to_lines(
    lines: &mut Vec<String>,
    hunks: &[Hunk],
    fuzz: usize,
    file_label: &str,
) -> Result<HunkApplyStats, ToolError> {
    let mut stats = HunkApplyStats::default();
    let mut cumulative_offset: isize = 0;

    for (idx, hunk) in hunks.iter().enumerate() {
        match apply_hunk(lines, hunk, fuzz, &mut cumulative_offset) {
            Ok(outcome) => {
                stats.hunks_applied += 1;
                if outcome.fuzz_used > 0 {
                    stats.fuzz_used += outcome.fuzz_used;
                    stats.hunks_with_fuzz += 1;
                }
                if outcome.relocated {
                    stats.hunks_relocated += 1;
                }
            }
            Err(e) => {
                let detail = format_hunk_no_match_error(lines, hunk, &e, fuzz);
                return Err(ToolError::execution_failed(format!(
                    "Failed to apply hunk {}/{} for `{}`: {}",
                    idx + 1,
                    hunks.len(),
                    file_label,
                    detail
                )));
            }
        }
    }

    Ok(stats)
}

/// Apply a hunk to the file content with fuzzy matching
fn apply_hunk(
    lines: &mut Vec<String>,
    hunk: &Hunk,
    max_fuzz: usize,
    cumulative_offset: &mut isize,
) -> Result<HunkApplyOutcome, ApplyHunkError> {
    // Build expected old lines from hunk
    let old_lines: Vec<&str> = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.as_str()),
            HunkLine::Add(_) => None,
        })
        .collect();

    // Build new lines from hunk
    let new_lines: Vec<String> = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(s) | HunkLine::Add(s) => Some(s.clone()),
            HunkLine::Remove(_) => None,
        })
        .collect();

    // Try to find the location with fuzzy matching
    // Apply cumulative offset from previous hunks, clamping to valid range.
    // A pure insertion (`@@ -N,0 +M,K @@`, as `git diff -U0` emits) names
    // the line it follows, so it goes *after* line N, not before it. Only a
    // header that says `,0` qualifies: an omitted or nonzero old count keeps
    // the "before line N" anchor. `new_start` settles the common hand-written
    // `@@ -N,0 +N,K @@` ("new text starts at line N"): when it points at
    // line N itself rather than after it, the text goes before line N.
    let pure_insertion = hunk.old_count == 0 && old_lines.is_empty();
    let starts_at_named_line = hunk.new_start > 0
        && cumulative_offset
            .checked_neg()
            .and_then(|back| (hunk.new_start - 1).checked_add_signed(back))
            == Some(hunk.old_start.saturating_sub(1));
    let base_idx = if pure_insertion && !starts_at_named_line {
        hunk.old_start
    } else {
        hunk.old_start.saturating_sub(1)
    };
    // Use checked_add_signed to safely handle negative offsets without
    // risking isize overflow on adversarial input.
    let start_idx = base_idx
        .checked_add_signed(*cumulative_offset)
        .unwrap_or(0)
        .min(lines.len());

    for fuzz in 0..=max_fuzz {
        // Try at exact position first, then nearby
        let search_range = if fuzz == 0 {
            vec![start_idx]
        } else {
            let min = start_idx.saturating_sub(fuzz);
            let max = (start_idx + fuzz).min(lines.len());
            (min..=max).collect()
        };

        for pos in search_range {
            if matches_at_position(lines, &old_lines, pos) {
                // Apply the hunk
                let end_pos = pos + old_lines.len();
                lines.splice(pos..end_pos, new_lines.clone());

                // Update cumulative offset: new lines added minus old lines removed
                let delta = new_lines.len() as isize - old_lines.len() as isize;
                *cumulative_offset += delta;

                return Ok(HunkApplyOutcome {
                    fuzz_used: fuzz,
                    relocated: false,
                });
            }
        }
    }

    // Special case: adding to empty file or new hunk at end
    if old_lines.is_empty() && (lines.is_empty() || start_idx >= lines.len()) {
        let delta = new_lines.len() as isize;
        lines.extend(new_lines);
        *cumulative_offset += delta;
        return Ok(HunkApplyOutcome {
            fuzz_used: 0,
            relocated: false,
        });
    }

    // #5003 — positional search failed. The line numbers are probably stale
    // because an earlier edit (this patch or a previous one) shifted the file
    // and the model regenerated the patch from outdated read_file output.
    // If the hunk carries enough anchor lines, look for a unique whole-file
    // content match and relocate there; anything that matched within `fuzz`
    // of `start_idx` was already tried above, so any unique match found here
    // is genuinely relocated. Ambiguous matches are refused (applying to the
    // wrong copy of a repeated block would corrupt the file).
    let anchor_matches: Vec<usize> = if old_lines.len() >= MIN_ANCHOR_LINES {
        (0..=lines.len().saturating_sub(old_lines.len()))
            .filter(|&pos| matches_at_position(lines, &old_lines, pos))
            .collect()
    } else {
        Vec::new()
    };
    match anchor_matches.as_slice() {
        [pos] => {
            let end_pos = pos + old_lines.len();
            lines.splice(*pos..end_pos, new_lines.clone());
            let delta = new_lines.len() as isize - old_lines.len() as isize;
            *cumulative_offset += delta;
            Ok(HunkApplyOutcome {
                fuzz_used: 0,
                relocated: true,
            })
        }
        [] => Err(ApplyHunkError::NoMatch {
            expected_line: hunk.old_start,
            adjusted_line: start_idx + 1, // Convert back to 1-indexed
            offset: *cumulative_offset,
        }),
        multiple => Err(ApplyHunkError::ContextAmbiguous {
            expected_line: hunk.old_start,
            candidate_lines: multiple.iter().map(|&p| p + 1).collect(),
        }),
    }
}

/// Check if `old_lines` match at the given position
fn matches_at_position(lines: &[String], old_lines: &[&str], pos: usize) -> bool {
    if pos + old_lines.len() > lines.len() {
        return false;
    }

    for (i, old_line) in old_lines.iter().enumerate() {
        // Normalize whitespace for comparison
        let file_line = lines[pos + i].trim_end();
        let expected = old_line.trim_end();
        if file_line != expected {
            return false;
        }
    }

    true
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_tools::patch_preflight::{
        parse_range, parse_unified_diff, preflight_apply_patch,
    };
    use tempfile::tempdir;

    /// #6559 D01-06: a rollback that cannot restore a file used to be
    /// discarded, so the error read as if nothing had changed. Deleting `x`
    /// and creating `x/y.txt` leaves a directory at `x`; when the third write
    /// fails, restoring the file `x` over that directory fails too.
    #[test]
    fn failed_rollback_is_reported_as_a_partly_applied_patch() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("x"), "original\n").expect("seed x");
        fs::write(root.join("blocker"), "a file, not a directory\n").expect("seed blocker");
        let pending = vec![
            PendingWrite {
                path: root.join("x"),
                content: None,
                original: Some("original\n".to_string()),
            },
            PendingWrite {
                path: root.join("x").join("y.txt"),
                content: Some("new\n".to_string()),
                original: None,
            },
            PendingWrite {
                path: root.join("blocker").join("z.txt"),
                content: Some("never written\n".to_string()),
                original: None,
            },
        ];

        let message = apply_pending_writes(&pending)
            .expect_err("the third write cannot create its directory")
            .to_string();

        assert!(message.contains("partly applied"), "{message}");
        assert!(
            message.contains(&root.join("x").display().to_string()),
            "{message}"
        );
    }

    /// The receipt entry a created/updated file must carry: its size and
    /// SHA-256 are those of the bytes now on disk.
    fn on_disk_receipt(root: &std::path::Path, path: &str, outcome: &str) -> Value {
        let bytes = fs::read(root.join(path)).expect("receipt target exists");
        json!({
            "path": path,
            "outcome": outcome,
            "size": bytes.len(),
            "sha256": codewhale_runtime::hashing::sha256_hex(&bytes),
        })
    }

    fn parse_patch_result(result: ToolResult) -> PatchResult {
        serde_json::from_str(&result.content).expect("patch result json")
    }

    #[test]
    fn test_parse_range() {
        assert_eq!(parse_range("10,5").unwrap(), (10, 5));
        assert_eq!(parse_range("10").unwrap(), (10, 1));
        assert_eq!(parse_range("1,0").unwrap(), (1, 0));
    }

    #[test]
    fn test_parse_unified_diff() {
        let patch = r"--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,3 @@
 line1
-line2
+modified line2
 line3
";

        let hunks = parse_unified_diff(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_count, 3);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_count, 3);
    }

    /// #6205 — a patch that lands unformatted Rust in an already-clean file is
    /// normalized before the write, and the rendered diff shows the normalized
    /// text, so the model's next patch context matches the bytes on disk.
    #[tokio::test]
    async fn patch_normalizes_rust_in_an_already_clean_file() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let file = tmp.path().join("clean.rs");
        fs::write(&file, "fn main() {\n    let x = 1;\n}\n").expect("write");

        let patch = "--- a/clean.rs\n+++ b/clean.rs\n@@ -1,3 +1,4 @@\n fn main() {\n     let x = 1;\n+        let y=2;\n }\n";
        let result = ApplyPatchTool
            .execute(json!({"path": "clean.rs", "patch": patch}), &ctx)
            .await
            .expect("execute");

        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n"
        );
        let diff = result.metadata.as_ref().expect("metadata")["mutation"]["diff"]
            .as_str()
            .expect("diff")
            .to_string();
        assert!(diff.contains("+    let y = 2;"), "{diff}");
        assert!(!diff.contains("let y=2;"), "{diff}");
    }

    /// #6204 — a patch whose result does not parse is refused before any file
    /// is written, so a multi-file patch cannot land half-applied.
    #[tokio::test]
    async fn patch_refuses_a_hunk_that_breaks_rust_syntax() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let file = tmp.path().join("main.rs");
        let original = "fn main() {\n    println!(\"hi\");\n}\n";
        fs::write(&file, original).expect("write");

        let patch = "--- a/main.rs\n+++ b/main.rs\n@@ -1,3 +1,2 @@\n fn main() {\n     println!(\"hi\");\n-}\n";
        let error = ApplyPatchTool
            .execute(json!({"path": "main.rs", "patch": patch}), &ctx)
            .await
            .expect_err("a patch that breaks Rust syntax must be refused");

        let message = error.to_string();
        assert!(message.contains("Rust syntax error at line"), "{message}");
        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            original,
            "a refused patch must leave the file untouched"
        );
    }

    #[test]
    fn input_schema_exposes_replace_and_deprecated_changes_alias() {
        let schema = ApplyPatchTool.input_schema();

        assert_eq!(schema["properties"]["replace"]["type"], "array");
        assert_eq!(schema["properties"]["changes"]["type"], "array");
        assert!(
            schema["properties"]["changes"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("Deprecated"))
        );
        assert_eq!(
            schema["oneOf"],
            json!([
                { "required": ["patch"] },
                { "required": ["replace"] },
                { "required": ["changes"] }
            ])
        );
    }

    #[test]
    fn test_preflight_apply_patch_with_path_override() {
        let patch = r"@@ -1,2 +1,2 @@
 old
-value
+new-value
";

        let preflight = preflight_apply_patch(&json!({
            "path": "src/lib.rs",
            "patch": patch
        }))
        .expect("preflight");

        assert_eq!(preflight.touched_files, vec!["src/lib.rs"]);
        assert_eq!(preflight.files_total, 1);
        assert_eq!(preflight.hunks_total, 1);
        assert_eq!(preflight.path_override.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn test_preflight_apply_patch_multi_file_create_and_delete() {
        let patch = r"diff --git a/new.rs b/new.rs
--- /dev/null
+++ b/new.rs
@@ -0,0 +1 @@
+fn added() {}
diff --git a/old.rs b/old.rs
--- a/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-fn old() {}
";

        let preflight = preflight_apply_patch(&json!({ "patch": patch })).expect("preflight");

        assert_eq!(preflight.touched_files, vec!["new.rs", "old.rs"]);
        assert_eq!(preflight.files_total, 2);
        assert_eq!(preflight.hunks_total, 2);
        assert_eq!(preflight.creates, vec!["new.rs"]);
        assert_eq!(preflight.deletes, vec!["old.rs"]);
    }

    #[test]
    fn test_preflight_apply_patch_timestamp_headers_strip_metadata() {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\t2026-06-26 10:00:00 +0000\n\
+++ b/src/lib.rs\t2026-06-26 10:01:00 +0000\n\
@@ -1,1 +1,1 @@\n\
-old\n\
+new\n";

        let preflight = preflight_apply_patch(&json!({ "patch": patch })).expect("preflight");

        assert_eq!(preflight.touched_files, vec!["src/lib.rs"]);
        assert_eq!(preflight.files_total, 1);
        assert_eq!(preflight.hunks_total, 1);
    }

    #[test]
    fn test_preflight_apply_patch_ignores_forged_headers_inside_hunk_shape() {
        let patch = r"--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 line1
--- a/forged.rs
+++ b/forged.rs
 line3
";

        let preflight = preflight_apply_patch(&json!({
            "path": "src/lib.rs",
            "patch": patch
        }))
        .expect("preflight");

        assert_eq!(preflight.touched_files, vec!["src/lib.rs"]);
        assert_eq!(preflight.header_path_mismatch, None);
    }

    #[test]
    fn test_preflight_apply_patch_replace_list() {
        let canonical = preflight_apply_patch(&json!({
            "replace": [
                { "path": "one.txt", "content": "one" },
                { "path": "two.txt", "content": "two" }
            ]
        }))
        .expect("preflight");

        let legacy = preflight_apply_patch(&json!({
            "changes": [
                { "path": "one.txt", "content": "one" },
                { "path": "two.txt", "content": "two" }
            ]
        }))
        .expect("legacy preflight");

        assert_eq!(canonical.touched_files, vec!["one.txt", "two.txt"]);
        assert_eq!(canonical.files_total, 2);
        assert_eq!(canonical.hunks_total, 0);
        assert_eq!(legacy, canonical);
    }

    #[test]
    fn test_preflight_replace_files_total_counts_entries() {
        let preflight = preflight_apply_patch(&json!({
            "replace": [
                { "path": "same.txt", "content": "one" },
                { "path": "same.txt", "content": "two" }
            ]
        }))
        .expect("preflight");

        assert_eq!(preflight.touched_files, vec!["same.txt"]);
        assert_eq!(preflight.files_total, 2);
    }

    #[test]
    fn test_preflight_patch_files_total_counts_sections() {
        let patch = r"diff --git a/same.txt b/same.txt
--- a/same.txt
+++ b/same.txt
@@ -1,1 +1,1 @@
-one
+two
diff --git a/same.txt b/same.txt
--- a/same.txt
+++ b/same.txt
@@ -2,1 +2,1 @@
-three
+four
";

        let preflight = preflight_apply_patch(&json!({ "patch": patch })).expect("preflight");

        assert_eq!(preflight.touched_files, vec!["same.txt"]);
        assert_eq!(preflight.files_total, 2);
        assert_eq!(preflight.hunks_total, 2);
    }

    #[test]
    fn test_apply_hunk_simple() {
        let mut lines = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
        ];

        let hunk = Hunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            lines: vec![
                HunkLine::Context("line1".to_string()),
                HunkLine::Remove("line2".to_string()),
                HunkLine::Add("modified".to_string()),
                HunkLine::Context("line3".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let outcome = apply_hunk(&mut lines, &hunk, 0, &mut offset).unwrap();
        assert_eq!(outcome.fuzz_used, 0);
        assert!(!outcome.relocated);
        assert_eq!(lines, vec!["line1", "modified", "line3"]);
    }

    #[test]
    fn test_apply_hunk_with_fuzz() {
        let mut lines = vec![
            "line0".to_string(),
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
        ];

        // Hunk expects to start at line 1, but content is at line 2
        let hunk = Hunk {
            old_start: 1, // Wrong position
            old_count: 2,
            new_start: 1,
            new_count: 2,
            lines: vec![
                HunkLine::Remove("line1".to_string()),
                HunkLine::Add("modified".to_string()),
                HunkLine::Context("line2".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let outcome = apply_hunk(&mut lines, &hunk, 3, &mut offset).unwrap();
        assert!(outcome.fuzz_used > 0);
        assert!(!outcome.relocated);
        assert_eq!(lines, vec!["line0", "modified", "line2", "line3"]);
    }

    #[test]
    fn test_apply_hunk_no_match_returns_error() {
        let mut lines = vec!["line1".to_string(), "line2".to_string()];
        let hunk = Hunk {
            old_start: 5,
            old_count: 1,
            new_start: 5,
            new_count: 1,
            lines: vec![
                HunkLine::Context("missing".to_string()),
                HunkLine::Add("new".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let err = apply_hunk(&mut lines, &hunk, 0, &mut offset).unwrap_err();
        assert!(matches!(
            err,
            ApplyHunkError::NoMatch {
                expected_line: 5,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_apply_patch_tool() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        // Create a test file
        fs::write(tmp.path().join("test.txt"), "line1\nline2\nline3\n").expect("write");

        let patch = r"--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,3 @@
 line1
-line2
+modified
 line3
";

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"path": "test.txt", "patch": patch}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert_eq!(
            result.metadata.as_ref().unwrap()["event"],
            "apply_patch.preflight"
        );
        assert_eq!(
            result.metadata.as_ref().unwrap()["touched_files"],
            json!(["test.txt"])
        );
        assert!(
            result
                .metadata
                .as_ref()
                .unwrap()
                .get("header_path_mismatch")
                .is_none()
        );
        assert!(
            result
                .metadata
                .as_ref()
                .unwrap()
                .get("path_override")
                .is_some()
        );
        let mutation = &result.metadata.as_ref().unwrap()["mutation"];
        assert_eq!(
            mutation["files"],
            json!([on_disk_receipt(tmp.path(), "test.txt", "updated")])
        );
        assert!(
            mutation["diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("-line2") && diff.contains("+modified")),
            "{mutation}"
        );
        let patch_result = parse_patch_result(result);
        assert_eq!(patch_result.touched_files, vec!["test.txt"]);
        assert_eq!(patch_result.hunks_applied, 1);

        // Verify the patch was applied
        let content = fs::read_to_string(tmp.path().join("test.txt")).expect("read");
        assert!(content.contains("modified"));
        assert!(!content.contains("line2"));
        // Regression: the file's trailing newline must survive the patch.
        assert!(content.ends_with('\n'), "trailing newline was dropped");
    }

    #[test]
    fn reassemble_preserving_newlines_keeps_style() {
        let lines = vec!["a".to_string(), "b".to_string()];
        // LF with trailing newline.
        assert_eq!(reassemble_preserving_newlines(&lines, "x\ny\n"), "a\nb\n");
        // LF without trailing newline.
        assert_eq!(reassemble_preserving_newlines(&lines, "x\ny"), "a\nb");
        // CRLF is preserved (endings and trailing).
        assert_eq!(
            reassemble_preserving_newlines(&lines, "x\r\ny\r\n"),
            "a\r\nb\r\n"
        );
        // New/empty file gets a conventional trailing newline.
        assert_eq!(reassemble_preserving_newlines(&lines, ""), "a\nb\n");
        // Empty result stays empty.
        assert_eq!(reassemble_preserving_newlines(&[], "x\n"), "");
    }

    #[tokio::test]
    async fn apply_patch_preserves_crlf_line_endings() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("crlf.txt"), "line1\r\nline2\r\nline3\r\n").expect("write");
        let patch =
            "--- a/crlf.txt\n+++ b/crlf.txt\n@@ -1,3 +1,3 @@\n line1\n-line2\n+modified\n line3\n";
        let result = ApplyPatchTool
            .execute(json!({"path": "crlf.txt", "patch": patch}), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        let content = fs::read_to_string(tmp.path().join("crlf.txt")).expect("read");
        assert!(content.contains("modified"));
        // Regression: a CRLF file must not be flipped to LF.
        assert!(
            content.contains("\r\n"),
            "CRLF was flipped to LF: {content:?}"
        );
        assert!(!content.contains("\n\n"), "spurious bare LF introduced");
        assert!(content.ends_with("\r\n"), "trailing CRLF dropped");
    }

    #[tokio::test]
    async fn test_apply_patch_add_lines() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("test.txt"), "line1\nline3\n").expect("write");

        let patch = r"@@ -1,2 +1,3 @@
 line1
+line2
 line3
";

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"path": "test.txt", "patch": patch}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let mutation = &result.metadata.as_ref().expect("metadata")["mutation"];
        assert_eq!(
            mutation["files"],
            json!([on_disk_receipt(tmp.path(), "test.txt", "updated")])
        );
        assert!(
            mutation["diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("+line2")),
            "{mutation}"
        );
        let patch_result = parse_patch_result(result);
        assert_eq!(patch_result.touched_files, vec!["test.txt"]);

        let content = fs::read_to_string(tmp.path().join("test.txt")).expect("read");
        assert!(content.contains("line2"));
    }

    #[tokio::test]
    async fn test_apply_patch_create_new_file() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let patch = r"@@ -0,0 +1,3 @@
+line1
+line2
+line3
";

        let tool = ApplyPatchTool;
        let result = tool
            .execute(
                json!({"path": "new_file.txt", "patch": patch, "create_if_missing": true}),
                &ctx,
            )
            .await
            .expect("execute");

        assert!(result.success);
        let mutation = &result.metadata.as_ref().expect("metadata")["mutation"];
        assert_eq!(
            mutation["files"],
            json!([on_disk_receipt(tmp.path(), "new_file.txt", "created")])
        );
        assert!(
            mutation["diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("+line1")),
            "{mutation}"
        );
        let patch_result = parse_patch_result(result);
        assert_eq!(patch_result.touched_files, vec!["new_file.txt"]);
        assert!(patch_result.file_summaries.first().unwrap().created);
        assert!(tmp.path().join("new_file.txt").exists());
    }

    #[tokio::test]
    async fn test_apply_patch_replace_list() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("one.txt"), "old\n").expect("write");
        ctx.note_file_read(&ctx.resolve_path("one.txt").expect("resolve"));

        let tool = ApplyPatchTool;
        let result = tool
            .execute(
                json!({
                    "replace": [
                        { "path": "one.txt", "content": "new\n" },
                        { "path": "two.txt", "content": "second\n" }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("execute");

        assert!(result.success);
        let metadata = result.metadata.as_ref().expect("metadata");
        assert_eq!(metadata["event"], "apply_patch.preflight");
        assert_eq!(metadata["touched_files"], json!(["one.txt", "two.txt"]));
        assert_eq!(metadata["files_total"], 2);
        assert_eq!(metadata["hunks_total"], 0);
        assert!(metadata.get("path_override").is_none());
        assert_eq!(
            metadata["mutation"]["files"],
            json!([
                on_disk_receipt(tmp.path(), "one.txt", "updated"),
                on_disk_receipt(tmp.path(), "two.txt", "created")
            ])
        );
        let mutation_diff = metadata["mutation"]["diff"]
            .as_str()
            .expect("mutation diff");
        assert!(mutation_diff.contains("diff --git a/one.txt b/one.txt"));
        assert!(mutation_diff.contains("diff --git a/two.txt b/two.txt"));
        assert!(mutation_diff.contains("--- a/one.txt"), "{mutation_diff}");
        assert!(mutation_diff.contains("+++ b/two.txt"), "{mutation_diff}");
        let patch_result = parse_patch_result(result);
        let mut touched = patch_result.touched_files.clone();
        touched.sort();
        assert_eq!(touched, vec!["one.txt", "two.txt"]);
        assert_eq!(patch_result.hunks_total, 0);
        assert_eq!(
            fs::read_to_string(tmp.path().join("one.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("two.txt")).unwrap(),
            "second\n"
        );
    }

    #[tokio::test]
    async fn test_apply_patch_legacy_changes_list() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("legacy.txt"), "old\n").expect("write");
        ctx.note_file_read(&ctx.resolve_path("legacy.txt").expect("resolve"));

        let result = ApplyPatchTool
            .execute(
                json!({
                    "changes": [
                        { "path": "legacy.txt", "content": "new\n" }
                    ]
                }),
                &ctx,
            )
            .await
            .expect("legacy changes alias should execute");

        assert!(result.success);
        assert_eq!(
            fs::read_to_string(tmp.path().join("legacy.txt")).unwrap(),
            "new\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_rejects_every_mixed_mode_before_writing() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("guard.txt"), "old\n").expect("write");
        let patch = "--- a/guard.txt\n+++ b/guard.txt\n@@ -1 +1 @@\n-old\n+patched\n";
        let replacement = json!([{
            "path": "guard.txt",
            "content": "replaced\n"
        }]);
        let cases = [
            (
                ["patch", "replace"],
                json!({"patch": patch, "replace": replacement.clone()}),
            ),
            (
                ["patch", "changes"],
                json!({"patch": patch, "changes": replacement.clone()}),
            ),
            (
                ["replace", "changes"],
                json!({
                    "replace": replacement.clone(),
                    "changes": replacement.clone()
                }),
            ),
        ];

        for (fields, input) in cases {
            let err = ApplyPatchTool
                .execute(input, &ctx)
                .await
                .expect_err("mixed modes must be rejected");
            let ToolError::InvalidInput { message } = err else {
                panic!("mixed modes should be invalid input, got: {err}");
            };
            assert!(message.contains("simultaneously"), "{message}");
            for field in fields {
                assert!(message.contains(field), "{message}");
            }
            assert_eq!(
                fs::read_to_string(tmp.path().join("guard.txt")).unwrap(),
                "old\n",
                "mixed modes must be rejected before the first write"
            );
        }
    }

    #[tokio::test]
    async fn test_apply_patch_replace_list_rolls_back_on_write_failure() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("one.txt"), "old\n").expect("write");
        ctx.note_file_read(&ctx.resolve_path("one.txt").expect("resolve"));
        fs::write(tmp.path().join("blocked"), "not a dir\n").expect("write blocker");

        let tool = ApplyPatchTool;
        let err = tool
            .execute(
                json!({
                    "replace": [
                        { "path": "one.txt", "content": "new\n" },
                        { "path": "blocked/two.txt", "content": "second\n" }
                    ]
                }),
                &ctx,
            )
            .await
            .expect_err("second write should fail");

        let message = err.to_string();
        assert!(message.contains("blocked"), "{message}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("one.txt")).unwrap(),
            "old\n"
        );
        assert!(!tmp.path().join("blocked").join("two.txt").exists());
    }

    #[tokio::test]
    async fn test_apply_patch_multi_file_diff() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        fs::write(tmp.path().join("a.txt"), "line1\nline2\n").expect("write");
        fs::write(tmp.path().join("b.txt"), "alpha\nbeta\n").expect("write");

        let patch = r"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 line1
-line2
+line2-mod
diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,3 @@
 alpha
+beta2
 beta
";

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let metadata = result.metadata.as_ref().expect("metadata");
        assert_eq!(metadata["event"], "apply_patch.preflight");
        assert_eq!(metadata["touched_files"], json!(["a.txt", "b.txt"]));
        assert_eq!(metadata["files_total"], 2);
        assert_eq!(metadata["hunks_total"], 2);
        assert!(metadata.get("path_override").is_none());
        let patch_result = parse_patch_result(result);
        let mut touched = patch_result.touched_files.clone();
        touched.sort();
        assert_eq!(touched, vec!["a.txt", "b.txt"]);
        assert_eq!(patch_result.files_applied, 2);
        let a = fs::read_to_string(tmp.path().join("a.txt")).unwrap();
        let b = fs::read_to_string(tmp.path().join("b.txt")).unwrap();
        assert!(a.contains("line2-mod"));
        assert!(b.contains("beta2"));
    }

    #[tokio::test]
    async fn mutation_receipt_covers_delete_rename_and_multifile_outcomes() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("old.txt"), "same\n").expect("old");
        fs::write(tmp.path().join("update.txt"), "before\n").expect("update");
        fs::write(tmp.path().join("delete.txt"), "gone\n").expect("delete");

        let patch = r"diff --git a/old.txt b/old.txt
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-same
diff --git a/new.txt b/new.txt
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+same
diff --git a/update.txt b/update.txt
--- a/update.txt
+++ b/update.txt
@@ -1 +1 @@
-before
+after
diff --git a/create.txt b/create.txt
--- /dev/null
+++ b/create.txt
@@ -0,0 +1 @@
+fresh
diff --git a/delete.txt b/delete.txt
--- a/delete.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone
";

        let result = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect("execute");
        let mutation = &result.metadata.as_ref().expect("metadata")["mutation"];
        assert_eq!(
            mutation["files"],
            json!([
                on_disk_receipt(tmp.path(), "update.txt", "updated"),
                on_disk_receipt(tmp.path(), "create.txt", "created"),
                // A deleted entry carries neither size nor sha256.
                { "path": "delete.txt", "outcome": "deleted" }
            ])
        );
        // The rename carries the destination's written bytes.
        let renamed = fs::read(tmp.path().join("new.txt")).expect("renamed bytes");
        assert_eq!(
            mutation["renames"],
            json!([{
                "from": "old.txt",
                "to": "new.txt",
                "size": renamed.len(),
                "sha256": codewhale_runtime::hashing::sha256_hex(&renamed),
            }])
        );
        let exact = mutation["diff"].as_str().expect("exact mutation diff");
        assert!(exact.contains("rename from old.txt"), "{exact}");
        assert!(exact.contains("rename to new.txt"), "{exact}");
        assert!(exact.contains("--- a/update.txt"), "{exact}");
        assert!(exact.contains("+++ b/create.txt"), "{exact}");
        assert!(exact.contains("--- a/delete.txt"), "{exact}");

        assert!(!tmp.path().join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(tmp.path().join("new.txt")).expect("renamed target"),
            "same\n"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("update.txt")).expect("updated"),
            "after\n"
        );
        assert!(tmp.path().join("create.txt").exists());
        assert!(!tmp.path().join("delete.txt").exists());
    }

    #[tokio::test]
    async fn test_apply_patch_requires_headers_without_path() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        let patch = r"@@ -1,1 +1,1 @@
-old
+new
";

        let err = tool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput { message } => {
                assert!(message.contains("no file headers"));
                assert!(message.contains("Provide `path`"));
            }
            other => panic!("expected invalid input, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_path_override_rejects_multi_file_diff() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        let patch = r"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,1 +1,1 @@
-one
+one-mod
diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -1,1 +1,1 @@
-two
+two-mod
";

        let err = tool
            .execute(json!({"path": "a.txt", "patch": patch}), &ctx)
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput { message } => {
                assert!(message.contains("multiple files"));
                assert!(message.contains("a.txt"));
                assert!(message.contains("b.txt"));
            }
            other => panic!("expected invalid input, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_apply_patch_summary_reports_fuzz() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        fs::write(tmp.path().join("test.txt"), "line0\nline1\nline2\nline3\n").expect("write");

        let patch = r"@@ -1,2 +1,2 @@
-line1
+modified
 line2
";

        let result = tool
            .execute(json!({"path": "test.txt", "patch": patch, "fuzz": 3}), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        let patch_result = parse_patch_result(result);
        assert_eq!(patch_result.hunks_with_fuzz, 1);
        assert!(patch_result.fuzz_used > 0);
        assert!(patch_result.message.contains("Fuzz used"));
        let summary = patch_result.file_summaries.first().unwrap();
        assert_eq!(summary.hunks_with_fuzz, 1);
    }

    #[tokio::test]
    async fn test_path_override_header_mismatch_note() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        fs::write(tmp.path().join("override.txt"), "old\n").expect("write");

        let patch = r"--- a/other.txt
+++ b/other.txt
@@ -1,1 +1,1 @@
-old
+new
";

        let result = tool
            .execute(json!({"path": "override.txt", "patch": patch}), &ctx)
            .await
            .expect("execute");
        let metadata = result.metadata.as_ref().expect("metadata");
        assert!(
            metadata["header_path_mismatch"]
                .as_str()
                .unwrap()
                .contains("headers reference `other.txt`")
        );
        let patch_result = parse_patch_result(result);
        assert!(
            patch_result
                .message
                .contains("headers reference `other.txt`")
        );
        assert!(
            patch_result
                .message
                .contains("path` overrides to `override.txt`")
        );
    }

    #[test]
    fn test_apply_patch_tool_properties() {
        let tool = ApplyPatchTool;
        assert_eq!(tool.name(), "apply_patch");
        assert!(!tool.is_read_only());
        assert!(tool.is_sandboxable());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Suggest);
    }

    #[test]
    fn test_multi_hunk_offset_tracking() {
        // File with 6 lines
        let mut lines: Vec<String> = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
            "line4".to_string(),
            "line5".to_string(),
            "line6".to_string(),
        ];

        // Hunk 1: Add 2 lines after line1 (offset becomes +2)
        let hunk1 = Hunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 4,
            lines: vec![
                HunkLine::Context("line1".to_string()),
                HunkLine::Add("new_a".to_string()),
                HunkLine::Add("new_b".to_string()),
                HunkLine::Context("line2".to_string()),
            ],
        };

        // Hunk 2: Modify line5 (originally at position 5, now at position 7 due to +2 offset)
        let hunk2 = Hunk {
            old_start: 5, // Original position in the diff
            old_count: 1,
            new_start: 7,
            new_count: 1,
            lines: vec![
                HunkLine::Remove("line5".to_string()),
                HunkLine::Add("modified5".to_string()),
            ],
        };

        let mut offset: isize = 0;

        // Apply first hunk
        let outcome1 = apply_hunk(&mut lines, &hunk1, 3, &mut offset).unwrap();
        assert_eq!(outcome1.fuzz_used, 0);
        assert!(!outcome1.relocated);
        assert_eq!(offset, 2); // Added 2 lines (4 new - 2 old)
        assert_eq!(
            lines,
            vec![
                "line1", "new_a", "new_b", "line2", "line3", "line4", "line5", "line6"
            ]
        );

        // Apply second hunk - this would fail without offset tracking!
        let outcome2 = apply_hunk(&mut lines, &hunk2, 3, &mut offset).unwrap();
        assert_eq!(outcome2.fuzz_used, 0);
        assert!(!outcome2.relocated);
        assert!(lines.contains(&"modified5".to_string()));
        assert!(!lines.contains(&"line5".to_string()));
    }

    #[test]
    fn test_apply_hunk_relocates_to_unique_context_match() {
        // #5003 - stale line numbers (hunk says line 1, content is at line 20).
        let mut lines: Vec<String> = (0..25).map(|i| format!("line{i}")).collect();
        let hunk = Hunk {
            old_start: 1, // stale line number
            old_count: 5,
            new_start: 1,
            new_count: 5,
            lines: vec![
                HunkLine::Context("line19".to_string()),
                HunkLine::Context("line20".to_string()),
                HunkLine::Remove("line21".to_string()),
                HunkLine::Add("line21-modified".to_string()),
                HunkLine::Context("line22".to_string()),
                HunkLine::Context("line23".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let outcome = apply_hunk(&mut lines, &hunk, 1, &mut offset).unwrap();
        assert!(
            outcome.relocated,
            "expected relocation for stale line numbers"
        );
        assert_eq!(outcome.fuzz_used, 0);
        assert_eq!(lines[21], "line21-modified");
        assert!(!lines.contains(&"line21".to_string()));
    }

    #[test]
    fn test_apply_hunk_ambiguous_context_reports_candidates() {
        // Two identical block-a..d blocks: anchor is not unique -> ContextAmbiguous.
        let mut lines: Vec<String> = [
            "header0", "header1", "header2", "block-a", "block-b", "block-c", "block-d", "middle",
            "block-a", "block-b", "block-c", "block-d", "footer",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let hunk = Hunk {
            old_start: 1,
            old_count: 4,
            new_start: 1,
            new_count: 4,
            lines: vec![
                HunkLine::Remove("block-a".to_string()),
                HunkLine::Remove("block-b".to_string()),
                HunkLine::Remove("block-c".to_string()),
                HunkLine::Remove("block-d".to_string()),
                HunkLine::Add("block-a-modified".to_string()),
                HunkLine::Add("block-b".to_string()),
                HunkLine::Add("block-c".to_string()),
                HunkLine::Add("block-d".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let err = apply_hunk(&mut lines, &hunk, 1, &mut offset).unwrap_err();
        match err {
            ApplyHunkError::ContextAmbiguous {
                expected_line,
                candidate_lines,
            } => {
                assert_eq!(expected_line, 1);
                // The two duplicate blocks start at 1-based lines 4 and 9.
                assert_eq!(candidate_lines, vec![4, 9]);
            }
            other => panic!("expected ContextAmbiguous, got: {other:?}"),
        }
    }

    #[test]
    fn test_apply_hunk_short_anchor_not_relocated() {
        // Anchor too short (1 line < MIN_ANCHOR_LINES): keep NoMatch behavior.
        let mut lines: Vec<String> = ["zero", "one", "two", "three", "four"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hunk = Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            lines: vec![
                HunkLine::Remove("four".to_string()),
                HunkLine::Add("four-modified".to_string()),
            ],
        };

        let mut offset: isize = 0;
        let err = apply_hunk(&mut lines, &hunk, 1, &mut offset).unwrap_err();
        assert!(
            matches!(err, ApplyHunkError::NoMatch { .. }),
            "short anchors must not be relocated"
        );
    }

    #[tokio::test]
    async fn test_apply_patch_relocates_stale_line_numbers() {
        // #5003 - integration: hunk claims line 1 but content is at line 21.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        let content = (0..30)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(tmp.path().join("stale.txt"), &content).expect("write");

        let patch = r"@@ -1,5 +1,5 @@
 line19
 line20
-line21
+line21-modified
 line22
 line23
";

        let result = tool
            .execute(
                json!({"path": "stale.txt", "patch": patch, "fuzz": 1}),
                &ctx,
            )
            .await
            .expect("execute");
        assert!(result.success);
        let patch_result = parse_patch_result(result);
        assert_eq!(patch_result.hunks_relocated, 1);
        assert!(
            patch_result.message.contains("stale line numbers"),
            "message: {}",
            patch_result.message
        );
        let summary = patch_result.file_summaries.first().unwrap();
        assert_eq!(summary.hunks_relocated, 1);

        let edited = fs::read_to_string(tmp.path().join("stale.txt")).expect("read");
        assert!(edited.contains("line21-modified"));
        assert!(!edited.contains("\nline21\n"));
    }

    #[tokio::test]
    async fn test_apply_patch_ambiguous_context_reports_candidates() {
        // #5003 - integration: duplicate context blocks, ambiguous relocation.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        let content = "header0\nheader1\nheader2\nblock-a\nblock-b\nblock-c\nblock-d\nmiddle\nblock-a\nblock-b\nblock-c\nblock-d\nfooter\n";
        fs::write(tmp.path().join("dup.txt"), content).expect("write");

        let patch = r"@@ -1,4 +1,4 @@
-block-a
-block-b
-block-c
-block-d
+block-a-modified
+block-b
+block-c
+block-d
";

        let err = tool
            .execute(json!({"path": "dup.txt", "patch": patch, "fuzz": 1}), &ctx)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("multiple locations"),
            "expected ambiguity error, got: {message}"
        );
        // The two duplicate blocks start at 1-based lines 4 and 9.
        assert!(
            message.contains("lines 4, 9"),
            "expected candidate lines, got: {message}"
        );
        let unchanged = fs::read_to_string(tmp.path().join("dup.txt")).expect("read");
        assert_eq!(
            unchanged, content,
            "ambiguous hunk must not modify the file"
        );
    }
    #[tokio::test]
    async fn test_apply_patch_relocates_after_crlf_chinese_edit_shift() {
        // #5003 - issue scenario: a C-style file with CRLF line endings and
        // Chinese comments. A first edit shifts line numbers; a second patch
        // still uses stale line numbers but its context is unique in the
        // file, so apply_patch relocates instead of failing.
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ApplyPatchTool;

        let mut lines = vec!["/* 扭矩控制模块 */".to_string()];
        for i in 0..60 {
            lines.push(format!("int cfg_{i} = {i}; // 配置项 {i}"));
        }
        let content = lines.join("\r\n") + "\r\n";
        fs::write(tmp.path().join("app_foc.c"), &content).expect("write");

        // First edit: insert 8 lines right after cfg_9 (old line 11). This
        // shifts every later line number by +8.
        let patch1 = r"@@ -11,1 +11,9 @@
 int cfg_9 = 9; // 配置项 9
+int new_a = 100; // 新增配置 A
+int new_b = 101; // 新增配置 B
+int new_c = 102; // 新增配置 C
+int new_d = 103; // 新增配置 D
+int new_e = 104; // 新增配置 E
+int new_f = 105; // 新增配置 F
+int new_g = 106; // 新增配置 G
+int new_h = 107; // 新增配置 H
";
        let r1 = tool
            .execute(
                json!({"path": "app_foc.c", "patch": patch1, "fuzz": 0}),
                &ctx,
            )
            .await
            .expect("first patch");
        assert!(r1.success, "first patch should apply: {}", r1.content);

        // Second edit: claims line 31 (stale - cfg_21 now), but the replaced
        // block cfg_30..cfg_34 actually lives at lines 40-44. Positional
        // search with fuzz=1 fails; the unique whole-file anchor relocates.
        let patch2 = r"@@ -31,5 +31,5 @@
 int cfg_30 = 30; // 配置项 30
 int cfg_31 = 31; // 配置项 31
-int cfg_32 = 32; // 配置项 32
+int cfg_32 = 3200; // 配置项 32 已修改
 int cfg_33 = 33; // 配置项 33
 int cfg_34 = 34; // 配置项 34
";
        let r2 = tool
            .execute(
                json!({"path": "app_foc.c", "patch": patch2, "fuzz": 1}),
                &ctx,
            )
            .await
            .expect("second patch");
        assert!(r2.success, "second patch should relocate: {}", r2.content);
        let pr2 = parse_patch_result(r2);
        assert_eq!(pr2.hunks_relocated, 1, "second patch must be relocated");

        let edited = fs::read_to_string(tmp.path().join("app_foc.c")).expect("read");
        assert!(
            edited.contains("int cfg_32 = 3200;"),
            "replacement must land"
        );
        assert!(
            edited.contains("int cfg_33 = 33;"),
            "context after the edit must stay intact"
        );
        assert!(
            edited.contains("int new_h = 107;"),
            "first edit must survive"
        );
    }

    // === Content-hash patch guard (#3979) ===
    //
    // The guard is a whole-patch precondition on the patch target, not a
    // per-file check — see `verify_patch_expected_hash`. These pin that it
    // fires before anything is written.

    const GUARD_PATCH: &str = "--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,3 @@
 line1
-line2
+modified
 line3
";

    #[tokio::test]
    async fn patch_with_matching_expected_hash_proceeds() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let body = "line1\nline2\nline3\n";
        fs::write(tmp.path().join("test.txt"), body).expect("write");

        ApplyPatchTool
            .execute(
                json!({
                    "path": "test.txt",
                    "patch": GUARD_PATCH,
                    "expected_hash": content_hash(body.as_bytes()),
                }),
                &ctx,
            )
            .await
            .expect("matching hash must not block the patch");

        let updated = fs::read_to_string(tmp.path().join("test.txt")).expect("read");
        assert!(updated.contains("modified"), "{updated}");
    }

    #[tokio::test]
    async fn patch_with_stale_expected_hash_rejects_without_writing() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let body = "line1\nline2\nline3\n";
        fs::write(tmp.path().join("test.txt"), body).expect("write");

        let err = ApplyPatchTool
            .execute(
                json!({
                    "path": "test.txt",
                    "patch": GUARD_PATCH,
                    "expected_hash": content_hash(b"a different file entirely\n"),
                }),
                &ctx,
            )
            .await
            .expect_err("stale hash must reject");

        let message = err.to_string();
        assert!(message.contains("changed since it was read"), "{message}");
        assert!(message.contains("nothing was written"), "{message}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("test.txt")).expect("read"),
            body,
            "a rejected patch must not modify the file"
        );
    }

    #[tokio::test]
    async fn patch_without_expected_hash_is_unchanged() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("test.txt"), "line1\nline2\nline3\n").expect("write");

        ApplyPatchTool
            .execute(json!({ "path": "test.txt", "patch": GUARD_PATCH }), &ctx)
            .await
            .expect("absent expected_hash keeps the pre-#3979 behavior");

        let updated = fs::read_to_string(tmp.path().join("test.txt")).expect("read");
        assert!(updated.contains("modified"), "{updated}");
    }

    #[tokio::test]
    async fn multi_file_patch_guards_the_first_file_and_writes_nothing_on_mismatch() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("one.txt"), "one\n").expect("write");
        fs::write(tmp.path().join("two.txt"), "two\n").expect("write");

        let patch = "diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-one
+ONE
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1 +1 @@
-two
+TWO
";

        let err = ApplyPatchTool
            .execute(
                json!({
                    "patch": patch,
                    "expected_hash": content_hash(b"stale\n"),
                }),
                &ctx,
            )
            .await
            .expect_err("stale hash on the first file must reject the whole patch");
        assert!(err.to_string().contains("one.txt"), "{err}");

        // Transactional: the unguarded second file must not have been
        // written either.
        assert_eq!(
            fs::read_to_string(tmp.path().join("one.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("two.txt")).unwrap(),
            "two\n"
        );
    }

    #[tokio::test]
    async fn patch_expected_hash_on_a_missing_target_fails_closed() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let err = ApplyPatchTool
            .execute(
                json!({
                    "path": "absent.txt",
                    "patch": GUARD_PATCH,
                    "create_if_missing": true,
                    "expected_hash": content_hash(b"anything"),
                }),
                &ctx,
            )
            .await
            .expect_err("guarded patch of a missing file must fail closed");

        assert!(err.to_string().contains("does not exist"), "{err}");
        assert!(!tmp.path().join("absent.txt").exists());
    }

    /// B7: two sections for the same file both land; the second applies on
    /// top of the first instead of re-reading the disk and dropping it.
    #[tokio::test]
    async fn two_sections_for_one_file_apply_both() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("f.txt"), "a1\na2\na3\nmid\nb1\nb2\nb3\n").expect("write");
        let patch = r"--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 a1
-a2
+A2
 a3
--- a/f.txt
+++ b/f.txt
@@ -5,3 +5,3 @@
 b1
-b2
+B2
 b3
";
        let result = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect("execute");
        assert!(result.success, "{}", result.content);
        assert_eq!(
            fs::read_to_string(tmp.path().join("f.txt")).expect("read"),
            "a1\nA2\na3\nmid\nb1\nB2\nb3\n"
        );
        let summary = parse_patch_result(result);
        assert_eq!(summary.files_total, 1, "one file, two sections");
        assert_eq!(summary.files_applied, 1);
        assert_eq!(summary.hunks_applied, 2);
    }

    #[tokio::test]
    async fn creation_patch_on_an_existing_file_is_refused() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("foo.rs"), "fn keep() {}\n").expect("write");
        let patch = "--- /dev/null\n+++ b/foo.rs\n@@ -0,0 +1,2 @@\n+fn a() {}\n+fn b() {}\n";

        let err = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect_err("a /dev/null creation must not land on an existing file");

        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("foo.rs")).expect("read"),
            "fn keep() {}\n"
        );
    }

    #[tokio::test]
    async fn pure_insertion_hunk_lands_after_the_named_line() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("f.txt"), "l1\nl2\nl3\nl4\n").expect("write");
        // `git diff -U0` form: insert after line 3.
        let patch = "@@ -3,0 +4,1 @@\n+inserted\n";

        ApplyPatchTool
            .execute(json!({"path": "f.txt", "patch": patch}), &ctx)
            .await
            .expect("execute");

        assert_eq!(
            fs::read_to_string(tmp.path().join("f.txt")).expect("read"),
            "l1\nl2\nl3\ninserted\nl4\n"
        );
    }

    #[tokio::test]
    async fn delete_section_covering_part_of_a_file_is_refused() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let original = "1\n2\n3\n4\n5\n";
        fs::write(tmp.path().join("big.txt"), original).expect("write");
        let patch = "--- a/big.txt\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-1\n-2\n-3\n";

        let err = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect_err("a partial delete must not remove the file");

        assert!(err.to_string().contains("2 line(s)"), "{err}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("big.txt")).expect("still there"),
            original
        );
    }

    #[tokio::test]
    async fn creation_patch_with_a_path_override_on_an_existing_file_is_refused() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("foo.rs"), "fn keep() {}\n").expect("write");
        let patch = "--- /dev/null\n+++ b/foo.rs\n@@ -0,0 +1,2 @@\n+fn a() {}\n+fn b() {}\n";

        let err = ApplyPatchTool
            .execute(json!({"path": "foo.rs", "patch": patch}), &ctx)
            .await
            .expect_err("`path` must not turn a /dev/null creation into a prepend");

        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("foo.rs")).expect("read"),
            "fn keep() {}\n"
        );
    }

    #[tokio::test]
    async fn creation_section_after_an_edit_of_the_same_file_is_refused() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("foo.txt"), "a\nb\n").expect("write");
        let patch = "--- a/foo.txt\n+++ b/foo.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n\
                     --- /dev/null\n+++ b/foo.txt\n@@ -0,0 +1,1 @@\n+new\n";

        let err = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx)
            .await
            .expect_err("a second, creating section must not prepend to the edited file");

        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("foo.txt")).expect("read"),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn insertion_hunks_without_a_zero_old_count_keep_the_before_anchor() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        for (patch, expected) in [
            // Omitted old count (defaults to 1): before line 2, as before.
            ("@@ -2 +2,1 @@\n+x\n", "l1\nx\nl2\nl3\n"),
            // Hand-written "new text starts at line 1": at the top.
            ("@@ -1,0 +1,1 @@\n+x\n", "x\nl1\nl2\nl3\n"),
            // `git diff -U0` form: after line 1.
            ("@@ -1,0 +2,1 @@\n+x\n", "l1\nx\nl2\nl3\n"),
        ] {
            fs::write(tmp.path().join("f.txt"), "l1\nl2\nl3\n").expect("write");
            ApplyPatchTool
                .execute(json!({"path": "f.txt", "patch": patch}), &ctx)
                .await
                .expect("execute");
            assert_eq!(
                fs::read_to_string(tmp.path().join("f.txt")).expect("read"),
                expected,
                "{patch:?}"
            );
        }
    }
}

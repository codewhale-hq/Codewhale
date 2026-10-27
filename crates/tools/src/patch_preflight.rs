use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ToolError, optional_bool, optional_str};

const FILE_LIST_LIMIT: usize = 6;

/// No-mutation summary of what an `apply_patch` input intends to touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyPatchPreflight {
    pub touched_files: Vec<String>,
    pub files_total: usize,
    pub hunks_total: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deletes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_path_mismatch: Option<String>,
}

/// A single hunk in a unified diff
#[derive(Debug, Clone)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<HunkLine>,
}

/// A line in a hunk
#[derive(Debug, Clone)]
pub enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

#[derive(Debug, Clone)]
pub struct FilePatch {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub delete_after: bool,
    pub create_if_missing: bool,
    /// The section's old header is `/dev/null`: it creates the file and has
    /// nothing to anchor against, so it must not land on an existing one.
    pub creates_new_file: bool,
}

#[derive(Debug, Default, Clone)]
struct PatchShape {
    has_hunks: bool,
    header_files: Vec<String>,
    /// Some section's old header is `/dev/null` (a file creation).
    creates_new_file: bool,
}

impl PatchShape {
    fn file_count(&self) -> usize {
        self.header_files.len()
    }
}

#[derive(Debug, Clone)]
pub enum ApplyPatchPreflightKind {
    Replace,
    PathOverride {
        path: String,
        hunks: Vec<Hunk>,
        creates_new_file: bool,
    },
    FilePatches(Vec<FilePatch>),
}

/// Canonicalized `apply_patch` payload mode.
///
/// `replace` is the preferred spelling for full-file replacements. `changes`
/// remains a compatibility alias for callers that learned the original tool
/// schema before the clearer name was introduced.
#[derive(Debug, Clone, Copy)]
pub enum NormalizedApplyPatchInput<'a> {
    Patch(&'a str),
    Replacement {
        entries: &'a [Value],
        source_field: &'static str,
    },
}

/// Validate mutual exclusivity and normalize the legacy `changes` alias.
///
/// This is the single parser used by execution, preflight, policy, approval,
/// and UI consumers so every surface agrees on the accepted input contract.
pub fn normalize_apply_patch_input(
    input: &Value,
) -> Result<NormalizedApplyPatchInput<'_>, ToolError> {
    let provided: Vec<&'static str> = ["patch", "replace", "changes"]
        .into_iter()
        .filter(|field| input.get(*field).is_some())
        .collect();

    if provided.len() > 1 {
        let fields = provided
            .iter()
            .map(|field| format!("`{field}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ToolError::invalid_input(format!(
            "Cannot use {fields} simultaneously. Choose exactly one of `patch`, `replace`, or the deprecated `changes` alias."
        )));
    }

    let Some(field) = provided.first().copied() else {
        return Err(ToolError::missing_field(
            "patch, replace, or deprecated changes",
        ));
    };

    if field == "patch" {
        let patch = input
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid_input("`patch` must be a string"))?;
        return Ok(NormalizedApplyPatchInput::Patch(patch));
    }

    let entries = input.get(field).and_then(Value::as_array).ok_or_else(|| {
        ToolError::invalid_input(format!(
            "`{field}` must be an array of objects like {{path, content}}"
        ))
    })?;
    if entries.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "`{field}` cannot be empty"
        )));
    }

    Ok(NormalizedApplyPatchInput::Replacement {
        entries,
        source_field: field,
    })
}

#[derive(Debug, Clone)]
pub struct ApplyPatchPreflightPlan {
    pub summary: ApplyPatchPreflight,
    pub kind: ApplyPatchPreflightKind,
}

/// Parse `apply_patch` input into a reusable, no-mutation preflight summary.
///
/// This deliberately stops before workspace resolution or file reads. It is
/// suitable for policy checks, audit logs, diagnostics hooks, and future undo
/// planning that must know the target files before mutation.
pub fn preflight_apply_patch(input: &Value) -> Result<ApplyPatchPreflight, ToolError> {
    let normalized = normalize_apply_patch_input(input)?;
    Ok(preflight_apply_patch_plan(input, normalized)?.summary)
}

pub fn preflight_apply_patch_plan(
    input: &Value,
    normalized: NormalizedApplyPatchInput<'_>,
) -> Result<ApplyPatchPreflightPlan, ToolError> {
    let create_if_missing = optional_bool(input, "create_if_missing", false)?;

    if let NormalizedApplyPatchInput::Replacement {
        entries,
        source_field,
    } = normalized
    {
        return Ok(ApplyPatchPreflightPlan {
            summary: preflight_replace(entries, source_field)?,
            kind: ApplyPatchPreflightKind::Replace,
        });
    }

    let NormalizedApplyPatchInput::Patch(patch_text) = normalized else {
        unreachable!("replacement input returned before patch parsing")
    };
    let path_override = optional_str(input, "path")?;
    let patch_shape = inspect_patch_shape(patch_text);
    validate_patch_shape(&patch_shape, path_override)?;
    let header_path_mismatch =
        path_override.and_then(|path| diff_header_mismatch(path, &patch_shape));

    if let Some(path) = path_override {
        let hunks = parse_unified_diff(patch_text)?;
        if hunks.is_empty() {
            return Err(ToolError::invalid_input(
                "Patch did not contain any hunks (`@@ ... @@`). Provide a unified diff hunk.",
            ));
        }
        return Ok(ApplyPatchPreflightPlan {
            summary: ApplyPatchPreflight {
                touched_files: vec![path.to_string()],
                files_total: 1,
                hunks_total: hunks.len(),
                creates: if create_if_missing {
                    vec![path.to_string()]
                } else {
                    Vec::new()
                },
                deletes: Vec::new(),
                path_override: Some(path.to_string()),
                header_path_mismatch,
            },
            kind: ApplyPatchPreflightKind::PathOverride {
                path: path.to_string(),
                hunks,
                // `path` retargets the section; a `--- /dev/null` header
                // still means "create", and must not land on an existing file.
                creates_new_file: patch_shape.creates_new_file,
            },
        });
    }

    let file_patches = parse_unified_diff_files(patch_text, create_if_missing)?;
    if file_patches.is_empty() {
        return Err(ToolError::invalid_input(
            "No valid file patches found. Ensure the patch includes `---`/`+++` headers or provide `path`.",
        ));
    }

    let mut touched_files = Vec::new();
    let mut creates = Vec::new();
    let mut deletes = Vec::new();
    let mut hunks_total = 0;
    for file_patch in &file_patches {
        if file_patch.hunks.is_empty() {
            return Err(ToolError::invalid_input(format!(
                "Patch section for `{}` has no hunks (`@@ ... @@`).",
                file_patch.path
            )));
        }
        push_unique(&mut touched_files, file_patch.path.clone());
        hunks_total += file_patch.hunks.len();
        if file_patch.create_if_missing && !file_patch.delete_after {
            push_unique(&mut creates, file_patch.path.clone());
        }
        if file_patch.delete_after {
            push_unique(&mut deletes, file_patch.path.clone());
        }
    }

    Ok(ApplyPatchPreflightPlan {
        summary: ApplyPatchPreflight {
            files_total: file_patches.len(),
            touched_files,
            hunks_total,
            creates,
            deletes,
            path_override: None,
            header_path_mismatch,
        },
        kind: ApplyPatchPreflightKind::FilePatches(file_patches),
    })
}

fn preflight_replace(
    changes: &[Value],
    source_field: &str,
) -> Result<ApplyPatchPreflight, ToolError> {
    let mut touched_files = Vec::new();
    for change in changes {
        let path = change
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field(format!("{source_field}[].path")))?;
        let _content = change
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field(format!("{source_field}[].content")))?;
        push_unique(&mut touched_files, path.to_string());
    }

    Ok(ApplyPatchPreflight {
        files_total: changes.len(),
        touched_files,
        hunks_total: 0,
        creates: Vec::new(),
        deletes: Vec::new(),
        path_override: None,
        header_path_mismatch: None,
    })
}

/// Parse a unified diff into hunks
pub fn parse_unified_diff(patch: &str) -> Result<Vec<Hunk>, ToolError> {
    let mut hunks = Vec::new();
    let mut lines = patch.lines().peekable();

    // Skip header lines (---, +++ etc)
    while let Some(line) = lines.peek() {
        if line.starts_with("@@") {
            break;
        }
        lines.next();
    }

    // Parse hunks
    while let Some(line) = lines.next() {
        if line.starts_with("@@") {
            let hunk = parse_hunk_header(line, &mut lines)?;
            hunks.push(hunk);
        }
    }

    Ok(hunks)
}

fn parse_unified_diff_files(
    patch: &str,
    create_if_missing: bool,
) -> Result<Vec<FilePatch>, ToolError> {
    let mut files = Vec::new();
    let mut lines = patch.lines().peekable();
    let mut current: Option<FilePatch> = None;
    let mut old_path: Option<String> = None;

    while let Some(line) = lines.next() {
        if line.starts_with("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            old_path = None;
            continue;
        }

        if let Some(stripped) = line.strip_prefix("--- ") {
            old_path = Some(stripped.trim().to_string());
            continue;
        }

        if let Some(stripped) = line.strip_prefix("+++ ") {
            let new_path = Some(stripped.trim().to_string());
            let (path, delete_after, create_flag) =
                resolve_diff_paths(old_path.as_deref(), new_path.as_deref(), create_if_missing)?;
            let creates_new_file = old_path.as_deref().is_some_and(is_dev_null_header);
            old_path = None;
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(FilePatch {
                path,
                hunks: Vec::new(),
                delete_after,
                create_if_missing: create_flag,
                creates_new_file,
            });
            continue;
        }

        if line.starts_with("@@") {
            let Some(file) = current.as_mut() else {
                if let Some(path) = old_path.as_deref() {
                    return Err(ToolError::invalid_input(format!(
                        "Patch hunk encountered after `--- {path}` but before a matching `+++` header. Each file section must include both headers."
                    )));
                }
                return Err(ToolError::invalid_input(
                    "Patch hunk encountered before any file header. Add `---`/`+++` headers or provide `path`.",
                ));
            };
            let hunk = parse_hunk_header(line, &mut lines)?;
            file.hunks.push(hunk);
        }
    }

    if let Some(file) = current {
        files.push(file);
    }

    Ok(files)
}

fn resolve_diff_paths(
    old_path: Option<&str>,
    new_path: Option<&str>,
    create_if_missing: bool,
) -> Result<(String, bool, bool), ToolError> {
    let old_norm = old_path.and_then(normalize_diff_path);
    let new_norm = new_path.and_then(normalize_diff_path);
    let delete_after = new_norm.is_none();
    let create_flag = create_if_missing || old_norm.is_none();
    let path = new_norm
        .or(old_norm)
        .ok_or_else(|| ToolError::invalid_input("Patch is missing both old and new file paths"))?;
    Ok((path, delete_after, create_flag))
}

fn is_dev_null_header(raw: &str) -> bool {
    let raw = raw.split_once('\t').map_or(raw, |(path, _timestamp)| path);
    matches!(raw.trim(), "/dev/null" | "dev/null")
}

fn normalize_diff_path(raw: &str) -> Option<String> {
    let raw = raw.split_once('\t').map_or(raw, |(path, _timestamp)| path);
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw == "/dev/null" || raw == "dev/null" {
        return None;
    }
    let raw = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    Some(raw.to_string())
}

/// Parse a hunk header and its content
fn parse_hunk_header<'a, I>(
    header: &str,
    lines: &mut std::iter::Peekable<I>,
) -> Result<Hunk, ToolError>
where
    I: Iterator<Item = &'a str> + Clone,
{
    // Parse @@ -old_start,old_count +new_start,new_count @@
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(ToolError::invalid_input(format!(
            "Invalid hunk header: {header}. Expected numeric unified-diff form `@@ -old_start,old_count +new_start,new_count @@` (example: `@@ -12,3 +12,5 @@`)."
        )));
    }

    let old_range = parts[1].trim_start_matches('-');
    let new_range = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    // Parse hunk lines
    let mut hunk_lines = Vec::new();
    let expected_lines = old_count.max(new_count) + old_count.min(new_count);
    let mut old_remaining = old_count;
    let mut new_remaining = new_count;

    for _ in 0..expected_lines * 2 {
        // `--- ` opens the next file section once this hunk's counts are
        // spent, or when `+++ ` follows (a header after a miscounted hunk).
        // Otherwise it is a removed line whose text starts `-- `. Reading it
        // as a removal unconditionally glued a second section for the same
        // file onto the first hunk (B7).
        let next_file_header = {
            let mut ahead = lines.clone();
            ahead.next().is_some_and(|line| line.starts_with("--- "))
                && ((old_remaining == 0 && new_remaining == 0)
                    || ahead.next().is_some_and(|line| line.starts_with("+++ ")))
        };
        if next_file_header {
            break;
        }
        // Allow for more lines than expected
        match lines.peek() {
            Some(line) if line.starts_with("@@") => break,
            Some(line) if line.starts_with('-') => {
                hunk_lines.push(HunkLine::Remove(line[1..].to_string()));
                old_remaining = old_remaining.saturating_sub(1);
                lines.next();
            }
            Some(line) if line.starts_with('+') => {
                hunk_lines.push(HunkLine::Add(line[1..].to_string()));
                new_remaining = new_remaining.saturating_sub(1);
                lines.next();
            }
            Some(line) if line.starts_with(' ') || line.is_empty() => {
                let content = if line.is_empty() { "" } else { &line[1..] };
                hunk_lines.push(HunkLine::Context(content.to_string()));
                old_remaining = old_remaining.saturating_sub(1);
                new_remaining = new_remaining.saturating_sub(1);
                lines.next();
            }
            Some(line)
                if line.starts_with("diff ")
                    || line.starts_with("--- ")
                    || line.starts_with("+++ ") =>
            {
                // Start of a new file patch - don't consume, let outer loop handle it
                break;
            }
            Some(line) if !line.starts_with('\\') => {
                // Treat as context line without leading space
                hunk_lines.push(HunkLine::Context((*line).to_string()));
                old_remaining = old_remaining.saturating_sub(1);
                new_remaining = new_remaining.saturating_sub(1);
                lines.next();
            }
            Some(_) => {
                lines.next(); // Skip "\ No newline at end of file" etc
            }
            None => break,
        }
    }

    Ok(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: hunk_lines,
    })
}

/// Parse a range like "10,5" or "10" into (start, count)
pub fn parse_range(range: &str) -> Result<(usize, usize), ToolError> {
    let parts: Vec<&str> = range.split(',').collect();
    let start = parts[0].parse::<usize>().map_err(|_| {
        ToolError::invalid_input(format!(
            "Invalid line number `{}` in hunk header. Expected numeric unified-diff form `@@ -old_start,old_count +new_start,new_count @@` (example: `@@ -12,3 +12,5 @@`); use positive integers like `12` or `12,3`.",
            parts[0]
        ))
    })?;
    let count = if parts.len() > 1 {
        parts[1].parse::<usize>().map_err(|_| {
            ToolError::invalid_input(format!(
                "Invalid line count `{}` in hunk header. Expected numeric unified-diff form `@@ -old_start,old_count +new_start,new_count @@` (example: `@@ -12,3 +12,5 @@`); use positive integers like `3`.",
                parts[1]
            ))
        })?
    } else {
        1
    };
    Ok((start, count))
}

fn inspect_patch_shape(patch: &str) -> PatchShape {
    let mut shape = PatchShape::default();
    let mut seen = HashSet::new();
    let mut old_path: Option<String> = None;
    let mut old_is_dev_null = false;
    let mut hunk_old_remaining = 0usize;
    let mut hunk_new_remaining = 0usize;

    for line in patch.lines() {
        if line.starts_with("@@") {
            shape.has_hunks = true;
            if let Some((old_count, new_count)) = hunk_line_counts_for_shape(line) {
                hunk_old_remaining = old_count;
                hunk_new_remaining = new_count;
            }
            continue;
        }

        if hunk_old_remaining > 0 || hunk_new_remaining > 0 {
            advance_hunk_shape_counts(line, &mut hunk_old_remaining, &mut hunk_new_remaining);
            continue;
        }

        if let Some(stripped) = line.strip_prefix("--- ") {
            old_is_dev_null = is_dev_null_header(stripped);
            old_path = normalize_diff_path(stripped);
            continue;
        }

        if let Some(stripped) = line.strip_prefix("+++ ") {
            shape.creates_new_file |= old_is_dev_null && !is_dev_null_header(stripped);
            old_is_dev_null = false;
            let new_path = normalize_diff_path(stripped);
            let resolved = new_path.or(old_path.clone());
            if let Some(path) = resolved
                && seen.insert(path.clone())
            {
                shape.header_files.push(path);
            }
            old_path = None;
        }
    }

    shape
}

fn hunk_line_counts_for_shape(header: &str) -> Option<(usize, usize)> {
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let (_, old_count) = parse_range(parts[1].trim_start_matches('-')).ok()?;
    let (_, new_count) = parse_range(parts[2].trim_start_matches('+')).ok()?;
    Some((old_count, new_count))
}

fn advance_hunk_shape_counts(line: &str, old_remaining: &mut usize, new_remaining: &mut usize) {
    if line.starts_with('\\') {
        return;
    }
    if line.starts_with('+') {
        *new_remaining = new_remaining.saturating_sub(1);
    } else if line.starts_with('-') {
        *old_remaining = old_remaining.saturating_sub(1);
    } else {
        *old_remaining = old_remaining.saturating_sub(1);
        *new_remaining = new_remaining.saturating_sub(1);
    }
}

fn validate_patch_shape(shape: &PatchShape, path_override: Option<&str>) -> Result<(), ToolError> {
    if !shape.has_hunks {
        return Err(ToolError::invalid_input(
            "Patch must include at least one hunk header in numeric unified-diff form (`@@ -old_start,old_count +new_start,new_count @@`, example: `@@ -12,3 +12,5 @@`).",
        ));
    }

    match path_override {
        Some(_) if shape.file_count() > 1 => Err(ToolError::invalid_input(format!(
            "Patch references multiple files ({}) but `path` was provided. Remove `path` to apply a multi-file patch, or provide a single-file patch.",
            format_file_list(&shape.header_files),
        ))),
        None if shape.file_count() == 0 => Err(ToolError::invalid_input(
            "Patch contains hunks but no file headers (`---`/`+++`). Provide `path` or add headers.",
        )),
        _ => Ok(()),
    }
}

fn diff_header_mismatch(path_override: &str, shape: &PatchShape) -> Option<String> {
    if shape.file_count() != 1 {
        return None;
    }
    let header_path = &shape.header_files[0];
    let override_norm = normalize_diff_path(path_override).unwrap_or_else(|| path_override.into());
    if &override_norm == header_path {
        None
    } else {
        Some(format!(
            "Note: patch headers reference `{header_path}` but `path` overrides to `{override_norm}`."
        ))
    }
}

pub fn format_file_list(files: &[String]) -> String {
    if files.is_empty() {
        return "<none>".to_string();
    }
    let mut shown: Vec<String> = files.iter().take(FILE_LIST_LIMIT).cloned().collect();
    let remaining = files.len().saturating_sub(shown.len());
    if remaining > 0 {
        shown.push(format!("... (+{remaining} more)"));
    }
    shown.join(", ")
}

pub fn push_unique(target: &mut Vec<String>, value: String) {
    if !target.iter().any(|existing| existing == &value) {
        target.push(value);
    }
}

//! Search tools: `grep_files` for code search
//!
//! These tools provide powerful code search capabilities within the workspace,
//! similar to ripgrep/grep functionality.

use super::file::{
    PATH_ALIASES, SEARCH_CONTENT_ALIASES, SEARCH_CONTENT_PARAMS, apply_param_aliases,
};
use super::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_bool, optional_str,
    optional_u64, required_str,
};
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Maximum number of results to return to avoid overwhelming output
const MAX_RESULTS: usize = 100;

/// Maximum file size to search (skip large binaries)
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB

/// Hard cap on a single grep_files run. The directory walk plus per-file regex
/// is synchronous blocking work; without this it can run for minutes on a large
/// tree. Mirrors the file_search tool so both blocking searches behave the same.
const GREP_FILES_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a grep match. Empty context arrays are omitted on the
/// wire (models request context in ~0% of calls); `default` keeps old
/// payloads parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub file: String,
    pub line_number: usize,
    pub line: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_before: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_after: Vec<String>,
}

/// Tool for searching files using regex patterns
pub struct GrepFilesTool;

#[async_trait]
impl ToolSpec for GrepFilesTool {
    fn name(&self) -> &'static str {
        "grep_files"
    }

    fn model_visible(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Search for a regex pattern in workspace files. The pure-Rust search skips common non-code directories by default and returns matching lines with context."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (relative to workspace, default: .)"
                },
                "include": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Glob patterns for files to include (e.g., ['*.rs', '*.ts'])"
                },
                "exclude": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Glob patterns for files to exclude (e.g., ['*.min.js', 'node_modules/*'])"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines before and after each match (default: 2)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Whether to perform case-insensitive matching (default: false)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 100, max: 1000)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File search_content")?;
        apply_param_aliases(&mut input, SEARCH_CONTENT_ALIASES, "File search_content")?;
        SEARCH_CONTENT_PARAMS.reject_unknown(&input)?;

        let pattern_str = required_str(&input, "pattern")?;
        let path_str = optional_str(&input, "path")?.unwrap_or(".");
        let context_lines = usize::try_from(optional_u64(&input, "context_lines", 2)?)
            .unwrap_or(usize::MAX)
            .min(1000);
        let case_insensitive = optional_bool(&input, "case_insensitive", false)?;
        // Bounded like file_search's limit: an explicit value is the
        // model's choice, but a runaway (or typo) must not mint an
        // unbounded JSON array.
        let max_results = usize::try_from(optional_u64(&input, "max_results", MAX_RESULTS as u64)?)
            .unwrap_or(MAX_RESULTS)
            .clamp(1, 1000);

        // Parse include patterns
        let include_patterns: Vec<String> = input
            .get("include")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Parse exclude patterns
        let exclude_patterns: Vec<String> =
            input.get("exclude").and_then(|v| v.as_array()).map_or_else(
                || {
                    // Default exclusions for common non-code directories.
                    // Bare directory names skip the directory traversal entirely;
                    // `dir/*` filters files inside if the directory is already
                    // being walked (belt-and-suspenders — see #2200).
                    vec![
                        "node_modules".to_string(),
                        "node_modules/*".to_string(),
                        ".git".to_string(),
                        ".git/*".to_string(),
                        "target".to_string(),
                        "target/*".to_string(),
                        "*.min.js".to_string(),
                        "*.min.css".to_string(),
                        "dist".to_string(),
                        "dist/*".to_string(),
                        "build".to_string(),
                        "build/*".to_string(),
                        "__pycache__".to_string(),
                        "__pycache__/*".to_string(),
                        ".venv".to_string(),
                        ".venv/*".to_string(),
                        "venv".to_string(),
                        "venv/*".to_string(),
                    ]
                },
                |arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                },
            );

        // Build regex
        let regex_pattern = if case_insensitive {
            format!("(?i){pattern_str}")
        } else {
            pattern_str.to_string()
        };

        // Model-supplied: compile through the bounded cache (length cap,
        // program/DFA size limits, invalid patterns remembered).
        let regex = crate::regex_cache::compile_user_regex(&regex_pattern)
            .map_err(|e| ToolError::invalid_input(format!("Invalid regex pattern: {e}")))?;

        // Resolve search path
        let search_path =
            crate::tools::file::resolve_guarded_read_path(context, path_str, "grep_files")?;

        let workspace = context.workspace.clone();
        let cancel_token = context.cancel_token.clone();
        let follow_symlinks = context.follow_symlinks;

        // The directory walk and per-file regex are synchronous blocking work.
        // Run them on a blocking worker bounded by a hard timeout so a huge tree
        // can't pin the async runtime and leave the stop button unresponsive.
        let result = run_blocking_grep(GREP_FILES_TIMEOUT, cancel_token.clone(), move || {
            let cancel_token = cancel_token.as_ref();
            let workspace_canonical = workspace.canonicalize().ok();

            // Stream the walk: each file is searched as it is discovered and
            // the traversal stops as soon as the match budget is exhausted.
            // Files are never materialized in a big Vec and file contents are
            // read line-by-line, so memory stays bounded by the result set.
            let mut results: Vec<GrepMatch> = Vec::new();
            let mut files_searched = 0;
            let mut total_matches = 0;
            // Set when the cap stopped the search before it covered the whole
            // tree: a match was seen beyond `max_results` in the file that
            // filled the budget, or another candidate file was left unread.
            // The walk never scans past the file that fills the budget.
            let mut truncated = false;
            let mut unreadable_files = 0usize;

            let skipped_dirs = visit_files(
                &search_path,
                &include_patterns,
                &exclude_patterns,
                cancel_token,
                follow_symlinks,
                &mut |file_path| {
                    if truncated || results.len() >= max_results {
                        truncated = true;
                        return Ok(WalkControl::Stop);
                    }
                    check_cancelled(cancel_token)?;

                    // Skip files that are too large
                    if let Ok(metadata) = fs::metadata(file_path)
                        && metadata.len() > MAX_FILE_SIZE
                    {
                        return Ok(WalkControl::Continue);
                    }

                    // Relative path from workspace. The walk root comes
                    // back canonical while the workspace may not be
                    // (macOS /var, Windows \\?\ verbatim paths), so
                    // canonicalize both sides before falling back to
                    // the absolute path.
                    let relative_path = file_path
                        .strip_prefix(&workspace)
                        .ok()
                        .map(|rel| rel.to_string_lossy().to_string())
                        .or_else(|| {
                            workspace_canonical
                                .as_deref()
                                .and_then(|canonical| file_path.strip_prefix(canonical).ok())
                                .map(|rel| rel.to_string_lossy().to_string())
                        })
                        .or_else(|| {
                            // Windows verbatim roots (\\?\C:\ws) canonicalize
                            // the workspace but not the walked file path, so
                            // the strip above can still miss. Canonicalize the
                            // file too before giving up.
                            let ws = workspace_canonical
                                .clone()
                                .or_else(|| workspace.canonicalize().ok());
                            let fp = file_path.canonicalize().ok();
                            ws.zip(fp).and_then(|(ws, fp)| {
                                fp.strip_prefix(&ws)
                                    .ok()
                                    .map(|rel| rel.to_string_lossy().to_string())
                            })
                        })
                        .unwrap_or_else(|| file_path.to_string_lossy().to_string());

                    let budget = max_results.saturating_sub(results.len());
                    let (file_matches, overflowed) = match search_file_streaming(
                        file_path,
                        &relative_path,
                        &regex,
                        context_lines,
                        budget,
                        cancel_token,
                    )? {
                        FileScan::Scanned {
                            matches,
                            overflowed,
                        } => (matches, overflowed),
                        FileScan::NotText => return Ok(WalkControl::Continue),
                        FileScan::Unreadable => {
                            unreadable_files += 1;
                            return Ok(WalkControl::Continue);
                        }
                    };

                    files_searched += 1;
                    total_matches += file_matches.len();
                    results.extend(file_matches);
                    if overflowed {
                        truncated = true;
                        return Ok(WalkControl::Stop);
                    }
                    Ok(WalkControl::Continue)
                },
            )?;

            let matches_json: Vec<Value> = results
                .iter()
                .map(|item| grep_match_to_json(item, context_lines))
                .collect();

            // Echolocation sounding: chart the matched dirs (sorted, capped)
            // so the model sees each match's podmates without a follow-up
            // list. Computed here, inside the blocking worker, with the
            // workspace already at hand.
            let matched_files: Vec<String> = results.iter().map(|item| item.file.clone()).collect();
            let vis = crate::tools::echolocation::PodVisibility {
                include: &include_patterns,
                exclude: &exclude_patterns,
                extensions: &[],
                glob_root: &search_path,
                gitignore: false,
            };
            let (pods, pods_omitted) =
                crate::tools::echolocation::sound_match_pods(&workspace, &matched_files, &vis);
            let pods_json: Vec<Value> = pods
                .iter()
                .map(|pod| {
                    json!({
                        "dir": pod.dir,
                        "mates": pod.mates,
                        "mate_total": pod.mate_total,
                    })
                })
                .collect();

            // Build result. When context_lines == 1, return the single context
            // line as a string instead of a one-item array. That keeps the common
            // "show just the adjacent line" case easy for model callers to read.
            // json! would to_value() these already-built arrays and deep-copy
            // every match and pod. Insert them by move; key order matches the
            // previous object because serde_json preserves insertion order.
            let mut built = serde_json::Map::new();
            built.insert("matches".to_owned(), Value::Array(matches_json));
            built.insert("total_matches".to_owned(), Value::from(total_matches));
            built.insert("files_searched".to_owned(), Value::from(files_searched));
            built.insert("truncated".to_owned(), Value::from(truncated));
            built.insert("pods".to_owned(), Value::Array(pods_json));
            built.insert("pods_omitted".to_owned(), Value::from(pods_omitted));
            let mut output = Value::Object(built);
            if truncated {
                output["max_results"] = json!(max_results);
            }
            // Anything the walk could not read (a directory, a directory
            // entry, or a file) is counted so the caller knows coverage was
            // incomplete rather than reading silence as "no matches there".
            let unreadable = skipped_dirs + unreadable_files;
            if unreadable > 0 {
                output["unreadable_paths_skipped"] = json!(unreadable);
            }
            Ok(output)
        })
        .await?;

        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

/// Run the synchronous grep walk on a blocking worker, cancellable via the
/// token and bounded by `timeout`. Mirrors `run_blocking_file_search`.
async fn run_blocking_grep<F>(
    timeout: Duration,
    cancel_token: Option<CancellationToken>,
    search: F,
) -> Result<Value, ToolError>
where
    F: FnOnce() -> Result<Value, ToolError> + Send + 'static,
{
    if cancel_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(grep_cancelled());
    }

    let task = tokio::task::spawn_blocking(search);
    let result = match cancel_token {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => return Err(grep_cancelled()),
                result = tokio::time::timeout(timeout, task) => result,
            }
        }
        None => tokio::time::timeout(timeout, task).await,
    };

    let joined = result.map_err(|_| grep_timeout(timeout))?;
    joined.map_err(|err| {
        ToolError::execution_failed(format!("grep_files worker failed before completion: {err}"))
    })?
}

fn grep_cancelled() -> ToolError {
    ToolError::cancelled("grep_files cancelled before completion")
}

fn grep_timeout(timeout: Duration) -> ToolError {
    ToolError::Timeout {
        seconds: timeout.as_secs().max(1),
    }
}

fn grep_match_to_json(item: &GrepMatch, context_lines: usize) -> Value {
    if context_lines == 1 {
        json!({
            "file": item.file,
            "line_number": item.line_number,
            "line": item.line,
            "context_before": item.context_before.first().cloned().unwrap_or_default(),
            "context_after": item.context_after.first().cloned().unwrap_or_default(),
        })
    } else {
        json!(item)
    }
}

/// Search a single file line-by-line with a small ring buffer for
/// before-context, so file contents are never fully materialized.
///
/// Returns [`FileScan::NotText`] when the file contains invalid UTF-8
/// anywhere (the whole file must be valid before contributing any match) and
/// [`FileScan::Unreadable`] when it cannot be opened or read. At most `budget` matches are
/// recorded; the scan still runs to EOF so late invalid bytes disqualify the
/// file and pending after-context is completed. `overflowed` is true when
/// the file held at least one match beyond `budget`.
fn search_file_streaming(
    path: &Path,
    relative_path: &str,
    regex: &Regex,
    context_lines: usize,
    budget: usize,
    cancel_token: Option<&CancellationToken>,
) -> Result<FileScan, ToolError> {
    let Ok(file) = fs::File::open(path) else {
        return Ok(FileScan::Unreadable);
    };
    let mut reader = std::io::BufReader::new(file);
    let mut raw: Vec<u8> = Vec::new();
    let mut before: VecDeque<String> = VecDeque::new();
    let mut matches: Vec<GrepMatch> = Vec::new();
    // Matches still waiting for after-context lines: (index into `matches`,
    // lines still needed). Entries complete in FIFO order.
    let mut pending: VecDeque<(usize, usize)> = VecDeque::new();
    let mut line_idx = 0usize;
    // True when a match exists past the collected budget, so the caller
    // can report truncation honestly instead of inferring it.
    let mut overflowed = false;

    loop {
        raw.clear();
        let n = match reader.read_until(b'\n', &mut raw) {
            Ok(n) => n,
            Err(_) => return Ok(FileScan::Unreadable),
        };
        if n == 0 {
            break;
        }
        check_cancelled(cancel_token)?;

        // Mirror `str::lines`: strip the trailing '\n', and a '\r' only when
        // it directly precedes that '\n'.
        let mut end = raw.len();
        if raw[..end].ends_with(b"\n") {
            end -= 1;
            if raw[..end].ends_with(b"\r") {
                end -= 1;
            }
        }
        let Ok(line) = std::str::from_utf8(&raw[..end]) else {
            return Ok(FileScan::NotText);
        };

        for (idx, remaining) in &mut pending {
            matches[*idx].context_after.push(line.to_string());
            *remaining -= 1;
        }
        while pending
            .front()
            .is_some_and(|(_, remaining)| *remaining == 0)
        {
            pending.pop_front();
        }

        // Past the budget we keep testing but stop collecting; the
        // first surplus match proves the cut. With nothing pending (no
        // after-context owed) we can stop scanning immediately.
        let is_match = regex.is_match(line);
        if matches.len() < budget && is_match {
            matches.push(GrepMatch {
                file: relative_path.to_string(),
                line_number: line_idx + 1,
                line: line.to_string(),
                context_before: before.iter().cloned().collect(),
                context_after: Vec::new(),
            });
            if context_lines > 0 {
                pending.push_back((matches.len() - 1, context_lines));
            }
        } else if is_match {
            overflowed = true;
            if pending.is_empty() {
                break;
            }
        }

        if context_lines > 0 {
            if before.len() == context_lines {
                before.pop_front();
            }
            before.push_back(line.to_string());
        }
        line_idx += 1;
    }

    Ok(FileScan::Scanned {
        matches,
        overflowed,
    })
}

/// Outcome of scanning one file.
enum FileScan {
    Scanned {
        matches: Vec<GrepMatch>,
        overflowed: bool,
    },
    /// Binary or not valid UTF-8: skipped on purpose.
    NotText,
    /// Could not be opened or read: skipped, and counted as incomplete coverage.
    Unreadable,
}

/// Flow control for the streaming file walk.
enum WalkControl {
    Continue,
    Stop,
}

/// Walk files matching the include/exclude patterns, invoking `visit` for
/// each one in traversal order. The walk stops early when `visit` returns
/// [`WalkControl::Stop`]. Returns how many subdirectories (or entries) could
/// not be read and were skipped; only an unreadable `root` fails the walk.
fn visit_files(
    root: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    cancel_token: Option<&CancellationToken>,
    follow_symlinks: bool,
    visit: &mut dyn FnMut(&Path) -> Result<WalkControl, ToolError>,
) -> Result<usize, ToolError> {
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let mut skipped = 0usize;
    check_cancelled(cancel_token)?;

    if root.is_file() {
        visit(root)?;
        return Ok(0);
    }

    if follow_symlinks && let Ok(canonical_root) = root.canonicalize() {
        visited_dirs.insert(canonical_root);
    }

    visit_files_recursive(
        root,
        root,
        include_patterns,
        exclude_patterns,
        cancel_token,
        &mut visited_dirs,
        follow_symlinks,
        &mut skipped,
        visit,
    )?;
    Ok(skipped)
}

#[allow(clippy::too_many_arguments)]
fn visit_files_recursive(
    root: &Path,
    current: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    cancel_token: Option<&CancellationToken>,
    visited_dirs: &mut HashSet<PathBuf>,
    follow_symlinks: bool,
    skipped: &mut usize,
    visit: &mut dyn FnMut(&Path) -> Result<WalkControl, ToolError>,
) -> Result<WalkControl, ToolError> {
    check_cancelled(cancel_token)?;

    // An unreadable subdirectory (permissions, or removed mid-walk) is
    // skipped and counted rather than discarding every match found so far.
    // The search root itself still fails loudly.
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(e) if current != root => {
            tracing::debug!(dir = %current.display(), error = %e, "grep_files skipped an unreadable directory");
            *skipped += 1;
            return Ok(WalkControl::Continue);
        }
        Err(e) => {
            return Err(ToolError::execution_failed(format!(
                "Failed to read directory {}: {}",
                current.display(),
                e
            )));
        }
    };

    for entry in entries {
        check_cancelled(cancel_token)?;

        let Ok(entry) = entry else {
            *skipped += 1;
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            *skipped += 1;
            continue;
        };
        if file_type.is_symlink() && !follow_symlinks {
            continue;
        }

        // Get relative path for pattern matching. Globs use `/`, so a native
        // `\` separator is normalized as file_search does; otherwise
        // `src/**` could never match `src\a.rs` on Windows.
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative_str = relative.to_string_lossy();
        let relative_str = if std::path::MAIN_SEPARATOR == '/' {
            relative_str
        } else {
            std::borrow::Cow::Owned(relative_str.replace(std::path::MAIN_SEPARATOR, "/"))
        };

        // Check exclusions
        if should_exclude(&relative_str, exclude_patterns) {
            continue;
        }

        // When following symlinks, resolve the target type for directories
        // and files so symlinked dirs are traversed and symlinked files are
        // included.
        let effective_type = if file_type.is_symlink() && follow_symlinks {
            match fs::metadata(&path) {
                Ok(meta) => meta.file_type(),
                Err(_) => continue,
            }
        } else {
            file_type
        };

        if effective_type.is_dir() {
            if follow_symlinks {
                let canonical_dir = match path.canonicalize() {
                    Ok(canonical) => canonical,
                    Err(_) => continue,
                };
                if !visited_dirs.insert(canonical_dir) {
                    continue;
                }
            }
            if let WalkControl::Stop = visit_files_recursive(
                root,
                &path,
                include_patterns,
                exclude_patterns,
                cancel_token,
                visited_dirs,
                follow_symlinks,
                skipped,
                visit,
            )? {
                return Ok(WalkControl::Stop);
            }
        } else if effective_type.is_file() {
            // Sandbox read deny-list (S1). A recursive search is not a directed
            // read request, so a denied file is skipped rather than failing the
            // whole search — otherwise one `.env` anywhere in the tree would
            // make `search` useless. The skip is logged; a directed
            // `read_file`/`read`/`read_media` on the same path still returns an
            // explicit refusal rather than an empty result.
            if let Err(denial) = crate::sandbox::read_guard::active().check(&path) {
                tracing::debug!(
                    target: "codewhale::sandbox::read_guard",
                    requested = %denial.requested.display(),
                    "sandbox read deny-list skipped a file during search"
                );
                continue;
            }
            // Check inclusions (if any specified)
            if (include_patterns.is_empty() || should_include(&relative_str, include_patterns))
                && let WalkControl::Stop = visit(&path)?
            {
                return Ok(WalkControl::Stop);
            }
        }
    }

    Ok(WalkControl::Continue)
}

fn check_cancelled(cancel_token: Option<&CancellationToken>) -> Result<(), ToolError> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(ToolError::cancelled("search cancelled before completion"));
    }
    Ok(())
}

/// Check if a path matches any of the exclude patterns
fn should_exclude(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if matches_glob(path, pattern) {
            return true;
        }
    }
    false
}

/// Check if a path matches any of the include patterns
fn should_include(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if matches_glob(path, pattern) {
            return true;
        }
    }
    false
}

/// Simple glob pattern matching
/// Supports: * (any chars), ** (any path), ? (single char)
pub(crate) fn matches_glob(path: &str, pattern: &str) -> bool {
    // Handle ** for any path
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0].trim_end_matches('/');
            let suffix = parts[1].trim_start_matches('/');

            // A prefix ending in `/` names a directory: `build/**` covers
            // `build` and `build/...`, never `build.rs` or `builders/`. A
            // prefix without one (`src/test_**`) is a plain string prefix.
            let under_prefix = if parts[0].ends_with('/') {
                path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            } else {
                path.starts_with(prefix)
            };
            if !prefix.is_empty() && !under_prefix {
                return false;
            }
            if !suffix.is_empty() {
                return path.ends_with(suffix)
                    || path
                        .split('/')
                        .any(|part| matches_simple_glob(part, suffix));
            }
            return true;
        }
    }

    // Handle patterns like "*.rs" - match against filename only
    if pattern.starts_with('*') && !pattern.contains('/') {
        let filename = path.rsplit('/').next().unwrap_or(path);
        return matches_simple_glob(filename, pattern);
    }

    // Handle patterns with path components
    if pattern.contains('/') {
        return matches_simple_glob(path, pattern);
    }

    // Match against filename
    let filename = path.rsplit('/').next().unwrap_or(path);
    matches_simple_glob(filename, pattern)
}

/// Simple glob matching for single path component
fn matches_simple_glob(text: &str, pattern: &str) -> bool {
    let mut text_chars = text.chars().peekable();
    let mut pattern_chars = pattern.chars().peekable();

    while let Some(p) = pattern_chars.next() {
        match p {
            '*' => {
                // Match zero or more characters
                let next_pattern: String = pattern_chars.collect();
                if next_pattern.is_empty() {
                    return true;
                }

                // Try matching at each position (use char-indices to stay on
                // UTF-8 boundaries — byte-index slicing panics on multi-byte
                // characters like 冰糖, see #249).
                let remaining: String = text_chars.collect();
                for (i, _) in remaining.char_indices() {
                    if matches_simple_glob(&remaining[i..], &next_pattern) {
                        return true;
                    }
                }
                // Also try the empty suffix at end of string
                if matches_simple_glob("", &next_pattern) {
                    return true;
                }
                return false;
            }
            '?' => {
                // Match exactly one character
                if text_chars.next().is_none() {
                    return false;
                }
            }
            c => {
                // Match literal character
                if text_chars.next() != Some(c) {
                    return false;
                }
            }
        }
    }

    text_chars.next().is_none()
}

// === Unit Tests ===

#[cfg(test)]
mod tests;

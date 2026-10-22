//! File search tool with fuzzy matching and scoring.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use globset::{GlobBuilder, GlobMatcher, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::file::{PATH_ALIASES, SEARCH_NAME_ALIASES, SEARCH_NAME_PARAMS, apply_param_aliases};
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, optional_u64, required_str,
};

const FILE_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
struct FileSearchMatch {
    path: String,
    name: String,
    score: f64,
}

pub struct FileSearchTool;

#[async_trait]
impl ToolSpec for FileSearchTool {
    fn name(&self) -> &'static str {
        "file_search"
    }

    fn model_visible(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Find workspace files by name, path, or glob (e.g. src/**/*.rs). Space-separated terms must all match. Literal matches rank above fuzzy matches; results are capped. Respects ignore files and skips build/dependency directories by default. Use extensions to filter suffixes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "File name, path fragment, space-separated terms, or glob (e.g. **/engine*.rs)."
                },
                "path": {
                    "type": "string",
                    "description": "Optional base path to search (relative to workspace)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of ranked results to return (default: 20, maximum: 200)."
                },
                "extensions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of file extensions to include (e.g. [\"rs\", \"md\"])."
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional replacement exclusion globs (e.g. [\"**/target/**\", \"*.lock\"]); [] disables default exclusions."
                }
            },
            "required": ["query"]
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
        let mut input = input;
        apply_param_aliases(&mut input, PATH_ALIASES, "File search_name")?;
        apply_param_aliases(&mut input, SEARCH_NAME_ALIASES, "File search_name")?;
        SEARCH_NAME_PARAMS.reject_unknown(&input)?;

        let query = required_str(&input, "query")?.trim();
        if query.is_empty() {
            return Err(ToolError::invalid_input("query cannot be empty"));
        }

        let limit = optional_u64(&input, "limit", 20)?.clamp(1, 200) as usize;
        // S1: enumerating names inside a denied tree is a read of that tree —
        // Seatbelt's `deny file-read*` blocks readdir of denied dirs, so a
        // denied search root is refused rather than walked. The raw spelling is
        // checked before `resolve_path` (F2) so the refusal names the caller's
        // path, never a symlink target it might resolve to.
        let base_path = match optional_str(&input, "path")? {
            Some(path) if !path.trim().is_empty() => {
                super::file::enforce_read_denylist(Path::new(path), "file_search")?;
                context.resolve_path(path)?
            }
            _ => context.workspace.clone(),
        };
        super::file::enforce_read_denylist(&base_path, "file_search")?;

        let extensions = parse_extensions(&input);
        let exclude_patterns = parse_exclude_patterns(&input);
        let result = search_files_async(
            query.to_string(),
            base_path,
            extensions,
            exclude_patterns,
            limit,
            context.cancel_token.clone(),
            FILE_SEARCH_TIMEOUT,
            context.follow_symlinks,
        )
        .await?;
        ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[allow(clippy::too_many_arguments)]
async fn search_files_async(
    query: String,
    base_path: PathBuf,
    extensions: Vec<String>,
    exclude_patterns: Vec<String>,
    limit: usize,
    cancel_token: Option<CancellationToken>,
    timeout: Duration,
    follow_symlinks: bool,
) -> Result<Value, ToolError> {
    run_blocking_file_search(timeout, cancel_token, move |worker_cancel_token| {
        let matches = search_files(
            &query,
            &base_path,
            extensions.clone(),
            exclude_patterns.clone(),
            limit,
            Some(&worker_cancel_token),
            follow_symlinks,
        )?;
        check_cancelled(Some(&worker_cancel_token))?;
        let paths: Vec<String> = matches.iter().map(|item| item.path.clone()).collect();
        let vis = super::echolocation::PodVisibility {
            include: &[],
            exclude: &exclude_patterns,
            extensions: &extensions,
            glob_root: &base_path,
            gitignore: true,
        };
        let (pods, pods_omitted) = super::echolocation::sound_match_pods(&base_path, &paths, &vis);
        let pods_json: Vec<Value> = pods
            .iter()
            .map(|pod| {
                json!({
                    "dir": pod.dir, "mates": pod.mates, "mate_total": pod.mate_total,
                })
            })
            .collect();
        check_cancelled(Some(&worker_cancel_token))?;
        Ok(json!({ "matches": matches, "pods": pods_json, "pods_omitted": pods_omitted }))
    })
    .await
}

async fn run_blocking_file_search<F, T>(
    timeout: Duration,
    cancel_token: Option<CancellationToken>,
    search: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    if cancel_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(file_search_cancelled());
    }

    let worker_cancel_token = cancel_token
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let _worker_cancel_guard = worker_cancel_token.clone().drop_guard();
    let task = tokio::task::spawn_blocking(move || search(worker_cancel_token));
    let result = match cancel_token {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => return Err(file_search_cancelled()),
                result = tokio::time::timeout(timeout, task) => result,
            }
        }
        None => tokio::time::timeout(timeout, task).await,
    };

    let joined = result.map_err(|_| file_search_timeout(timeout))?;
    joined.map_err(|err| {
        ToolError::execution_failed(format!(
            "file_search worker failed before completion: {err}"
        ))
    })?
}

fn file_search_cancelled() -> ToolError {
    ToolError::cancelled("file_search cancelled before completion")
}

fn file_search_timeout(timeout: Duration) -> ToolError {
    ToolError::Timeout {
        seconds: timeout.as_secs().max(1),
    }
}

fn parse_extensions(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(values) = input.get("extensions").and_then(|v| v.as_array()) {
        for value in values {
            if let Some(ext) = value.as_str() {
                let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
                if !ext.is_empty() {
                    out.push(ext);
                }
            }
        }
    }
    if out.is_empty()
        && let Some(value) = input.get("extension").and_then(|v| v.as_str())
    {
        let ext = value.trim().trim_start_matches('.').to_ascii_lowercase();
        if !ext.is_empty() {
            out.push(ext);
        }
    }
    out
}

fn parse_exclude_patterns(input: &Value) -> Vec<String> {
    if let Some(values) = input.get("exclude").and_then(Value::as_array) {
        return values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }

    [
        "**/target/**",
        "**/node_modules/**",
        "**/.git/**",
        "**/DerivedData/**",
        "**/dist/**",
        "**/build/**",
        "*.lock",
        "*.plist",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn search_files(
    query: &str,
    base_path: &Path,
    extensions: Vec<String>,
    exclude_patterns: Vec<String>,
    limit: usize,
    cancel_token: Option<&CancellationToken>,
    follow_symlinks: bool,
) -> Result<Vec<FileSearchMatch>, ToolError> {
    check_cancelled(cancel_token)?;

    if !base_path.exists() {
        return Err(ToolError::invalid_input(format!(
            "Base path does not exist: {}",
            base_path.display()
        )));
    }

    let query_norm = query.to_lowercase().replace('\\', "/");
    let query_norm = query_norm.strip_prefix("./").unwrap_or(&query_norm);
    if query_norm.is_empty() {
        return Err(ToolError::invalid_input("query cannot be empty"));
    }
    let query_glob = if query_norm.contains(['*', '?', '[', '{']) {
        let pattern = if query_norm.contains('/') {
            query_norm.to_string()
        } else {
            format!("**/{query_norm}")
        };
        match GlobBuilder::new(&pattern)
            .literal_separator(true)
            .case_insensitive(true)
            .build()
        {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(err) if query_norm.contains(['*', '?']) => {
                return Err(ToolError::invalid_input(format!(
                    "Invalid query glob: {err}"
                )));
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let query_terms: Vec<_> = query_norm.split_whitespace().collect();
    let mut exclusions = GlobSetBuilder::new();
    for pattern in exclude_patterns {
        let pattern = pattern.replace('\\', "/");
        let pattern = if pattern.contains('/') {
            pattern
        } else {
            format!("**/{pattern}")
        };
        for glob in std::iter::once(pattern.as_str()).chain(pattern.strip_suffix("/**")) {
            exclusions.add(
                GlobBuilder::new(glob)
                    .literal_separator(true)
                    .build()
                    .map_err(|err| {
                        ToolError::invalid_input(format!("Invalid exclude glob: {err}"))
                    })?,
            );
        }
    }
    let exclusions = exclusions
        .build()
        .map_err(|err| ToolError::invalid_input(format!("Invalid exclude globs: {err}")))?;
    let mut results: Vec<FileSearchMatch> = Vec::with_capacity(limit);
    let walk_root = base_path.to_path_buf();
    let walk_cancel_token = cancel_token.cloned();

    let mut builder = WalkBuilder::new(base_path);
    builder
        .hidden(false)
        .follow_links(follow_symlinks)
        .require_git(false)
        .filter_entry(move |entry| {
            let rel_path = entry
                .path()
                .strip_prefix(&walk_root)
                .unwrap_or(entry.path());
            if entry.depth() > 0
                && !walk_cancel_token
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                && exclusions.is_match(rel_path)
            {
                return false;
            }
            // Sandbox read deny-list (S1). A walk rooted above a denied tree
            // must not enumerate its names. Reject directories before descent;
            // the explicit root refusal remains at the guard above.
            if let Err(denial) = crate::sandbox::read_guard::active().check(entry.path()) {
                tracing::debug!(
                    target: "codewhale::sandbox::read_guard",
                    requested = %denial.requested.display(),
                    "sandbox read deny-list skipped an entry during file_search"
                );
                return false;
            }
            true
        });
    let walker = builder.build();

    for entry in walker {
        check_cancelled(cancel_token)?;

        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();
        let rel_path = path
            .strip_prefix(base_path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if !extensions.is_empty() && !extension_matches(path, &extensions) {
            continue;
        }

        let name = file_name(path);

        let score = match score_match(
            query_norm,
            &query_terms,
            query_glob.as_ref(),
            &rel_path,
            &name,
        ) {
            Some(score) => score,
            None => continue,
        };

        let found = FileSearchMatch {
            path: rel_path,
            name,
            score,
        };
        if results.len() == limit
            && results
                .last()
                .is_some_and(|worst| compare_match(&found, worst) != Ordering::Less)
        {
            continue;
        }
        let index = results
            .binary_search_by(|existing| compare_match(existing, &found))
            .unwrap_or_else(|index| index);
        if index < limit {
            results.insert(index, found);
            results.truncate(limit);
        }
    }

    check_cancelled(cancel_token)?;
    Ok(results)
}

fn check_cancelled(cancel_token: Option<&CancellationToken>) -> Result<(), ToolError> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(file_search_cancelled());
    }
    Ok(())
}

pub(crate) fn extension_matches(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    extensions.iter().any(|wanted| wanted == &ext)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn score_match(
    query: &str,
    terms: &[&str],
    glob: Option<&GlobMatcher>,
    rel_path: &str,
    name: &str,
) -> Option<f64> {
    let path_norm = rel_path.to_lowercase();
    let name_norm = name.to_lowercase();
    if path_norm == query {
        return Some(1.0);
    }
    if name_norm == query {
        return Some(0.99);
    }
    if let Some(glob) = glob {
        if name_norm.contains(query) || path_norm.contains(query) {
            return score_fragment(query, &path_norm, &name_norm);
        }
        return glob
            .is_match(&path_norm)
            .then(|| 0.9 + 0.08 / name.chars().count().max(1) as f64);
    }

    if terms.len() > 1 {
        let mut total = 0.0;
        let mut minimum = 1.0_f64;
        for term in terms {
            let score = score_fragment(term, &path_norm, &name_norm)?;
            minimum = minimum.min(score);
            total += score;
        }
        return Some(minimum * 0.995 + 0.005 * total / terms.len() as f64);
    }
    score_fragment(query, &path_norm, &name_norm)
}

fn score_fragment(query: &str, path_norm: &str, name_norm: &str) -> Option<f64> {
    if path_norm == query {
        return Some(1.0);
    }
    if name_norm == query {
        return Some(0.99);
    }

    if name_norm.starts_with(query) {
        return Some(0.9 + length_bonus(query, name_norm));
    }
    if path_norm.starts_with(query) {
        return Some(0.8 + length_bonus(query, path_norm));
    }

    if name_norm.contains(query) {
        return Some(0.7 + length_bonus(query, name_norm));
    }
    if path_norm.contains(query) {
        return Some(0.6 + length_bonus(query, path_norm));
    }

    if let Some(score) = fuzzy_score(query, name_norm) {
        return Some(0.4 + 0.15 * score);
    }
    if let Some(score) = fuzzy_score(query, path_norm) {
        return Some(0.2 + 0.15 * score);
    }

    None
}

fn length_bonus(query: &str, target: &str) -> f64 {
    let q_len = query.chars().count().max(1) as f64;
    let t_len = target.chars().count().max(1) as f64;
    (q_len / t_len).min(1.0) * 0.08
}

fn fuzzy_score(query: &str, target: &str) -> Option<f64> {
    let mut query_chars = query.chars();
    let mut current = query_chars.next()?;
    let mut first = None;
    let mut last = 0;
    let mut matched = 0;

    for (idx, ch) in target.chars().enumerate() {
        if ch == current {
            first.get_or_insert(idx);
            last = idx;
            matched += 1;
            if let Some(next) = query_chars.next() {
                current = next;
            } else {
                break;
            }
        }
    }

    if matched != query.chars().count() {
        return None;
    }

    let first = first? as f64;
    let last = last as f64;
    let span = (last - first + 1.0).max(1.0);
    let query_len = query.chars().count().max(1) as f64;
    let target_len = target.chars().count().max(1) as f64;

    let density = (query_len / span).min(1.0);
    let coverage = (query_len / target_len).min(1.0);
    Some((density * 0.7 + coverage * 0.3).min(1.0))
}

fn compare_match(a: &FileSearchMatch, b: &FileSearchMatch) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.path.cmp(&b.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_search_basic() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n").expect("write");
        std::fs::write(root.join("README.md"), "docs\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "main", "limit": 5}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(result.content.contains("main.rs"));
    }

    #[tokio::test]
    async fn test_file_search_charts_match_pods() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/net")).expect("mkdir");
        std::fs::write(root.join("src/net/server.rs"), "fn serve() {}\n").expect("write");
        std::fs::write(root.join("src/net/client.rs"), "fn connect() {}\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let result = FileSearchTool
            .execute(json!({"query": "server", "limit": 5}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).expect("json result");
        assert!(!body["matches"].as_array().expect("matches").is_empty());
        let pods = body["pods"].as_array().expect("pods array");
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0]["dir"], "src/net");
        let mates: Vec<&str> = pods[0]["mates"]
            .as_array()
            .expect("mates")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(mates.contains(&"server.rs (match)"), "{mates:?}");
        assert!(mates.contains(&"client.rs"), "{mates:?}");
    }

    #[tokio::test]
    async fn test_file_search_respects_gitignore() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").expect("write");
        std::fs::write(root.join("ignored.txt"), "nope\n").expect("write");
        std::fs::write(root.join("keep.txt"), "ok\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "txt"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(!result.content.contains("ignored.txt"));
        assert!(result.content.contains("keep.txt"));
    }

    #[tokio::test]
    async fn test_file_search_extension_filter() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").expect("write");
        std::fs::write(root.join("notes.md"), "docs\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "m", "extensions": ["rs"]}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(result.content.contains("main.rs"));
        assert!(!result.content.contains("notes.md"));
    }

    #[tokio::test]
    async fn test_file_search_exclude_filter() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("fixtures")).expect("mkdir");
        std::fs::write(root.join("fixtures").join("needle.txt"), "no\n").expect("write");
        std::fs::write(root.join("needle.txt"), "yes\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "needle", "exclude": ["fixtures/**"]}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).expect("search json");
        assert!(
            body["matches"]
                .as_array()
                .expect("matches")
                .iter()
                .any(|item| item.get("path").and_then(Value::as_str) == Some("needle.txt"))
        );
        assert!(!result.content.contains("fixtures/needle.txt"));
    }

    #[tokio::test]
    async fn test_file_search_default_excludes_build_artifacts() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("target")).expect("mkdir");
        std::fs::write(root.join("target").join("needle.txt"), "no\n").expect("write");
        std::fs::create_dir_all(root.join("member").join("target")).expect("mkdir nested");
        std::fs::write(
            root.join("member").join("target").join("nested_needle.txt"),
            "no\n",
        )
        .expect("write nested");
        std::fs::write(root.join("needle.txt"), "yes\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "needle"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).expect("search json");
        assert!(
            body["matches"]
                .as_array()
                .expect("matches")
                .iter()
                .any(|item| item.get("path").and_then(Value::as_str) == Some("needle.txt"))
        );
        assert!(!result.content.contains("target/needle.txt"));
        assert!(!result.content.contains("nested_needle.txt"));
    }

    #[tokio::test]
    async fn test_file_search_default_excludes_stop_at_the_directory() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("build")).expect("mkdir");
        std::fs::create_dir_all(root.join("builders")).expect("mkdir");
        std::fs::write(root.join("build").join("build_out.rs"), "no\n").expect("write");
        std::fs::write(root.join("build.rs"), "yes\n").expect("write");
        std::fs::write(root.join("builders").join("build_x.rs"), "yes\n").expect("write");

        let ctx = ToolContext::new(root.to_path_buf());
        let result = FileSearchTool
            .execute(json!({"query": "build", "limit": 20}), &ctx)
            .await
            .expect("execute");

        let matches: Value = serde_json::from_str(&result.content).expect("search json");
        let paths: Vec<&str> = matches["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .filter_map(|item| item.get("path").and_then(Value::as_str))
            .collect();
        // `build/**` hides the build directory, not same-prefix siblings.
        assert!(paths.contains(&"build.rs"), "{paths:?}");
        assert!(paths.contains(&"builders/build_x.rs"), "{paths:?}");
        assert!(!paths.contains(&"build/build_out.rs"), "{paths:?}");
    }

    #[tokio::test]
    async fn test_file_search_respects_cancel_token() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("needle.txt"), "yes\n").expect("write");
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();
        let ctx = ToolContext::new(root.to_path_buf()).with_cancel_token(cancel_token);

        let tool = FileSearchTool;
        let err = tool
            .execute(json!({"query": "needle"}), &ctx)
            .await
            .expect_err("cancelled file_search should return an error");

        assert!(
            format!("{err:?}").contains("cancelled"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_file_search_blocking_wrapper_reports_timeout() {
        let (finished, worker_finished) = tokio::sync::oneshot::channel();
        let err = run_blocking_file_search::<_, Vec<FileSearchMatch>>(
            Duration::from_millis(1),
            None,
            move |cancel_token| {
                while !cancel_token.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = finished.send(cancel_token.is_cancelled());
                Err(file_search_cancelled())
            },
        )
        .await
        .expect_err("slow file_search worker should time out");

        assert!(
            matches!(err, ToolError::Timeout { seconds: 1 }),
            "unexpected error: {err:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), worker_finished)
                .await
                .expect("timed-out worker must stop")
                .expect("worker completion"),
            "timeout must cancel the worker without a parent cancellation token"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_file_search_does_not_follow_symlinked_files() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).expect("mkdir workspace");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, "outside\n").expect("write outside");
        std::os::unix::fs::symlink(&outside_file, root.join("secret.txt")).expect("symlink");

        let ctx = ToolContext::new(root);
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "secret"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(!result.content.contains("secret.txt"));
    }

    /// F1: searching a denied directory is enumeration of it — `file_search`
    /// with its root inside a denied tree must refuse, matching the OS layer
    /// (Seatbelt `deny file-read*` blocks readdir of denied dirs).
    #[tokio::test]
    async fn test_file_search_refuses_a_denied_root() {
        let holder = tempdir().expect("tempdir");
        let project = holder.path().join("project");
        // Deterministic anchor: the `.env` filename rule denies any path whose
        // file name is `.env`, directory or not.
        std::fs::create_dir_all(project.join(".env")).expect("mkdir");
        std::fs::write(project.join(".env").join("token"), "x\n").expect("write");

        let ctx = ToolContext::new(project.clone());
        let tool = FileSearchTool;
        let error = tool
            .execute(
                json!({"query": "token", "path": project.join(".env")}),
                &ctx,
            )
            .await
            .expect_err("a denied search root must be refused, not walked");

        assert!(
            matches!(error, ToolError::PermissionDenied { .. }),
            "expected a permission refusal, got: {error:?}"
        );
    }

    /// F1: a walk rooted *above* a denied path must not enumerate names inside
    /// it — `path = "~"` must not surface `~/.ssh/id_rsa`, and a project root
    /// must not surface its `.env` (denied by name under the default list).
    #[tokio::test]
    async fn test_file_search_skips_denied_entries_during_the_walk() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("needle.txt"), "yes\n").expect("write");
        std::fs::write(root.join(".env"), "SECRET=1\n").expect("write env");
        std::fs::write(root.join(".env.local"), "SECRET=2\n").expect("write env local");
        std::fs::create_dir_all(root.join("denied").join(".env")).expect("mkdir denied");
        std::fs::write(
            root.join("denied").join(".env").join("needle-secret.txt"),
            "SECRET=3\n",
        )
        .expect("write denied child");

        let ctx = ToolContext::new(root.to_path_buf());
        let tool = FileSearchTool;
        let result = tool
            .execute(json!({"query": "env"}), &ctx)
            .await
            .expect("execute");

        assert!(result.success);
        assert!(
            !result.content.contains(".env"),
            "denied entries must not be enumerated by a name search: {}",
            result.content
        );

        // The innocent sibling is still found — the deny-list filters, it does
        // not blank the search.
        let result = tool
            .execute(json!({"query": "needle"}), &ctx)
            .await
            .expect("execute");
        assert!(result.success);
        assert!(result.content.contains("needle.txt"));
        assert!(!result.content.contains("needle-secret.txt"));
    }
}

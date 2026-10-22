//! Repo-aware working set tracking and prompt context packing.
//!
//! The goal of this module is to keep a small, high-signal list of
//! "active" paths that the assistant should prioritize. It observes
//! user messages and tool calls, extracts likely paths, and produces:
//! - a compact working-set summary block for the system prompt
//! - pinned message indices that compaction should preserve

use crate::tools::echolocation::{
    CodeReadReference, MAX_DELIVERED_REFS, delivered_navigation_refs,
};
use crate::workspace_discovery::{
    DISCOVERY_ALWAYS_DIRS, path_is_excluded_from_discovery, should_skip_unignored_discovery_entry,
};
use codewhale_models::{ContentBlock, Message};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Repo-aware resolver for `@`-mentions and file pickers.
///
/// `cwd` is captured at construction; if the host's current directory changes
/// during a session, build a fresh `Workspace`. Composer discovery callers run
/// its bounded walkers on a background worker; [`Workspace::resolve_exact`]
/// deliberately performs no fuzzy tree traversal.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    cwd: Option<PathBuf>,
    #[cfg(test)]
    file_index: OnceLock<HashMap<String, Vec<PathBuf>>>,
    completion_walk_depth: Option<usize>,
    /// Follow symbolic links during file discovery walks. When `true`,
    /// symlinked directories are traversed, enabling multi-project workspaces
    /// where project directories are symlinked into a hub directory.
    follow_links: bool,
}

struct SearchContext<'a> {
    needle: &'a str,
    limit: usize,
    prefix_hits: &'a mut Vec<String>,
    substring_hits: &'a mut Vec<String>,
    seen: &'a mut HashSet<PathBuf>,
    cancelled: &'a dyn Fn() -> bool,
}

impl SearchContext<'_> {
    fn is_full(&self) -> bool {
        self.prefix_hits.len() + self.substring_hits.len() >= self.limit
    }

    fn should_stop(&self) -> bool {
        self.is_full() || (self.cancelled)()
    }

    fn remember(&mut self, path: PathBuf) -> bool {
        self.seen.insert(path)
    }

    fn push_match(&mut self, candidate: String) {
        let lower = candidate.to_lowercase();
        if self.needle.is_empty() || lower.starts_with(self.needle) {
            self.prefix_hits.push(candidate);
        } else if lower.contains(self.needle) {
            self.substring_hits.push(candidate);
        }
    }
}

impl Workspace {
    /// Construct a workspace anchored at `root`, capturing the process CWD as
    /// the secondary resolution pass. Convenience entry point intended for
    /// callers that don't already have a CWD on hand; the App routes through
    /// [`Workspace::with_cwd`] with its own captured launch directory.
    #[allow(dead_code)] // Keeps the surface stable for #97 (Ctrl+P picker).
    pub fn new(root: PathBuf) -> Self {
        Self::with_cwd(root, std::env::current_dir().ok())
    }

    /// Construct with an explicit cwd. Used by tests that need deterministic
    /// resolution against a known directory without depending on (and
    /// mutating) the process's real working directory.
    pub fn with_cwd(root: PathBuf, cwd: Option<PathBuf>) -> Self {
        Self::with_cwd_and_depth(root, cwd, DEFAULT_COMPLETIONS_WALK_DEPTH)
    }

    /// Construct with an explicit completion walk depth. A depth of `0`
    /// disables the depth limit for users with deeply nested workspaces.
    pub fn with_cwd_and_depth(root: PathBuf, cwd: Option<PathBuf>, walk_depth: usize) -> Self {
        Self::with_cwd_depth_and_follow_links(root, cwd, walk_depth, false)
    }

    /// Construct with an explicit completion walk depth and symlink-following
    /// preference. See [`Workspace::follow_links`].
    pub fn with_cwd_depth_and_follow_links(
        root: PathBuf,
        cwd: Option<PathBuf>,
        walk_depth: usize,
        follow_links: bool,
    ) -> Self {
        Self {
            root,
            cwd,
            #[cfg(test)]
            file_index: OnceLock::new(),
            completion_walk_depth: normalize_completion_walk_depth(walk_depth),
            follow_links,
        }
    }

    /// Two-pass resolution: workspace, then cwd, then fuzzy fallback.
    #[cfg(test)]
    pub fn resolve(&self, raw_path: &str) -> Result<PathBuf, PathBuf> {
        let literal = self.resolve_exact(raw_path);
        if literal.is_ok() {
            return literal;
        }
        let path = expand_mention_home(raw_path);
        if let Some(fuzzy) = self.fuzzy_resolve(&path) {
            return Ok(fuzzy);
        }
        literal
    }

    /// Resolve only the path the user actually typed: workspace, then cwd.
    ///
    /// Composer send-time resolution uses this path because fuzzy fallback
    /// builds a full basename index. Fuzzy tree discovery is instead confined
    /// to the bounded background completion worker (#4365).
    pub fn resolve_exact(&self, raw_path: &str) -> Result<PathBuf, PathBuf> {
        let path = expand_mention_home(raw_path);
        if path.is_absolute() {
            if path.exists() {
                return Ok(path);
            }
            return Err(path);
        }

        let ws_path = self.root.join(&path);
        if ws_path.exists() {
            return Ok(ws_path);
        }

        if let Some(cwd) = self.cwd.as_ref() {
            let cwd_path = cwd.join(&path);
            if cwd_path.exists() {
                return Ok(cwd_path);
            }
        }
        Err(ws_path)
    }

    #[cfg(test)]
    fn fuzzy_resolve(&self, path: &Path) -> Option<PathBuf> {
        let needle = path.file_name()?.to_string_lossy().to_lowercase();
        if needle.is_empty() {
            return None;
        }

        let index = self.file_index.get_or_init(|| self.build_file_index());
        index.get(&needle).and_then(|paths| paths.first()).cloned()
    }

    #[cfg(test)]
    fn build_file_index(&self) -> HashMap<String, Vec<PathBuf>> {
        let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut total: usize = 0;
        let builder =
            discovery_walk_builder(&self.root, self.completion_walk_depth, self.follow_links);

        for entry in builder.build().flatten() {
            if total >= FILE_INDEX_MAX_ENTRIES {
                tracing::warn!(
                    target: "working_set",
                    limit = FILE_INDEX_MAX_ENTRIES,
                    "file-index discovery hit the entry cap; truncating to keep first-turn latency bounded (#697)"
                );
                return index;
            }
            if entry
                .file_type()
                .is_some_and(|ft| ft.is_file() || ft.is_dir())
            {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                index
                    .entry(name)
                    .or_default()
                    .push(entry.path().to_path_buf());
                total += 1;
            }
        }

        // Also index AI-tool dot-directories with gitignore disabled.
        for dir_name in DISCOVERY_ALWAYS_DIRS {
            if total >= FILE_INDEX_MAX_ENTRIES {
                break;
            }
            let dot_dir = self.root.join(dir_name);
            if !dot_dir.is_dir() {
                continue;
            }
            let mut dot_builder = WalkBuilder::new(&dot_dir);
            dot_builder
                .hidden(true)
                .follow_links(self.follow_links)
                .git_ignore(false)
                .ignore(false);
            if let Some(depth) = child_completion_walk_depth(self.completion_walk_depth) {
                dot_builder.max_depth(Some(depth));
            }
            for entry in dot_builder.build().flatten() {
                if total >= FILE_INDEX_MAX_ENTRIES {
                    break;
                }
                // Exclude machine-generated bulk (e.g. .deepseek/snapshots/).
                if path_is_excluded_from_discovery(&self.root, entry.path()) {
                    continue;
                }
                if entry
                    .file_type()
                    .is_some_and(|ft| ft.is_file() || ft.is_dir())
                {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    index
                        .entry(name)
                        .or_default()
                        .push(entry.path().to_path_buf());
                    total += 1;
                }
            }
        }

        // Beyond the curated dot-dir whitelist above, also index any explicit
        // hidden/ignored path the user might `@`-mention (e.g. a project's
        // own `.generated/specs/`). `local_reference_paths` walks with
        // gitignore disabled but still honors `.deepseekignore`.
        for path in local_reference_paths(
            &self.root,
            LOCAL_REFERENCE_SCAN_LIMIT,
            self.completion_walk_depth,
            self.follow_links,
        ) {
            if total >= FILE_INDEX_MAX_ENTRIES {
                break;
            }
            let Some(name) = path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
            else {
                continue;
            };
            index.entry(name).or_default().push(path);
            total += 1;
        }
        index
    }

    /// Walk the workspace (and the recorded `cwd` when it diverges) and
    /// return relative paths whose representation matches `partial`.
    ///
    /// Ranking: a candidate matches when its case-insensitive display string
    /// starts with `partial` (prefix hit) or contains it as a substring; prefix
    /// hits sort first so `docs/de` lands `docs/deepseek_v4.pdf` ahead of any
    /// path that merely shares those bytes.
    ///
    /// Display strings are workspace-relative for files under `root`, and
    /// cwd-relative for files only under the recorded `cwd` — so what the user
    /// Tab-completes matches what their shell would have shown them.
    ///
    /// Honors `.gitignore`, `.git/info/exclude`, `.ignore`, and
    /// `.deepseekignore`. Capped at `limit` results.
    #[must_use]
    #[cfg(test)]
    pub fn completions(&self, partial: &str, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        let needle = partial.to_lowercase();
        let mut prefix_hits: Vec<String> = Vec::new();
        let mut substring_hits: Vec<String> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let never_cancelled = || false;

        // Walk the recorded cwd first when it diverges from the workspace
        // root, so cwd-relative entries appear ahead of duplicates surfaced by
        // the workspace walk.
        {
            let mut ctx = SearchContext {
                needle: &needle,
                limit,
                prefix_hits: &mut prefix_hits,
                substring_hits: &mut substring_hits,
                seen: &mut seen,
                cancelled: &never_cancelled,
            };

            let cwd_diverges = self
                .cwd
                .as_deref()
                .map(|c| c != self.root.as_path())
                .unwrap_or(false);
            if cwd_diverges && let Some(cwd) = self.cwd.as_deref() {
                walk_for_completions(
                    cwd,
                    cwd,
                    &mut ctx,
                    self.completion_walk_depth,
                    self.follow_links,
                );
                add_local_reference_completions(
                    cwd,
                    cwd,
                    &mut ctx,
                    self.completion_walk_depth,
                    self.follow_links,
                );
            }
            walk_for_completions(
                &self.root,
                &self.root,
                &mut ctx,
                self.completion_walk_depth,
                self.follow_links,
            );
            add_local_reference_completions(
                &self.root,
                &self.root,
                &mut ctx,
                self.completion_walk_depth,
                self.follow_links,
            );
        }

        prefix_hits.sort();
        substring_hits.sort();
        prefix_hits.extend(substring_hits);
        prefix_hits.truncate(limit);
        prefix_hits
    }

    /// One full completion walk with no needle: every discoverable display
    /// string from the workspace walk plus the divergent-cwd walk (and the
    /// always-discoverable AI dot-directories), deduped, in walk order.
    /// Pair with [`rank_completion_candidates`] so the composer can filter
    /// per keystroke without re-walking the filesystem (#3757).
    ///
    /// Needle-gated local path-reference completions are NOT included;
    /// callers must fall back to [`Workspace::completions`] for path-like
    /// needles (starting with `.` or containing a separator).
    #[must_use]
    #[cfg(test)]
    pub fn completion_candidates(&self) -> Vec<String> {
        let never_cancelled = || false;
        self.completion_candidates_inner(usize::MAX, false, &never_cancelled)
    }

    /// Candidate collection used by the composer background worker. The walk
    /// is hard-capped and checks `cancelled` between filesystem iterator
    /// steps. A single slow iterator step may still return late, but it runs on
    /// the discovery worker and can never park terminal input (#4365).
    pub(crate) fn completion_discovery_candidates(
        &self,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<String> {
        self.completion_candidates_inner(limit, true, cancelled)
    }

    fn completion_candidates_inner(
        &self,
        limit: usize,
        include_local_references: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<String> {
        if limit == 0 || cancelled() {
            return Vec::new();
        }
        let mut prefix_hits: Vec<String> = Vec::new();
        let mut substring_hits: Vec<String> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        {
            let mut ctx = SearchContext {
                needle: "",
                limit,
                prefix_hits: &mut prefix_hits,
                substring_hits: &mut substring_hits,
                seen: &mut seen,
                cancelled,
            };
            let cwd_diverges = self
                .cwd
                .as_deref()
                .map(|c| c != self.root.as_path())
                .unwrap_or(false);
            if cwd_diverges && let Some(cwd) = self.cwd.as_deref() {
                walk_for_completions(
                    cwd,
                    cwd,
                    &mut ctx,
                    self.completion_walk_depth,
                    self.follow_links,
                );
                if include_local_references {
                    add_all_local_reference_completions(
                        cwd,
                        cwd,
                        &mut ctx,
                        self.completion_walk_depth,
                        self.follow_links,
                    );
                }
            }
            walk_for_completions(
                &self.root,
                &self.root,
                &mut ctx,
                self.completion_walk_depth,
                self.follow_links,
            );
            if include_local_references {
                add_all_local_reference_completions(
                    &self.root,
                    &self.root,
                    &mut ctx,
                    self.completion_walk_depth,
                    self.follow_links,
                );
            }
        }
        // Empty needle routes everything into prefix_hits.
        prefix_hits
    }

    /// Deterministic directory-browser completions for `@` mentions.
    ///
    /// Unlike [`Workspace::completions`], this mode does not fuzzy-rank across
    /// the full workspace. It locks onto the directory part of `partial` and
    /// returns only that directory's immediate children in case-insensitive
    /// alphabetical order.
    #[must_use]
    #[cfg(test)]
    pub fn browser_completions(&self, partial: &str, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        let never_cancelled = || false;
        let mut entries =
            self.browser_completion_candidates_inner(partial, usize::MAX, &never_cancelled);
        entries.truncate(limit);
        entries
    }

    /// Background-worker directory-browser collection. It stops at `limit`
    /// before sorting, so a directory with millions of direct children cannot
    /// grow the cache without bound.
    pub(crate) fn browser_completion_discovery_candidates(
        &self,
        partial: &str,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<String> {
        self.browser_completion_candidates_inner(partial, limit, cancelled)
    }

    fn browser_completion_candidates_inner(
        &self,
        partial: &str,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<String> {
        if limit == 0 || cancelled() {
            return Vec::new();
        }

        let normalized = partial.replace('\\', "/");
        let trimmed = normalized.trim_start_matches('/');
        let (dir_part, name_part) = match trimmed.rsplit_once('/') {
            Some((dir, name)) => (dir.trim_end_matches('/'), name),
            None => ("", trimmed),
        };
        let Some(safe_dir_part) = browser_completion_dir_part(dir_part) else {
            return Vec::new();
        };
        let dir = if safe_dir_part.as_os_str().is_empty() {
            self.root.clone()
        } else {
            self.root.join(&safe_dir_part)
        };
        if !dir.is_dir() {
            return Vec::new();
        }
        let display_dir_part = safe_dir_part.to_string_lossy().replace('\\', "/");

        let show_hidden = name_part.starts_with('.');
        let needle = name_part.to_lowercase();
        let mut entries = Vec::new();

        let mut builder = WalkBuilder::new(&dir);
        builder
            .hidden(!show_hidden)
            .follow_links(self.follow_links)
            .max_depth(Some(1));
        let _ = builder.add_custom_ignore_filename(".deepseekignore");

        let mut visited = 0usize;
        for entry in builder.build().flatten() {
            if visited >= limit || cancelled() {
                break;
            }
            visited = visited.saturating_add(1);
            let path = entry.path();
            if path == dir || path_is_excluded_from_discovery(&self.root, path) {
                continue;
            }
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() && !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if !needle.is_empty() && !name.to_lowercase().starts_with(&needle) {
                continue;
            }
            let mut candidate = if display_dir_part.is_empty() {
                name.to_string()
            } else {
                format!("{display_dir_part}/{name}")
            };
            if file_type.is_dir() {
                candidate.push('/');
            }
            entries.push(candidate);
        }

        entries.sort_by_cached_key(|entry| entry.to_lowercase());
        entries
    }
}

fn browser_completion_dir_part(dir_part: &str) -> Option<PathBuf> {
    let mut safe = PathBuf::new();
    for component in Path::new(dir_part).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => safe.push(part),
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    Some(safe)
}

/// Default directory depth walked when surfacing file-mention completions.
/// Set high enough that conventionally nested source trees (Java/.NET/web
/// projects routinely reach 7-9 levels) stay reachable, while a `0` override
/// removes the limit entirely. Keeps Tab snappy in deep monorepos via the
/// `.gitignore`-aware walk and per-keypress candidate caps (#2488).
pub const DEFAULT_COMPLETIONS_WALK_DEPTH: usize = 10;

fn normalize_completion_walk_depth(depth: usize) -> Option<usize> {
    if depth == 0 { None } else { Some(depth) }
}

#[cfg(test)]
fn child_completion_walk_depth(depth: Option<usize>) -> Option<usize> {
    depth.map(|depth| depth.saturating_sub(1))
}

/// Hard cap on the number of `(file or directory)` entries indexed by
/// [`Workspace::build_file_index`]. The fuzzy-resolve index is a
/// convenience for [`Workspace::fuzzy_resolve`]; missing entries fall
/// back to literal-path resolution. Capping here keeps the first
/// `fuzzy_resolve` call bounded on huge workspaces (#697 reported a
/// ~10s hang on the first turn). For typical projects 50K is well
/// above the actual entry count and the cap is a no-op.
#[cfg(test)]
const FILE_INDEX_MAX_ENTRIES: usize = 50_000;

/// Configure a `WalkBuilder` for workspace discovery: hidden files,
/// depth-limited, custom `.deepseekignore` honored, and gitignore overrides
/// for AI-tool dot-directories so `@`-completion finds them even when
/// they're gitignored. Symlink following is controlled by `follow_links`.
fn discovery_walk_builder(
    root: &Path,
    max_depth: Option<usize>,
    follow_links: bool,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(true).follow_links(follow_links);
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }
    let _ = builder.add_custom_ignore_filename(".deepseekignore");
    builder
}

/// Walk the AI-tool dot-directories (`.deepseek/`, `.cursor/`, `.claude/`,
/// `.agents/`) with gitignore disabled so their contents are discoverable
/// even when the project's `.gitignore` / `.ignore` excludes them.
fn walk_always_discoverable_dirs(
    walk_root: &Path,
    display_root: &Path,
    ctx: &mut SearchContext<'_>,
    max_depth: Option<usize>,
    follow_links: bool,
) {
    for dir_name in DISCOVERY_ALWAYS_DIRS {
        if ctx.should_stop() {
            break;
        }
        let dot_dir = walk_root.join(dir_name);
        // A walker follows a symlink passed as its root even when ordinary
        // symlink following is off. Keep that opt-out for the extra roots too.
        if !dot_dir.is_dir() || (!follow_links && dot_dir.is_symlink()) {
            continue;
        }
        let mut builder = WalkBuilder::new(&dot_dir);
        builder
            .hidden(true)
            .follow_links(follow_links)
            .git_ignore(false)
            .ignore(false);
        if let Some(depth) = max_depth {
            builder.max_depth(Some(depth.saturating_sub(1)));
        }
        for entry in builder.build().flatten() {
            if ctx.should_stop() {
                break;
            }
            let path = entry.path();
            // Exclude machine-generated bulk (e.g. .deepseek/snapshots/)
            // even though gitignore is disabled for this walk.
            if path_is_excluded_from_discovery(walk_root, path) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(display_root) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if rel_str.is_empty() {
                continue;
            }
            let abs = path.to_path_buf();
            if !ctx.remember(abs) {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
            let candidate = if is_dir {
                format!("{rel_str}/")
            } else {
                rel_str.clone()
            };
            ctx.push_match(candidate);
        }
    }
}

fn walk_for_completions(
    walk_root: &Path,
    display_root: &Path,
    ctx: &mut SearchContext<'_>,
    max_depth: Option<usize>,
    follow_links: bool,
) {
    let builder = discovery_walk_builder(walk_root, max_depth, follow_links);

    for entry in builder.build().flatten() {
        if ctx.should_stop() {
            break;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(display_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }
        // Dedup across the (cwd, workspace) double-walk by absolute path; we
        // want the cwd-relative display when both walks see the same file.
        let abs = path.to_path_buf();
        if !ctx.remember(abs) {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        let candidate = if is_dir {
            format!("{rel_str}/")
        } else {
            rel_str.clone()
        };
        ctx.push_match(candidate);
    }

    // Also walk the AI-tool dot-directories with gitignore disabled so
    // `.deepseek/`, `.cursor/`, etc. are always discoverable.
    walk_always_discoverable_dirs(walk_root, display_root, ctx, max_depth, follow_links);
}

const LOCAL_REFERENCE_SCAN_LIMIT: usize = 4096;

#[cfg(test)]
fn add_local_reference_completions(
    root: &Path,
    display_root: &Path,
    ctx: &mut SearchContext<'_>,
    max_depth: Option<usize>,
    follow_links: bool,
) {
    if !should_try_local_reference_completion(ctx.needle) {
        return;
    }

    for path in local_reference_paths(root, LOCAL_REFERENCE_SCAN_LIMIT, max_depth, follow_links) {
        if ctx.should_stop() {
            break;
        }
        let Ok(rel) = path.strip_prefix(display_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() || !ctx.remember(path.clone()) {
            continue;
        }
        ctx.push_match(rel_str);
    }
}

/// Add hidden/ignored candidates to the background cache even when the
/// current needle is empty. The old UI path could defer this walk until a
/// path-like query existed; the background cache must be self-contained so no
/// later keystroke falls back to synchronous discovery.
fn add_all_local_reference_completions(
    root: &Path,
    display_root: &Path,
    ctx: &mut SearchContext<'_>,
    max_depth: Option<usize>,
    follow_links: bool,
) {
    if ctx.should_stop() {
        return;
    }
    let paths = local_reference_paths_with_cancel(
        root,
        LOCAL_REFERENCE_SCAN_LIMIT,
        max_depth,
        follow_links,
        ctx.cancelled,
    );
    for path in paths {
        if ctx.should_stop() {
            break;
        }
        let Ok(rel) = path.strip_prefix(display_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() || !ctx.remember(path.clone()) {
            continue;
        }
        ctx.push_match(rel_str);
    }
}

/// Rank pre-collected completion candidates for `partial` the same way
/// `Workspace::completions` ranks live walk hits: case-insensitive prefix
/// matches first, then substring matches, each bucket alphabetical, truncated
/// to `limit` (#3757).
#[must_use]
pub fn rank_completion_candidates(
    candidates: &[String],
    partial: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = partial.to_lowercase();
    let mut prefix_hits: Vec<String> = Vec::new();
    let mut substring_hits: Vec<String> = Vec::new();
    for candidate in candidates {
        let lower = candidate.to_lowercase();
        if needle.is_empty() || lower.starts_with(&needle) {
            prefix_hits.push(candidate.clone());
        } else if lower.contains(&needle) {
            substring_hits.push(candidate.clone());
        }
    }
    prefix_hits.sort();
    substring_hits.sort();
    prefix_hits.extend(substring_hits);
    prefix_hits.truncate(limit);
    prefix_hits
}

#[cfg(test)]
fn should_try_local_reference_completion(needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    // A bare separator or dot isn't an actionable path yet. Without this
    // guard, a single `@/` keystroke triggers a `LOCAL_REFERENCE_SCAN_LIMIT`
    // (4096-path) walk on the UI thread for #1921 — on WSL2 with a
    // `/mnt/c/...` workspace each entry crosses Windows-host I/O and the
    // composer appears frozen for seconds to minutes.
    if matches!(needle, "/" | "\\" | "." | "..") {
        return false;
    }
    needle.starts_with('.') || needle.contains('/') || needle.contains('\\')
}

#[cfg(test)]
fn local_reference_paths(
    root: &Path,
    limit: usize,
    max_depth: Option<usize>,
    follow_links: bool,
) -> Vec<PathBuf> {
    let never_cancelled = || false;
    local_reference_paths_with_cancel(root, limit, max_depth, follow_links, &never_cancelled)
}

fn local_reference_paths_with_cancel(
    root: &Path,
    limit: usize,
    max_depth: Option<usize>,
    follow_links: bool,
    cancelled: &dyn Fn() -> bool,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(follow_links)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false);
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }
    let _ = builder.add_custom_ignore_filename(".deepseekignore");
    let root_for_filter = root.to_path_buf();
    builder.filter_entry(move |entry| {
        !should_skip_unignored_discovery_entry(&root_for_filter, entry.path())
    });

    for entry in builder.build().flatten() {
        if out.len() >= limit || cancelled() {
            break;
        }
        let path = entry.path();
        if path == root {
            continue;
        }
        if entry
            .file_type()
            .is_some_and(|ft| ft.is_file() || ft.is_dir())
        {
            out.push(path.to_path_buf());
        }
    }
    out
}

impl Clone for Workspace {
    fn clone(&self) -> Self {
        // Don't carry the cached file_index — clones get a fresh OnceLock so
        // they don't pin a stale snapshot of the previous owner's tree.
        Self {
            root: self.root.clone(),
            cwd: self.cwd.clone(),
            #[cfg(test)]
            file_index: OnceLock::new(),
            completion_walk_depth: self.completion_walk_depth,
            follow_links: self.follow_links,
        }
    }
}

fn expand_mention_home(path: &str) -> PathBuf {
    if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Truncate `s` to at most `max_bytes`, snapping down to a UTF-8 char
/// boundary so the result is always valid. Returns the slice and whether any
/// truncation happened.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> (&str, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// Configuration for working-set tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetConfig {
    /// Maximum number of entries to keep.
    pub max_entries: usize,
    /// Maximum number of paths to pin during compaction.
    pub max_pinned_paths: usize,
    /// Maximum characters to scan per text block when pinning messages.
    pub max_scan_chars: usize,
    /// Maximum entries to show in the system prompt block.
    pub max_prompt_entries: usize,
    /// Cache-maximal context mode (#528): when enabled, the working-set block
    /// materializes the full current contents of the top active files into the
    /// system prompt (deterministic order, size-bounded) instead of only a
    /// path list. The contents stay byte-stable while the files are unchanged,
    /// so DeepSeek's KV prefix cache keeps hitting; editing a file cache-misses
    /// from that file's block onward. Off by default — existing behavior is the
    /// path list only.
    #[serde(default)]
    pub cache_maximal: bool,
    /// Per-file byte cap for materialized contents in cache-maximal mode.
    #[serde(default = "default_max_resident_file_bytes")]
    pub max_resident_file_bytes: usize,
    /// Total byte cap across all materialized files in cache-maximal mode.
    #[serde(default = "default_max_total_resident_bytes")]
    pub max_total_resident_bytes: usize,
}

fn default_max_resident_file_bytes() -> usize {
    24_000
}

fn default_max_total_resident_bytes() -> usize {
    96_000
}

impl Default for WorkingSetConfig {
    fn default() -> Self {
        Self {
            max_entries: 16,
            max_pinned_paths: 8,
            max_scan_chars: 2_000,
            max_prompt_entries: 8,
            cache_maximal: false,
            max_resident_file_bytes: default_max_resident_file_bytes(),
            max_total_resident_bytes: default_max_total_resident_bytes(),
        }
    }
}

/// The source that most recently updated an entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkingSetSource {
    UserMessage,
    ToolInput,
    ToolOutput,
    Rebuild,
}

/// A single working-set entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetEntry {
    /// Workspace-relative path string.
    pub path: String,
    /// Whether the path is a directory (best-effort).
    pub is_dir: bool,
    /// Whether the path exists on disk (best-effort).
    pub exists: bool,
    /// Number of times this path was observed.
    pub touches: u32,
    /// The last observed turn index.
    pub last_turn: u64,
    /// The last update source.
    pub last_source: WorkingSetSource,
}

impl WorkingSetEntry {
    fn new(path: String, exists: bool, is_dir: bool, turn: u64, source: WorkingSetSource) -> Self {
        Self {
            path,
            is_dir,
            exists,
            touches: 1,
            last_turn: turn,
            last_source: source,
        }
    }
}

/// Repo-aware working-set state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkingSet {
    /// Tracking configuration.
    pub config: WorkingSetConfig,
    /// Monotonic turn counter (increments on user messages).
    pub turn: u64,
    /// Path entries keyed by workspace-relative path.
    pub entries: HashMap<String, WorkingSetEntry>,
    /// Latest bounded navigation references delivered in this conversation.
    /// Not body readership, current source freshness or read authority. This
    /// field is never rendered into the session-pinned prompt prefix.
    #[serde(default)]
    pub delivered_code_refs: Vec<CodeReadReference>,
    #[serde(skip)]
    navigation_delivery_initialized: bool,
}

impl WorkingSet {
    /// Only provider acceptance of the projected request establishes delivery;
    /// intermediate program reads are absent from these messages.
    pub(crate) fn observe_navigation_delivery(&mut self, messages: &[Message]) {
        // Inspect inherited context on the first accepted request, then only
        // the current result tail. Do not reparse every historical payload on
        // every model step just to rediscover the same references.
        let start = if self.navigation_delivery_initialized {
            messages
                .iter()
                .rposition(|message| message.role == "assistant")
                .map_or(0, |index| index + 1)
        } else {
            0
        };
        self.navigation_delivery_initialized = true;
        for message in &messages[start..] {
            for block in &message.content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    self.record_delivered_navigation(delivered_navigation_refs(content));
                }
            }
        }
    }

    fn record_delivered_navigation(&mut self, refs: Vec<CodeReadReference>) {
        for reference in refs {
            self.delivered_code_refs.retain(|old| {
                old.path != reference.path || old.expected_revision == reference.expected_revision
            });
            if !self.delivered_code_refs.contains(&reference) {
                self.delivered_code_refs.push(reference);
                if self.delivered_code_refs.len() > MAX_DELIVERED_REFS {
                    self.delivered_code_refs.remove(0);
                }
            }
        }
    }

    /// Advance to the next turn.
    pub fn next_turn(&mut self) {
        self.turn = self.turn.saturating_add(1);
    }

    /// Observe a user message and update the working set.
    pub fn observe_user_message(&mut self, text: &str, workspace: &Path) {
        self.next_turn();
        let paths = extract_paths_from_text(text);
        self.record_candidates(paths, workspace, WorkingSetSource::UserMessage);
    }

    /// Observe a tool call (input and optional output).
    pub fn observe_tool_call(
        &mut self,
        tool_name: &str,
        input: &Value,
        output: Option<&str>,
        workspace: &Path,
    ) {
        let input_candidates = extract_paths_from_value(input, Some(tool_name));
        self.record_candidates(input_candidates, workspace, WorkingSetSource::ToolInput);

        if let Some(text) = output {
            let output_candidates = extract_paths_from_text(text);
            self.record_candidates(output_candidates, workspace, WorkingSetSource::ToolOutput);
        }
    }

    /// Rebuild the working set from existing messages (best effort).
    ///
    /// This is used when syncing a resumed session.
    pub fn rebuild_from_messages(&mut self, messages: &[Message], workspace: &Path) {
        self.entries.clear();
        self.turn = 0;
        self.delivered_code_refs.clear();
        self.navigation_delivery_initialized = true;
        let mut offered = Vec::new();

        for message in messages {
            // A subsequent assistant response confirms the historical request,
            // unlike a trailing tool result left at a crash boundary. Fresh
            // child seeds do not invoke this resume/compaction rebuild.
            if message.role == "assistant" {
                self.record_delivered_navigation(std::mem::take(&mut offered));
            }
            for block in &message.content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    offered.extend(delivered_navigation_refs(content));
                    if offered.len() > MAX_DELIVERED_REFS {
                        offered.drain(..offered.len() - MAX_DELIVERED_REFS);
                    }
                }
            }
            if message.role == "user" {
                self.next_turn();
            }
            let candidates = extract_paths_from_message(message);
            if candidates.is_empty() {
                continue;
            }
            self.record_candidates(candidates, workspace, WorkingSetSource::Rebuild);
        }
    }

    /// Render a compact working-set block for the system prompt.
    ///
    /// Byte-stable across `next_turn()` calls when no new paths are observed
    /// (#280): the rendered lines drop the turn-relative `touches` and
    /// `last seen N turn(s) ago` fields, and the order is taken from
    /// `sorted_for_prompt` (turn-agnostic) instead of `sorted_entries`.
    /// The block lands in the system prompt before the historical
    /// conversation; any byte that drifts here cache-misses everything that
    /// follows in DeepSeek's KV prefix cache.
    pub fn summary_block(&self, workspace: &Path) -> Option<String> {
        // Only stat-verified paths reach the model. Prose observation happily
        // records tokens that merely look like paths ("120x40",
        // "codewhale-hq/CodeWhale"), and a fabricated Active-paths line teaches the
        // model false workspace facts it then spends turns disproving.
        // Re-statting at render time also drops files deleted mid-session.
        // Bytes only change when the filesystem genuinely changed — the same
        // exception the #280 stability contract already makes for newly
        // observed paths.
        let prompt_entries: Vec<(&WorkingSetEntry, bool)> = self
            .sorted_for_prompt()
            .into_iter()
            .filter_map(|entry| {
                let metadata = fs::metadata(workspace.join(&entry.path)).ok()?;
                Some((entry, metadata.is_dir()))
            })
            .take(self.config.max_prompt_entries)
            .collect();

        let repo_summary = summarize_repo_root(workspace);

        if repo_summary.is_none() && prompt_entries.is_empty() {
            return None;
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push("## Repo Working Set".to_string());

        if let Some(summary) = repo_summary {
            lines.push(summary);
        }

        if !prompt_entries.is_empty() {
            lines.push("Active paths (prioritize these):".to_string());
            for (entry, is_dir) in &prompt_entries {
                let kind = if *is_dir { "dir" } else { "file" };
                lines.push(format!("- {} ({kind})", entry.path));
            }
        }

        lines.push(
            "When in doubt, use tools to verify and keep changes focused on the working set."
                .to_string(),
        );

        // Cache-maximal mode (#528): append the full current contents of the
        // top active files so the model reads live source each turn instead of
        // re-fetching it with tools. Kept after the path list and bounded by
        // per-file and total byte caps; order follows `sorted_for_prompt` so
        // the block is byte-stable while the files are unchanged.
        if self.cache_maximal_enabled() && !prompt_entries.is_empty() {
            let content_entries: Vec<&WorkingSetEntry> =
                prompt_entries.iter().map(|(entry, _)| *entry).collect();
            self.append_resident_file_contents(&mut lines, workspace, &content_entries);
        }

        Some(lines.join("\n"))
    }

    /// Whether cache-maximal context mode is active: explicit config, or the
    /// `CODEWHALE_CACHE_MAXIMAL` env toggle (`1`/`true`/`on`/`yes`). The env
    /// value is constant for the process, so the rendered block stays
    /// byte-stable turn-over-turn.
    fn cache_maximal_enabled(&self) -> bool {
        if self.config.cache_maximal {
            return true;
        }
        match std::env::var("CODEWHALE_CACHE_MAXIMAL") {
            Ok(v) => matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            ),
            Err(_) => false,
        }
    }

    /// Render `### Active file contents` blocks for the resident files, honoring
    /// the per-file and total byte caps. Unreadable or non-UTF-8 files are noted
    /// rather than skipped silently so the omission is visible to the model.
    fn append_resident_file_contents(
        &self,
        lines: &mut Vec<String>,
        workspace: &Path,
        prompt_entries: &[&WorkingSetEntry],
    ) {
        let mut header_pushed = false;
        let mut total_bytes: usize = 0;
        let mut omitted: usize = 0;

        for entry in prompt_entries {
            if entry.is_dir || !entry.exists {
                continue;
            }
            if total_bytes >= self.config.max_total_resident_bytes {
                omitted += 1;
                continue;
            }

            let abs = workspace.join(&entry.path);
            if crate::tools::file::enforce_read_denylist(&abs, "working_set").is_err() {
                if !header_pushed {
                    lines.push("### Active file contents (cache-resident)".to_string());
                    header_pushed = true;
                }
                lines.push(format!(
                    "<!-- file: {} (read deny-list, skipped) -->",
                    entry.path
                ));
                continue;
            }
            let body = match std::fs::read_to_string(&abs) {
                Ok(text) => text,
                Err(_) => {
                    if !header_pushed {
                        lines.push("### Active file contents (cache-resident)".to_string());
                        header_pushed = true;
                    }
                    lines.push(format!(
                        "<!-- file: {} (unreadable, skipped) -->",
                        entry.path
                    ));
                    continue;
                }
            };

            if !header_pushed {
                lines.push("### Active file contents (cache-resident)".to_string());
                header_pushed = true;
            }

            let remaining_total = self
                .config
                .max_total_resident_bytes
                .saturating_sub(total_bytes);
            let cap = self.config.max_resident_file_bytes.min(remaining_total);
            let (shown, truncated) = truncate_on_char_boundary(&body, cap);
            total_bytes += shown.len();

            lines.push(format!("<!-- file: {} -->", entry.path));
            lines.push("```".to_string());
            lines.push(shown.to_string());
            if truncated {
                lines.push(format!(
                    "<!-- ...{} more bytes truncated for prompt budget -->",
                    body.len().saturating_sub(shown.len())
                ));
            }
            lines.push("```".to_string());
        }

        if omitted > 0 {
            lines.push(format!(
                "<!-- {omitted} additional active file(s) omitted from the cache-resident budget -->"
            ));
        }
    }

    fn record_candidates(
        &mut self,
        candidates: Vec<String>,
        workspace: &Path,
        source: WorkingSetSource,
    ) {
        if candidates.is_empty() {
            return;
        }

        let workspace_canon = workspace.canonicalize().ok();

        for raw in candidates {
            let Some(normalized) = normalize_candidate(&raw) else {
                continue;
            };
            let Some((rel, exists, is_dir)) =
                relativize_candidate(&normalized, workspace, workspace_canon.as_deref())
            else {
                continue;
            };
            self.record_path(rel, exists, is_dir, source);
        }

        self.prune();
    }

    fn record_path(&mut self, rel: String, exists: bool, is_dir: bool, source: WorkingSetSource) {
        match self.entries.get_mut(&rel) {
            Some(entry) => {
                entry.exists |= exists;
                entry.is_dir |= is_dir;
                entry.touches = entry.touches.saturating_add(1);
                entry.last_turn = self.turn;
                entry.last_source = source;
            }
            None => {
                let entry = WorkingSetEntry::new(rel.clone(), exists, is_dir, self.turn, source);
                let _ = self.entries.insert(rel, entry);
            }
        }
    }

    fn prune(&mut self) {
        let max_entries = self.config.max_entries;
        if self.entries.len() <= max_entries {
            return;
        }

        // Rank by score ascending and drop the lowest until within bounds.
        let mut ranked: Vec<(String, i64)> = self
            .entries
            .values()
            .map(|entry| (entry.path.clone(), score_entry(entry, self.turn)))
            .collect();
        ranked.sort_by_key(|a| a.1);

        let to_remove = self.entries.len().saturating_sub(max_entries);
        for (path, _) in ranked.into_iter().take(to_remove) {
            let _ = self.entries.remove(&path);
        }
    }

    /// `sorted_entries` mixes in a recency bonus from `self.turn`, so its
    /// output reorders as turns advance even when no new paths are touched —
    /// that movement would cross `max_prompt_entries` boundaries and bust the
    /// KV prefix cache (#280). Compaction pinning still uses the recency-aware
    /// `sorted_entries`; only the prompt-facing surface is stabilised here.
    fn sorted_for_prompt(&self) -> Vec<&WorkingSetEntry> {
        let mut entries: Vec<&WorkingSetEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.touches.cmp(&a.touches).then_with(|| a.path.cmp(&b.path)));
        entries
    }
}

fn score_entry(entry: &WorkingSetEntry, current_turn: u64) -> i64 {
    let age = current_turn.saturating_sub(entry.last_turn);
    let recency_bonus = match age {
        0 => 6,
        1 => 4,
        2 => 3,
        3..=5 => 2,
        6..=10 => 1,
        _ => 0,
    };
    i64::from(entry.touches) * 4 + recency_bonus
}

fn normalize_candidate(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
        )
    });
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn relativize_candidate(
    candidate: &str,
    workspace: &Path,
    workspace_canon: Option<&Path>,
) -> Option<(String, bool, bool)> {
    let candidate_path = Path::new(candidate);

    // Reject obvious URLs and non-paths early.
    if candidate.contains("://") {
        return None;
    }

    let (rel_path, abs_path) = if candidate_path.is_absolute() {
        let within_workspace = workspace_canon
            .map(|ws| candidate_path.starts_with(ws))
            .unwrap_or_else(|| candidate_path.starts_with(workspace));
        if !within_workspace {
            return None;
        }
        let rel = candidate_path.strip_prefix(workspace).ok()?.to_path_buf();
        (rel, candidate_path.to_path_buf())
    } else {
        if starts_with_parent_dir(candidate_path) {
            return None;
        }
        let rel = clean_relative(candidate_path);
        let abs = workspace.join(&rel);
        (rel, abs)
    };

    let metadata = fs::metadata(&abs_path).ok();
    let exists = metadata.is_some();
    let is_dir = metadata
        .as_ref()
        .map(fs::Metadata::is_dir)
        .unwrap_or_else(|| candidate.ends_with('/'));

    let rel_string = path_to_string(&rel_path)?;
    Some((rel_string, exists, is_dir))
}

fn starts_with_parent_dir(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(std::path::Component::ParentDir)
    )
}

fn clean_relative(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut parts: Vec<PathBuf> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = parts.pop();
            }
            Component::Normal(p) => parts.push(PathBuf::from(p)),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part);
    }
    out
}

fn path_to_string(path: &Path) -> Option<String> {
    path.as_os_str().to_str().map(|s| s.replace('\\', "/"))
}

fn extract_paths_from_message(message: &Message) -> Vec<String> {
    let mut paths = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => {
                paths.extend(extract_paths_from_text(text));
            }
            ContentBlock::ToolUse { input, .. } => {
                paths.extend(extract_paths_from_value(input, None));
            }
            ContentBlock::ToolResult { content, .. } => {
                paths.extend(extract_paths_from_text(content));
            }
            ContentBlock::Thinking { .. }
            | ContentBlock::ServerToolUse { .. }
            | ContentBlock::ToolSearchToolResult { .. }
            | ContentBlock::CodeExecutionToolResult { .. }
            | ContentBlock::ImageUrl { .. } => {}
        }
    }
    paths
}

fn extract_paths_from_value(value: &Value, tool_hint: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    extract_paths_from_value_inner(value, tool_hint, None, &mut out);
    out
}

fn extract_paths_from_value_inner(
    value: &Value,
    tool_hint: Option<&str>,
    key_hint: Option<&str>,
    out: &mut Vec<String>,
) {
    match value {
        Value::String(s) => {
            let key_suggests_path = key_hint.map(key_is_path_like).unwrap_or(false);
            if key_suggests_path || looks_like_path(s) {
                out.extend(extract_paths_from_text(s));
                if key_suggests_path && !s.contains('/') && !s.contains('\\') {
                    out.push(s.to_string());
                }
            } else if tool_hint == Some("exec_shell") && s.len() < 400 {
                out.extend(extract_paths_from_text(s));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                extract_paths_from_value_inner(item, tool_hint, key_hint, out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                extract_paths_from_value_inner(v, tool_hint, Some(k.as_str()), out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn key_is_path_like(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("path")
        || lower.contains("file")
        || lower.contains("dir")
        || lower.contains("cwd")
        || lower.contains("workspace")
        || lower.contains("root")
        || lower == "target"
}

fn looks_like_path(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return true;
    }
    match Path::new(trimmed).extension().and_then(OsStr::to_str) {
        Some(ext) => COMMON_EXTENSIONS.contains(&ext),
        None => false,
    }
}

const COMMON_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "txt", "json", "yaml", "yml", "ts", "tsx", "js", "jsx", "py", "go", "java",
    "c", "cc", "cpp", "h", "hpp", "sh", "bash", "zsh", "sql", "html", "css", "scss",
];

fn extract_paths_from_text(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let re = path_regex();
    // Matches cannot cross line endings, and every arm needs a dot or separator.
    text.lines()
        .filter(|line| line.contains('/') || line.contains('\\') || line.contains('.'))
        .flat_map(|line| re.find_iter(line))
        .map(|m| m.as_str().to_string())
        .filter(|s| looks_like_path(s))
        .collect()
}

fn path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Path-ish tokens with separators or file extensions.
        Regex::new(
            r#"(?x)
            (?:
                (?:[A-Za-z]:\\)?                # optional Windows drive
                (?:\./|\../|/)?                 # optional leading
                [A-Za-z0-9._-]+
                (?:[/\\][A-Za-z0-9._-]+)+
                (?:\.[A-Za-z0-9]{1,8})?         # optional extension
            )
            |
            (?:
                [A-Za-z0-9._-]+\.[A-Za-z0-9]{1,8}
            )
            "#,
        )
        .expect("path regex should compile")
    })
}

fn summarize_repo_root(workspace: &Path) -> Option<String> {
    let key_files = detect_key_files(workspace);
    let top_dirs = list_top_level_dirs(workspace, 8);

    if key_files.is_empty() && top_dirs.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    if !key_files.is_empty() {
        parts.push(format!("Key files: {}", key_files.join(", ")));
    }
    if !top_dirs.is_empty() {
        parts.push(format!("Top-level dirs: {}", top_dirs.join(", ")));
    }
    Some(parts.join("\n"))
}

fn detect_key_files(workspace: &Path) -> Vec<String> {
    const CANDIDATES: &[&str] = &[
        "Cargo.toml",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "Makefile",
    ];

    CANDIDATES
        .iter()
        .filter_map(|name| {
            let path = workspace.join(name);
            if path.exists() {
                Some((*name).to_string())
            } else {
                None
            }
        })
        .collect()
}

fn list_top_level_dirs(workspace: &Path, limit: usize) -> Vec<String> {
    let mut dirs = Vec::new();
    let entries = match fs::read_dir(workspace) {
        Ok(entries) => entries,
        Err(_) => return dirs,
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };

        if name.starts_with('.') || IGNORED_ROOT_DIRS.contains(&name) {
            continue;
        }

        if let Ok(meta) = entry.metadata()
            && meta.is_dir()
        {
            dirs.push(name.to_string());
        }

        if dirs.len() >= limit {
            break;
        }
    }

    dirs.sort();
    dirs
}

const IGNORED_ROOT_DIRS: &[&str] = &["target", "node_modules", "dist", "build", ".git"];

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn workspace_file_search_discovery_respects_symlink_opt_out_for_ai_directories() {
        let workspace = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        fs::write(external.path().join("outside.rs"), "").unwrap();
        std::os::unix::fs::symlink(external.path(), workspace.path().join(".agents")).unwrap();
        for follow_links in [false, true] {
            let resolver = Workspace::with_cwd_depth_and_follow_links(
                workspace.path().to_path_buf(),
                None,
                DEFAULT_COMPLETIONS_WALK_DEPTH,
                follow_links,
            );
            let candidates = resolver.completion_discovery_candidates(100, &|| false);
            assert_eq!(
                candidates.iter().any(|path| path == ".agents/outside.rs"),
                follow_links
            );
        }
    }

    fn make_message(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn observe_user_message_tracks_paths() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        let file = src.join("lib.rs");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(&file, "pub fn x() {}").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message("Please check src/lib.rs", tmp.path());

        assert!(ws.entries.contains_key("src/lib.rs"));
        let entry = ws.entries.get("src/lib.rs").expect("entry");
        assert!(entry.exists);
        assert!(!entry.is_dir);
    }

    #[test]
    fn observe_tool_call_extracts_paths_from_input() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("Cargo.toml");
        fs::write(&file, "[package]\nname = \"x\"").expect("write");

        let mut ws = WorkingSet::default();
        let input = serde_json::json!({ "path": "Cargo.toml" });
        ws.observe_tool_call("read_file", &input, None, tmp.path());

        assert!(ws.entries.contains_key("Cargo.toml"));
    }

    #[test]
    fn summary_block_includes_repo_and_working_set() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"").expect("write");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("lib.rs"), "pub fn x() {}").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message("src/lib.rs", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        assert!(block.contains("Repo Working Set"));
        assert!(!block.contains("Workspace:"));
        assert!(block.contains("Cargo.toml"));
        assert!(block.contains("src"));
        assert!(block.contains("src/lib.rs"));
    }

    /// #280 regression: `summary_block` must produce byte-identical output
    /// across `next_turn()` advances when no new paths are touched. Prior to
    /// the fix, the rendered lines interpolated `entry.touches` and
    /// `self.turn - entry.last_turn`, both of which drift turn-over-turn even
    /// when the path set is unchanged. The drift busted DeepSeek's KV prefix
    /// cache on every user message because the working-set block lands in the
    /// system prompt before the historical conversation.
    #[test]
    fn summary_block_is_byte_stable_across_next_turn_when_no_new_paths_observed() {
        use crate::test_support::assert_byte_identical;

        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"").expect("write");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("a.rs"), "a").expect("write");
        fs::write(src.join("b.rs"), "b").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message("Edit src/a.rs and src/b.rs", tmp.path());

        let before = ws.summary_block(tmp.path()).expect("block before");
        ws.next_turn();
        let after = ws.summary_block(tmp.path()).expect("block after");

        assert_byte_identical(
            "summary_block must be stable across next_turn when no new paths touched",
            &before,
            &after,
        );
    }

    /// Companion to the byte-stability test: a fresh path *should* invalidate
    /// the block (the KV cache is allowed to miss when there's genuinely new
    /// signal), so the model still sees newly touched paths after the block
    /// stabilises across no-op turns.
    #[test]
    fn summary_block_changes_when_a_new_path_is_observed() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"").expect("write");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("a.rs"), "a").expect("write");
        fs::write(src.join("c.rs"), "c").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message("src/a.rs", tmp.path());
        let before = ws.summary_block(tmp.path()).expect("block before");

        ws.observe_user_message("src/c.rs", tmp.path());
        let after = ws.summary_block(tmp.path()).expect("block after");

        assert_ne!(before, after, "new path must update the rendered summary");
        assert!(after.contains("src/c.rs"));
    }

    #[test]
    fn summary_block_renders_only_paths_that_stat_verify() {
        // Prose observation records tokens that merely look like paths
        // ("120x40", "codewhale-hq/CodeWhale"); the rendered Active-paths list must
        // never teach the model a workspace fact the filesystem contradicts.
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("real.rs"), "real").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message(
            "Fix src/real.rs, test at 120x40/80x24, and check codewhale-hq/CodeWhale",
            tmp.path(),
        );

        let block = ws.summary_block(tmp.path()).expect("block");
        assert!(block.contains("- src/real.rs (file)"), "{block}");
        assert!(!block.contains("120x40"), "{block}");
        assert!(!block.contains("codewhale-hq/CodeWhale"), "{block}");

        // A file deleted mid-session falls out on the next render — the same
        // filesystem-changed exception #280 makes for newly observed paths.
        fs::remove_file(src.join("real.rs")).expect("remove");
        let after_delete = ws.summary_block(tmp.path());
        assert!(
            after_delete
                .as_deref()
                .is_none_or(|block| !block.contains("src/real.rs")),
            "{after_delete:?}"
        );
    }

    // ── Cache-maximal context mode (#528) ──
    // Tests drive the flag through `config.cache_maximal` directly so they
    // don't touch the process-wide `CODEWHALE_CACHE_MAXIMAL` env var (which
    // would race with parallel tests).

    fn cache_maximal_ws() -> WorkingSet {
        let mut ws = WorkingSet::default();
        ws.config.cache_maximal = true;
        ws
    }

    #[test]
    fn cache_maximal_off_keeps_path_list_only() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("lib.rs"), "pub fn hello() {}").expect("write");

        let mut ws = WorkingSet::default(); // cache_maximal defaults to false
        ws.observe_user_message("src/lib.rs", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        assert!(block.contains("src/lib.rs"), "path list still present");
        assert!(
            !block.contains("Active file contents"),
            "no materialized contents when the flag is off"
        );
        assert!(!block.contains("pub fn hello"));
    }

    #[test]
    fn cache_maximal_on_materializes_file_contents() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("lib.rs"), "pub fn hello() {}").expect("write");

        let mut ws = cache_maximal_ws();
        ws.observe_user_message("src/lib.rs", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        assert!(block.contains("Active file contents (cache-resident)"));
        assert!(block.contains("<!-- file: src/lib.rs -->"));
        assert!(block.contains("pub fn hello() {}"));
    }

    #[test]
    fn cache_maximal_directories_are_not_materialized() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");

        let mut ws = cache_maximal_ws();
        ws.observe_user_message("look in src/", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        // `src` is a dir; it appears in the path list but has no content block.
        assert!(!block.contains("<!-- file: src -->"));
    }

    #[test]
    fn cache_maximal_respects_per_file_byte_cap() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        let big = "x".repeat(10_000);
        fs::write(src.join("big.rs"), &big).expect("write");

        let mut ws = cache_maximal_ws();
        ws.config.max_resident_file_bytes = 100;
        ws.config.max_total_resident_bytes = 10_000;
        ws.observe_user_message("src/big.rs", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        assert!(block.contains("truncated for prompt budget"));
        // The full 10k body must not be inlined.
        assert!(!block.contains(&big));
    }

    #[test]
    fn cache_maximal_total_cap_omits_extra_files() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("a.rs"), "a".repeat(200)).expect("write");
        fs::write(src.join("b.rs"), "b".repeat(200)).expect("write");

        let mut ws = cache_maximal_ws();
        ws.config.max_resident_file_bytes = 200;
        ws.config.max_total_resident_bytes = 200; // only one file fits
        ws.observe_user_message("Edit src/a.rs and src/b.rs", tmp.path());
        let block = ws.summary_block(tmp.path()).expect("block");

        assert!(
            block.contains("omitted from the cache-resident budget"),
            "second file should be reported as omitted:\n{block}"
        );
    }

    #[test]
    fn cache_maximal_is_byte_stable_when_files_unchanged() {
        use crate::test_support::assert_byte_identical;

        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("a.rs"), "fn a() {}").expect("write");

        let mut ws = cache_maximal_ws();
        ws.observe_user_message("src/a.rs", tmp.path());
        let before = ws.summary_block(tmp.path()).expect("before");
        ws.next_turn();
        let after = ws.summary_block(tmp.path()).expect("after");

        assert_byte_identical(
            "cache-maximal block must be stable while files are unchanged (KV cache hit)",
            &before,
            &after,
        );
    }

    #[test]
    fn cache_maximal_changes_when_file_edited() {
        let tmp = TempDir::new().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).expect("mkdir");
        let file = src.join("a.rs");
        fs::write(&file, "fn a() {}").expect("write");

        let mut ws = cache_maximal_ws();
        ws.observe_user_message("src/a.rs", tmp.path());
        let before = ws.summary_block(tmp.path()).expect("before");

        fs::write(&file, "fn a() { todo!() }").expect("rewrite");
        let after = ws.summary_block(tmp.path()).expect("after");

        assert_ne!(before, after, "editing the file must change the block");
        assert!(after.contains("todo!()"));
    }

    #[test]
    fn extract_paths_from_message_picks_up_tool_results() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "tool_1".to_string(),
                content: "Changed src/compaction.rs".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        };

        let paths = extract_paths_from_message(&msg);
        assert!(paths.iter().any(|p| p.contains("src/compaction.rs")));
    }

    #[test]
    fn pinning_prefers_high_signal_paths() {
        let tmp = TempDir::new().expect("tempdir");
        fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        fs::write(tmp.path().join("src/a.rs"), "a").expect("write");
        fs::write(tmp.path().join("src/b.rs"), "b").expect("write");

        let mut ws = WorkingSet::default();
        ws.observe_user_message("src/a.rs", tmp.path());
        ws.observe_tool_call(
            "read_file",
            &serde_json::json!({ "path": "src/a.rs" }),
            Some("src/a.rs"),
            tmp.path(),
        );
        ws.observe_user_message("src/b.rs", tmp.path());

        let a_score = score_entry(ws.entries.get("src/a.rs").expect("a"), ws.turn);
        let b_score = score_entry(ws.entries.get("src/b.rs").expect("b"), ws.turn);
        assert!(a_score >= b_score);
    }

    #[test]
    fn estimate_tokens_is_available_for_future_budgeting() {
        use crate::compaction::estimate_tokens;
        let messages = vec![make_message("user", "src/main.rs")];
        assert!(estimate_tokens(&messages) > 0);
    }

    #[test]
    fn workspace_resolve_respects_cwd_and_workspace() {
        let tmp = TempDir::new().unwrap();

        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let bar = sub.join("bar.txt");
        std::fs::write(&bar, "bar").unwrap();

        let nested = tmp.path().join("nested/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let file_md = nested.join("file.md");
        std::fs::write(&file_md, "md").unwrap();

        // Construct with an explicit cwd so the test doesn't race with other
        // tests that mutate the real process cwd.
        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), Some(sub.clone()));

        // #101 repro #1: @bar.txt with cwd=sub MUST resolve via the cwd pass,
        // never to the bogus workspace path tmp/bar.txt (which doesn't exist).
        let res1 = ws.resolve("bar.txt").unwrap();
        assert_eq!(
            res1.canonicalize().unwrap_or(res1.clone()),
            bar.canonicalize().unwrap_or(bar.clone())
        );
        let wrong = tmp.path().join("bar.txt");
        assert_ne!(res1, wrong, "must not have routed to workspace fallback");

        // #101 repro #2: @nested/deep/file.md falls through to workspace root.
        let res2 = ws.resolve("nested/deep/file.md").unwrap();
        assert_eq!(
            res2.canonicalize().unwrap_or(res2),
            file_md.canonicalize().unwrap_or(file_md)
        );
    }

    /// Negative test (#101): a truly missing path returns `Err` with a path
    /// that callers can show to the user as a signal of failure.
    #[test]
    fn workspace_resolve_returns_err_for_truly_missing_path() {
        let tmp = TempDir::new().unwrap();
        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), Some(tmp.path().to_path_buf()));

        let res = ws.resolve("does/not/exist.txt");
        assert!(res.is_err(), "expected Err for missing path, got: {res:?}");
    }

    /// `Workspace::completions` returns workspace-relative entries for files
    /// under the root, and cwd-relative entries when the cwd-only file lives
    /// outside the workspace tree. Honors `.gitignore`.
    #[test]
    fn workspace_completions_walk_surfaces_workspace_and_cwd() {
        let tmp = TempDir::new().unwrap();
        // Two trees: a workspace under `ws/` and a cwd under `cwd/` that is
        // NOT inside the workspace, so the two walks are disjoint and we can
        // assert each branch contributed.
        let ws_root = tmp.path().join("ws");
        let cwd_root = tmp.path().join("cwd");
        std::fs::create_dir_all(&ws_root).unwrap();
        std::fs::create_dir_all(&cwd_root).unwrap();
        std::fs::write(ws_root.join("alpha.txt"), "a").unwrap();
        std::fs::write(cwd_root.join("alphabeta.txt"), "b").unwrap();

        let ws = Workspace::with_cwd(ws_root.clone(), Some(cwd_root.clone()));
        let entries = ws.completions("alpha", 16);
        assert!(
            entries.iter().any(|e| e == "alpha.txt"),
            "expected workspace entry alpha.txt; got: {entries:?}",
        );
        assert!(
            entries.iter().any(|e| e == "alphabeta.txt"),
            "expected cwd entry alphabeta.txt; got: {entries:?}",
        );
    }

    #[test]
    fn workspace_completions_honor_configured_walk_depth() {
        let tmp = TempDir::new().unwrap();
        // Sits at component depth 12, past the default walk depth (10) but
        // within the explicit deeper walk (16) below.
        let deep_dir = tmp.path().join("a/b/c/d/e/f/g/h/i/j/k");
        std::fs::create_dir_all(&deep_dir).unwrap();
        std::fs::write(deep_dir.join("target.txt"), "target").unwrap();

        let default_ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);
        let default_entries = default_ws.completions("target", 16);
        assert!(
            !default_entries
                .iter()
                .any(|entry| entry.ends_with("target.txt")),
            "default depth should keep very deep entries out of the hot completion path: {default_entries:?}",
        );

        let deep_ws = Workspace::with_cwd_and_depth(tmp.path().to_path_buf(), None, 16);
        let deep_entries = deep_ws.completions("target", 16);
        assert!(
            deep_entries
                .iter()
                .any(|entry| entry.ends_with("target.txt")),
            "configured deeper walk should surface the nested file: {deep_entries:?}",
        );

        let unlimited_ws = Workspace::with_cwd_and_depth(tmp.path().to_path_buf(), None, 0);
        let unlimited_entries = unlimited_ws.completions("target", 16);
        assert!(
            unlimited_entries
                .iter()
                .any(|entry| entry.ends_with("target.txt")),
            "depth 0 should disable the completion walk depth limit: {unlimited_entries:?}",
        );
    }

    #[test]
    fn browser_completions_show_only_immediate_children() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/nested")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "lib").unwrap();
        std::fs::write(tmp.path().join("src/nested/deep.rs"), "deep").unwrap();
        std::fs::write(tmp.path().join("README.md"), "readme").unwrap();

        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);

        let root_entries = ws.browser_completions("", 16);
        assert_eq!(root_entries, vec!["README.md", "src/"]);

        let src_entries = ws.browser_completions("src/", 16);
        assert_eq!(src_entries, vec!["src/lib.rs", "src/nested/"]);
        assert!(
            !src_entries.iter().any(|entry| entry.ends_with("deep.rs")),
            "browser mode must not walk past immediate children: {src_entries:?}",
        );
    }

    #[test]
    fn browser_completions_hide_dot_entries_until_dot_query() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".agents")).unwrap();
        std::fs::write(tmp.path().join(".env"), "secret-ish fixture").unwrap();
        std::fs::write(tmp.path().join("app.rs"), "app").unwrap();

        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);

        let default_entries = ws.browser_completions("", 16);
        assert_eq!(default_entries, vec!["app.rs"]);

        let dot_entries = ws.browser_completions(".", 16);
        assert_eq!(dot_entries, vec![".agents/", ".env"]);
    }

    #[test]
    fn browser_completions_reject_path_escape_segments() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        let sibling = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(workspace.join("inside.rs"), "inside").unwrap();
        std::fs::write(sibling.join("secret.rs"), "outside").unwrap();

        let ws = Workspace::with_cwd(workspace, None);

        assert_eq!(ws.browser_completions("", 16), vec!["inside.rs"]);
        assert!(
            ws.browser_completions("../", 16).is_empty(),
            "browser mode must not list workspace siblings",
        );
        assert!(
            ws.browser_completions("../outside", 16).is_empty(),
            "browser mode must not complete names from outside the workspace",
        );
    }

    #[test]
    fn workspace_completions_surface_explicit_hidden_and_ignored_paths() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), ".deepseek/\n.generated/\n").unwrap();
        std::fs::write(
            tmp.path().join(".deepseekignore"),
            ".generated/specs/secrets.env\n",
        )
        .unwrap();
        let deepseek_commands = tmp.path().join(".deepseek").join("commands");
        let generated_specs = tmp.path().join(".generated").join("specs");
        std::fs::create_dir_all(&deepseek_commands).unwrap();
        std::fs::create_dir_all(&generated_specs).unwrap();
        std::fs::write(deepseek_commands.join("start-task.md"), "start").unwrap();
        std::fs::write(generated_specs.join("device-layout.md"), "layout").unwrap();
        std::fs::write(generated_specs.join("secrets.env"), "secret").unwrap();

        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), Some(tmp.path().to_path_buf()));

        let start_entries = ws.completions(".deepseek/commands", 16);
        assert!(
            start_entries
                .iter()
                .any(|e| e == ".deepseek/commands/start-task.md"),
            "expected explicitly addressed hidden command file in completions: {start_entries:?}",
        );

        let generated_entries = ws.completions(".generated/specs", 16);
        assert!(
            generated_entries
                .iter()
                .any(|e| e == ".generated/specs/device-layout.md"),
            "expected explicitly addressed ignored user folder in completions: {generated_entries:?}",
        );
        assert!(
            !generated_entries
                .iter()
                .any(|e| e == ".generated/specs/secrets.env"),
            ".deepseekignore entries must not be reintroduced by local fallback: {generated_entries:?}",
        );
    }

    #[test]
    fn workspace_completions_skip_hidden_worktrees_and_build_bulk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), ".worktrees/\n.generated/\n").unwrap();

        std::fs::create_dir_all(root.join(".worktrees/release/src")).unwrap();
        std::fs::write(
            root.join(".worktrees/release/src/worktree-only.rs"),
            "fn main() {}",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".worktrees/release/target/debug")).unwrap();
        std::fs::write(
            root.join(".worktrees/release/target/debug/generated.o"),
            "object",
        )
        .unwrap();

        std::fs::create_dir_all(root.join(".claude/worktrees/agent/src")).unwrap();
        std::fs::write(
            root.join(".claude/worktrees/agent/src/agent-only.md"),
            "agent note",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(root.join(".claude/commands/keep.md"), "command").unwrap();

        std::fs::create_dir_all(root.join(".generated/specs")).unwrap();
        std::fs::write(root.join(".generated/specs/device-layout.md"), "layout").unwrap();

        let ws = Workspace::with_cwd(root.to_path_buf(), Some(root.to_path_buf()));

        let worktree_entries = ws.completions(".worktrees", 32);
        assert!(
            worktree_entries
                .iter()
                .all(|entry| !entry.starts_with(".worktrees/")),
            "hidden release worktrees must stay out of completions: {worktree_entries:?}",
        );

        let claude_worktree_entries = ws.completions(".claude/worktrees", 32);
        assert!(
            claude_worktree_entries
                .iter()
                .all(|entry| !entry.starts_with(".claude/worktrees/")),
            ".claude/worktrees must stay out of completions: {claude_worktree_entries:?}",
        );

        let generated_entries = ws.completions(".generated/specs", 32);
        assert!(
            generated_entries
                .iter()
                .any(|entry| entry == ".generated/specs/device-layout.md"),
            "explicit user-generated hidden folders should still complete: {generated_entries:?}",
        );

        let command_entries = ws.completions(".claude/commands", 32);
        assert!(
            command_entries
                .iter()
                .any(|entry| entry == ".claude/commands/keep.md"),
            "normal .claude command files should still complete: {command_entries:?}",
        );

        assert!(
            ws.resolve("worktree-only.rs").is_err(),
            "fuzzy resolution must not index files from hidden release worktrees"
        );
        assert!(
            ws.resolve("agent-only.md").is_err(),
            "fuzzy resolution must not index files from .claude/worktrees"
        );
        assert!(ws.resolve("keep.md").is_ok());
    }

    #[test]
    fn fuzzy_index_resolves_hidden_and_ignored_files_except_deepseekignored() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), ".generated/\n").unwrap();
        std::fs::write(
            tmp.path().join(".deepseekignore"),
            ".generated/specs/secrets.env\n",
        )
        .unwrap();
        let generated_specs = tmp.path().join(".generated").join("specs");
        std::fs::create_dir_all(&generated_specs).unwrap();
        std::fs::write(generated_specs.join("device-layout.md"), "layout").unwrap();
        std::fs::write(generated_specs.join("secrets.env"), "secret").unwrap();

        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);
        let resolved = ws.resolve("device-layout.md").unwrap();

        assert!(resolved.ends_with(".generated/specs/device-layout.md"));
        assert!(
            ws.resolve("secrets.env").is_err(),
            "basename fuzzy resolution must honor .deepseekignore"
        );
        assert!(
            ws.resolve(".generated/specs/secrets.env").is_ok(),
            "exact user-specified paths should still resolve"
        );
    }

    #[test]
    fn fuzzy_index_finds_files_and_directories() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b/target_dir")).unwrap();
        std::fs::write(tmp.path().join("a/b/needle.rs"), "fn main(){}").unwrap();

        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);

        // Basename-only mention triggers fuzzy fallback for both files and dirs.
        let f = ws.resolve("needle.rs").unwrap();
        assert!(f.ends_with("a/b/needle.rs"));
        let d = ws.resolve("target_dir").unwrap();
        assert!(d.ends_with("a/b/target_dir"));

        // Index was populated exactly once (subsequent lookups reuse it).
        assert!(ws.file_index.get().is_some());
    }

    /// Regression: `@`-mention completion must discover files inside
    /// `.deepseek/`, `.cursor/`, `.claude/`, `.agents/` even when
    /// those directories are excluded by `.gitignore` (or `.ignore`).
    /// The `discovery_walk_builder` override un-ignores them.
    #[test]
    fn completions_discovers_files_inside_gitignored_dot_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // `.ignore` works even outside a git repo; use it to simulate
        // a project that gitignores its AI-tool dot-directories.
        std::fs::write(
            root.join(".ignore"),
            ".deepseek/\n.cursor/\n.claude/\n.agents/\n",
        )
        .unwrap();

        // Create files inside each dot-dir.
        std::fs::create_dir_all(root.join(".deepseek/commands")).unwrap();
        std::fs::write(root.join(".deepseek/commands/build.md"), "build cmd").unwrap();
        std::fs::create_dir_all(root.join(".cursor/commands")).unwrap();
        std::fs::write(root.join(".cursor/commands/run.md"), "run cmd").unwrap();
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(root.join(".claude/commands/test.md"), "test cmd").unwrap();
        std::fs::create_dir_all(root.join(".agents/skills/example")).unwrap();
        std::fs::write(
            root.join(".agents/skills/example/SKILL.md"),
            "name: example\n",
        )
        .unwrap();

        let ws = Workspace::with_cwd(root.to_path_buf(), None);

        // Completions should find entries inside the dot-dirs.
        {
            let entries = ws.completions("build", 16);
            assert!(
                entries.iter().any(|e| e.contains("build.md")),
                "expected build.md in completions although .deepseek/ is ignored; got: {entries:?}"
            );
        }
        {
            let entries = ws.completions("run", 16);
            assert!(
                entries.iter().any(|e| e.contains("run.md")),
                "expected run.md from .cursor/; got: {entries:?}"
            );
        }
        {
            let entries = ws.completions("test", 16);
            assert!(
                entries.iter().any(|e| e.contains("test.md")),
                "expected test.md from .claude/; got: {entries:?}"
            );
        }

        // Fuzzy resolution should also work.
        let f = ws.resolve("build.md").unwrap();
        assert!(f.ends_with("build.md"));
        let f2 = ws.resolve("SKILL.md").unwrap();
        assert!(f2.ends_with("SKILL.md"));
    }

    /// Regression: the dot-dir walk must NOT index `.deepseek/snapshots/`,
    /// which is the snapshot side repo that can grow to hundreds of GB.
    /// Indexing it would re-create the same OOM/hang that #1112 was built
    /// to prevent.
    #[test]
    fn dot_dir_walk_excludes_snapshot_side_repo() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a snapshot-like directory tree.
        std::fs::create_dir_all(root.join(".deepseek/snapshots/deadbeef/deadbeef/.git/objects"))
            .unwrap();
        std::fs::write(
            root.join(".deepseek/snapshots/deadbeef/deadbeef/.git/objects/snapshot.pack"),
            b"fake pack data",
        )
        .unwrap();
        // Also create a legitimate file in .deepseek/ that should be found.
        std::fs::create_dir_all(root.join(".deepseek/commands")).unwrap();
        std::fs::write(root.join(".deepseek/commands/build.md"), "build cmd").unwrap();

        let ws = Workspace::with_cwd(root.to_path_buf(), None);

        // Searching for "build" must find build.md.
        let entries = ws.completions("build", 16);
        assert!(
            entries.iter().any(|e| e.contains("build.md")),
            "build.md must still be found; got: {entries:?}"
        );
        // Searching for "snapshot" must NOT return snapshot files.
        let snap_entries = ws.completions("snapshot", 16);
        assert!(
            !snap_entries.iter().any(|e| e.contains("snapshot")),
            "snapshot files must NOT appear in completions; got: {snap_entries:?}"
        );

        // Fuzzy index must also exclude snapshots.
        let f = ws.resolve("build.md").unwrap();
        assert!(f.ends_with("build.md"));
        // snapshot.pack should NOT resolve.
        let result = ws.resolve("snapshot.pack");
        assert!(
            result.is_err(),
            "snapshot.pack must not resolve via fuzzy index"
        );
    }

    /// Regression for #1921 — typing `@/` (or `@.`) must NOT trigger the
    /// `local_reference_paths` walk, which scans up to
    /// `LOCAL_REFERENCE_SCAN_LIMIT` paths on the UI thread. On WSL2 with a
    /// `/mnt/c/...` workspace this hangs the composer for seconds to minutes.
    #[test]
    fn should_try_local_reference_completion_skips_bare_separators_and_dots() {
        // The trigger gate must reject bare separators/dots.
        assert!(!should_try_local_reference_completion("/"));
        assert!(!should_try_local_reference_completion("\\"));
        assert!(!should_try_local_reference_completion("."));
        assert!(!should_try_local_reference_completion(".."));
        // Empty string was already rejected; keep that.
        assert!(!should_try_local_reference_completion(""));

        // Actionable references must still trigger.
        assert!(should_try_local_reference_completion("./foo"));
        assert!(should_try_local_reference_completion("../bar"));
        assert!(should_try_local_reference_completion(".env"));
        assert!(should_try_local_reference_completion("path/"));
        assert!(should_try_local_reference_completion("path/to/file"));
        assert!(should_try_local_reference_completion("/usr"));
    }

    #[test]
    fn cached_candidates_rank_like_live_completions() {
        // #3757: the composer caches one full candidate walk and ranks per
        // keystroke in memory; the ranked result must match what the live
        // walk would return for non-path-like needles.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/mention.rs"), "// m").unwrap();
        std::fs::write(root.join("README.md"), "# readme").unwrap();
        std::fs::write(root.join("Makefile"), "all:").unwrap();

        let ws = Workspace::with_cwd(root.to_path_buf(), None);
        let candidates = ws.completion_candidates();
        assert!(
            candidates.iter().any(|c| c == "src/main.rs"),
            "{candidates:?}"
        );

        for needle in ["ma", "readme", "men", ""] {
            let live = ws.completions(needle, 16);
            let ranked = rank_completion_candidates(&candidates, needle, 16);
            assert_eq!(ranked, live, "needle {needle:?}");
        }

        // Limit truncation applies after prefix/substring bucketing.
        let ranked = rank_completion_candidates(&candidates, "ma", 1);
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].to_lowercase().starts_with("ma"), "{ranked:?}");
    }

    #[test]
    fn background_completion_discovery_is_hard_capped_on_large_trees() {
        let tmp = TempDir::new().unwrap();
        for i in 0..256 {
            std::fs::write(tmp.path().join(format!("candidate_{i:03}.rs")), "x").unwrap();
        }
        let ws = Workspace::with_cwd(tmp.path().to_path_buf(), None);
        let never_cancelled = || false;

        let candidates = ws.completion_discovery_candidates(32, &never_cancelled);

        assert_eq!(
            candidates.len(),
            32,
            "the background cache must stop at its hard candidate limit"
        );
    }

    /// Regression for #1921 — `completions("/", N)` must return without
    /// invoking `local_reference_paths`, even on a workspace large enough
    /// to expose the original 4096-path walk. We can't assert "doesn't
    /// touch the disk", but we can assert the call completes promptly and
    /// stays within the requested limit.
    #[test]
    fn completions_for_bare_slash_does_not_trigger_local_reference_walk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Lay out enough files that a runaway walk would be visibly slow,
        // but the bounded path returns near-instantly. Depth-1 entries are
        // enough; we don't need to stress the filesystem.
        for i in 0..40 {
            std::fs::write(root.join(format!("file_{i}.txt")), "x").unwrap();
        }
        let ws = Workspace::with_cwd(root.to_path_buf(), None);

        let start = std::time::Instant::now();
        let entries = ws.completions("/", 64);
        let elapsed = start.elapsed();

        // Behavioral assertions:
        // 1. The call returns within a generous bound. Real freezes on
        //    WSL2 were tens of seconds; a 2s budget is comfortable for a
        //    40-file tmp dir on any CI host.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "completions(\"/\") took too long: {elapsed:?} (likely re-introduced #1921)"
        );
        // 2. Results stay within the requested cap.
        assert!(entries.len() <= 64);
    }
}

//! Workspace validation and first-class git worktree isolation for sub-agents.

use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::tools::spec::ToolError;
use codewhale_dependencies::{ExternalTool, Git};

use codewhale_agent_contract::role::FleetRole;

const SUBAGENT_WORKTREE_ROOT_DIR: &str = ".codewhale-worktrees";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubAgentWorktreeRequest {
    pub(super) branch: Option<String>,
    pub(super) path: Option<PathBuf>,
    pub(super) base_ref: Option<String>,
}

pub(super) fn prepare_child_workspace(
    parent_workspace: &Path,
    requested_cwd: Option<&Path>,
    worktree: Option<&SubAgentWorktreeRequest>,
    session_name: Option<&str>,
    agent_type: &FleetRole,
) -> Result<Option<PathBuf>, ToolError> {
    let discovery_anchor = if let Some(requested_cwd) = requested_cwd {
        validate_existing_child_cwd(parent_workspace, requested_cwd)?
    } else {
        parent_workspace
            .canonicalize()
            .unwrap_or_else(|_| parent_workspace.to_path_buf())
    };

    if let Some(worktree) = worktree {
        return create_isolated_worktree(&discovery_anchor, worktree, session_name, agent_type)
            .map(Some);
    }

    if requested_cwd.is_some() {
        return Ok(Some(discovery_anchor));
    }

    Ok(None)
}

fn validate_existing_child_cwd(
    parent_workspace: &Path,
    requested_cwd: &Path,
) -> Result<PathBuf, ToolError> {
    let resolved = if requested_cwd.is_absolute() {
        requested_cwd.to_path_buf()
    } else {
        parent_workspace.join(requested_cwd)
    };
    let workspace_canonical = parent_workspace
        .canonicalize()
        .unwrap_or_else(|_| parent_workspace.to_path_buf());
    let canonical = resolved.canonicalize().map_err(|e| {
        ToolError::invalid_input(format!(
            "Invalid cwd '{}': {e}. Allowed cwd: an existing directory under {} (relative paths resolve against it), or omit cwd to use it. The path may not exist yet — use worktree=true to let Codewhale create an isolated checkout.",
            requested_cwd.display(),
            workspace_canonical.display()
        ))
    })?;
    if !canonical.starts_with(&workspace_canonical) {
        return Err(ToolError::invalid_input(format!(
            "cwd must be inside the parent workspace: {} is not under {}. Allowed cwd: {} or a directory beneath it (relative paths resolve against it); omit cwd to run the child there.",
            canonical.display(),
            workspace_canonical.display(),
            workspace_canonical.display()
        )));
    }
    Ok(canonical)
}

pub(super) fn create_isolated_worktree(
    parent_workspace: &Path,
    request: &SubAgentWorktreeRequest,
    session_name: Option<&str>,
    agent_type: &FleetRole,
) -> Result<PathBuf, ToolError> {
    let repo_root = git_repo_root(parent_workspace)?;
    let branch = request
        .branch
        .clone()
        .unwrap_or_else(|| default_worktree_branch(session_name, agent_type));
    validate_git_branch_name(&repo_root, &branch)?;

    let base_ref = request
        .base_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("HEAD")
        .to_string();
    if base_ref.starts_with('-') {
        return Err(ToolError::invalid_input(format!(
            "Invalid worktree_base '{base_ref}': a ref cannot start with '-'"
        )));
    }
    let worktree_path = resolve_worktree_path(&repo_root, &branch, request.path.as_ref())?;
    // One worktree implementation: the Runtime lane's (#4176). It creates the
    // parent directory and captures git output instead of inheriting the TUI.
    codewhale_lane::provision_worktree(&codewhale_lane::WorktreeProvision {
        repo_root: repo_root.clone(),
        branch,
        path: worktree_path.clone(),
        base_ref: Some(base_ref),
    })
    .map_err(|err| {
        ToolError::execution_failed(format!("Failed to create sub-agent worktree: {err:#}"))
    })?;
    worktree_path.canonicalize().map_err(|err| {
        ToolError::execution_failed(format!(
            "Created worktree path '{}' could not be resolved: {err}",
            worktree_path.display()
        ))
    })
}

/// Remove a finished worker's isolated worktree (and its merged branch) when
/// the worker changed nothing in it. `changed` is the delivery-evidence
/// inventory: `None` means git could not answer, and nothing is removed.
/// Removal goes through the lane implementation, which only ever deletes a
/// path git itself lists as a linked worktree (#5824).
pub(super) fn remove_unchanged_worktree(
    worktree: &Path,
    changed: Option<&std::collections::BTreeSet<String>>,
) -> bool {
    if !changed.is_some_and(std::collections::BTreeSet::is_empty) || !worktree.exists() {
        return false;
    }
    // The delivery inventory sees tracked and untracked paths but not ignored
    // ones, and removal is forced. A fresh worktree holds no ignored files, so
    // any (a report, a build output) was written by the worker: keep it.
    let pristine = Git::output(
        &[
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
            "-z",
        ],
        worktree,
    )
    .is_ok_and(|output| output.status.success() && output.stdout.is_empty());
    if !pristine {
        return false;
    }
    if let Err(err) = codewhale_lane::remove_worktree_if_expired(worktree, Some(0), None) {
        tracing::debug!(
            "kept unchanged sub-agent worktree {}: {err:#}",
            worktree.display()
        );
    }
    !worktree.exists()
}

/// Remove the isolated worktree (and its new branch) of a spawn refused
/// before its child started. No agent ever ran in it, so everything it holds
/// came from the checkout itself, including what post-checkout hooks,
/// line-ending filters or LFS wrote there. Requiring a pristine
/// `git status`, as [`remove_unchanged_worktree`] does for a finished
/// worker, kept exactly those checkouts behind. The removal still goes
/// through the lane implementation, which deletes only a path git lists as a
/// linked worktree, and the branch only when merged.
pub(super) fn remove_unstarted_worktree(worktree: &Path) {
    let outcome = codewhale_lane::remove_worktree_if_expired(worktree, Some(0), None);
    if outcome.is_err() || worktree.exists() {
        tracing::warn!(
            "could not remove the worktree of a refused sub-agent spawn {}: {}",
            worktree.display(),
            outcome.err().map_or_else(
                || "git did not list it as a linked worktree".to_string(),
                |err| format!("{err:#}")
            )
        );
    }
}

pub(super) fn git_repo_root(workspace: &Path) -> Result<PathBuf, ToolError> {
    const MAX_PARENT_LEVELS: usize = 4;
    let start = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut paths_tried = Vec::new();
    let mut current = Some(start.as_path());
    let mut levels = 0usize;

    while let Some(dir) = current {
        paths_tried.push(dir.display().to_string());

        if let Some(root) = try_git_toplevel(dir) {
            return Ok(root);
        }

        if let Ok(entries) = fs::read_dir(dir) {
            let mut nested_roots = Vec::new();
            for entry in entries.flatten() {
                let child = entry.path();
                if !child.is_dir() || !path_looks_like_git_checkout(&child) {
                    continue;
                }
                if child
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.'))
                {
                    continue;
                }
                if let Some(root) = try_git_toplevel(&child) {
                    nested_roots.push(root);
                }
            }
            match nested_roots.len() {
                0 => {}
                1 => return Ok(nested_roots.into_iter().next().expect("single nested root")),
                _ => {
                    let repos = nested_roots
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(ToolError::invalid_input(format!(
                        "Multiple git repositories found under {}. Specify cwd to disambiguate: {repos}",
                        dir.display()
                    )));
                }
            }
        }

        levels += 1;
        if levels > MAX_PARENT_LEVELS {
            break;
        }
        current = dir.parent();
    }

    Err(ToolError::invalid_input(format!(
        "worktree=true requires a git repository. Tried: {}",
        paths_tried.join(", ")
    )))
}

fn path_looks_like_git_checkout(path: &Path) -> bool {
    let git_path = path.join(".git");
    git_path.is_dir() || git_path.is_file()
}

fn try_git_toplevel(path: &Path) -> Option<PathBuf> {
    let output = Git::output(&["rev-parse", "--show-toplevel"], path).ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn validate_git_branch_name(repo_root: &Path, branch: &str) -> Result<(), ToolError> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err(ToolError::invalid_input(
            "worktree_branch cannot be blank".to_string(),
        ));
    }
    run_git_checked(
        repo_root,
        &[
            "check-ref-format".to_string(),
            "--branch".to_string(),
            branch.to_string(),
        ],
        "validate sub-agent worktree branch",
    )
    .map(|_| ())
    .map_err(|err| ToolError::invalid_input(format!("Invalid worktree_branch '{branch}': {err}")))
}

/// Longest seed slug a default branch carries. The default checkout path is
/// the branch slug, which [`sanitize_worktree_slug`] caps at 48 characters;
/// `codex/agent-` (12) + seed + `-` + an 8-character unique suffix must fit,
/// or a long agent name truncated the suffix away and every retry of that
/// name (or any name sharing its first 27 characters) reused one path.
const DEFAULT_BRANCH_SEED_MAX: usize = 27;

fn default_worktree_branch(session_name: Option<&str>, agent_type: &FleetRole) -> String {
    let seed = session_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| agent_type.as_str());
    // The slug is ASCII, so truncating by bytes cannot split a character.
    let mut seed = sanitize_worktree_slug(seed);
    seed.truncate(DEFAULT_BRANCH_SEED_MAX);
    let seed = seed.trim_end_matches(['-', '.', '_']);
    format!(
        "codex/agent-{}-{}",
        if seed.is_empty() { "task" } else { seed },
        &Uuid::new_v4().to_string()[..8]
    )
}

fn resolve_worktree_path(
    repo_root: &Path,
    branch: &str,
    requested_path: Option<&PathBuf>,
) -> Result<PathBuf, ToolError> {
    let default_root = default_worktree_root(repo_root);
    let path = match requested_path {
        // Absolute and relative requests get the same containment: a
        // sub-agent checkout lives under the per-repo worktree root and
        // nowhere else.
        //
        // The request is rebased onto `default_root` as git reported it, so a
        // Windows caller spelling the same directory differently (`\\?\`
        // verbatim prefix from `canonicalize`, `/` separators, drive-letter
        // case) is compared, and checked out, in one form.
        Some(path) => {
            let requested = normalize_path_lexically(&default_root.join(path));
            match relative_to_root(&requested, &default_root)
                .map(|rest| normalize_path_lexically(&default_root.join(rest)))
                .filter(|resolved| worktree_path_within_root(resolved, &default_root))
            {
                Some(resolved) => resolved,
                None => {
                    return Err(ToolError::invalid_input(format!(
                        "worktree_path '{}' must stay under {}",
                        path.display(),
                        default_root.display()
                    )));
                }
            }
        }
        None => default_root.join(branch_worktree_slug(branch)),
    };
    let normalized = normalize_path_lexically(&path);
    let repo_canonical = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    if relative_to_root(&normalized, &repo_canonical).is_some() {
        return Err(ToolError::invalid_input(format!(
            "worktree_path must not be inside the parent checkout: {} is under {}",
            normalized.display(),
            repo_canonical.display()
        )));
    }
    Ok(normalized)
}

/// `candidate` (already lexically normalized) must sit under `root` both as
/// written and after resolving symlinks in its nearest existing ancestor, so
/// a symlink planted under the worktree root cannot redirect the checkout.
fn worktree_path_within_root(candidate: &Path, root: &Path) -> bool {
    candidate.starts_with(root)
        && canonicalize_existing_prefix(candidate).starts_with(canonicalize_existing_prefix(root))
}

/// The part of `candidate` below `root`, or `None` when it is not under it.
/// On Windows the two may spell one directory differently, so they are
/// compared in a simplified, case-insensitive form.
fn relative_to_root(candidate: &Path, root: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_relative_to_root(&candidate.to_string_lossy(), &root.to_string_lossy())
            .map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        candidate.strip_prefix(root).ok().map(Path::to_path_buf)
    }
}

/// String form of [`relative_to_root`] for Windows paths: `/` and `\` are one
/// separator, the `\\?\` and `\\?\UNC\` verbatim prefixes are dropped, and
/// ASCII case is ignored. The match must end on a component boundary, and a
/// remainder with a `..` component is refused.
#[cfg(any(windows, test))]
fn windows_relative_to_root(candidate: &str, root: &str) -> Option<String> {
    fn simplify(path: &str) -> String {
        let mut simple = path.replace('/', "\\");
        if let Some(rest) = simple.strip_prefix(r"\\?\UNC\") {
            simple = format!(r"\\{rest}");
        } else if let Some(rest) = simple.strip_prefix(r"\\?\") {
            simple = rest.to_string();
        }
        while simple.len() > 3 && simple.ends_with('\\') {
            simple.pop();
        }
        simple
    }
    let candidate = simplify(candidate);
    let root = simplify(root);
    if !candidate
        .get(..root.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&root))
    {
        return None;
    }
    let rest = &candidate[root.len()..];
    if !(rest.is_empty() || root.ends_with('\\') || rest.starts_with('\\')) {
        return None;
    }
    let rest = rest.trim_start_matches('\\');
    if rest.split('\\').any(|part| part == "..") {
        return None;
    }
    Some(rest.to_string())
}

/// Canonicalize the deepest existing ancestor of `path` and re-append the
/// components that do not exist yet.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(canonical) = cursor.canonicalize() {
            let mut resolved = canonical;
            for name in missing.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        match (cursor.file_name(), cursor.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                cursor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn default_worktree_root(repo_root: &Path) -> PathBuf {
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_worktree_slug)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let parent = repo_root.parent().unwrap_or(repo_root);
    normalize_path_lexically(&parent.join(SUBAGENT_WORKTREE_ROOT_DIR).join(repo_name))
}

/// The default checkout directory for `branch`: its slug, or, when the slug
/// had to be cut to [`WORKTREE_SLUG_MAX`], the cut slug with a short hash of
/// the whole branch name, so two long branches sharing a prefix get their own
/// checkouts instead of the second spawn failing on an existing path.
fn branch_worktree_slug(branch: &str) -> String {
    let slug = sanitize_worktree_slug(branch);
    if slug == sanitize_worktree_slug_within(branch, usize::MAX) {
        return slug;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(branch.as_bytes());
    let suffix: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // The slug is ASCII, so cutting by bytes cannot split a character.
    let mut prefix = slug;
    prefix.truncate(WORKTREE_SLUG_MAX - suffix.len() - 1);
    let prefix = prefix.trim_end_matches(['-', '.', '_']);
    format!("{prefix}-{suffix}")
}

/// Longest sub-agent worktree path slug.
const WORKTREE_SLUG_MAX: usize = 48;

fn sanitize_worktree_slug(input: &str) -> String {
    sanitize_worktree_slug_within(input, WORKTREE_SLUG_MAX)
}

fn sanitize_worktree_slug_within(input: &str, max: usize) -> String {
    let mut slug = String::new();
    for ch in input.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if matches!(ch, '-' | '_' | '.') {
            ch
        } else {
            '-'
        };
        if normalized == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(normalized);
        if slug.len() >= max {
            break;
        }
    }
    let slug = slug.trim_matches(['-', '.', '_']).to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn run_git_checked(workspace: &Path, args: &[String], action: &str) -> Result<String, ToolError> {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = Git::output(&arg_refs, workspace).map_err(|err| {
        ToolError::execution_failed(format!("Failed to {action}: could not run git: {err}"))
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git exited with status {}", output.status)
    };
    Err(ToolError::execution_failed(format!(
        "Failed to {action}: {detail}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long agent name used to push the unique suffix past the 48-character
    /// path slug, so a retry of the same name collided with the checkout the
    /// previous run kept.
    #[test]
    fn default_worktree_paths_stay_unique_for_long_agent_names() {
        let name = "audit-authentication-middleware-a";
        let first = default_worktree_branch(Some(name), &FleetRole::Worker);
        let second = default_worktree_branch(Some(name), &FleetRole::Worker);
        let sibling = default_worktree_branch(
            Some("audit-authentication-middleware-b"),
            &FleetRole::Worker,
        );
        assert!(
            first.starts_with("codex/agent-audit-authentication-midd"),
            "{first}"
        );
        let paths = [&first, &second, &sibling].map(|branch| branch_worktree_slug(branch));
        for (branch, path) in [&first, &second, &sibling].into_iter().zip(&paths) {
            assert_eq!(
                path.as_str(),
                branch.replace('/', "-"),
                "the default path slug keeps the whole branch, suffix included"
            );
        }
        assert_ne!(paths[0], paths[1], "a retry must get its own checkout");
        assert_ne!(
            paths[0], paths[2],
            "names sharing a prefix must not collide"
        );
    }

    /// An explicit branch with no worktree_path gets its path from the branch
    /// slug, cut at 48 characters. Two long branches sharing that prefix used
    /// to map to one checkout, and the second spawn failed on it.
    #[test]
    fn long_explicit_branches_sharing_a_prefix_get_distinct_paths() {
        let a = "feature/very-long-shared-prefix-authentication-work-a";
        let b = "feature/very-long-shared-prefix-authentication-work-b";
        let (path_a, path_b) = (branch_worktree_slug(a), branch_worktree_slug(b));
        assert_ne!(path_a, path_b);
        for path in [&path_a, &path_b] {
            assert!(path.len() <= WORKTREE_SLUG_MAX, "{path}");
            assert!(
                path.starts_with("feature-very-long-shared-prefix"),
                "{path}"
            );
        }
        assert_eq!(path_a, branch_worktree_slug(a), "the path is stable");
        assert_eq!(
            branch_worktree_slug("feature/short"),
            "feature-short",
            "a branch that fits keeps its plain slug"
        );
    }

    #[test]
    fn cwd_errors_name_the_allowed_root() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let root = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");

        let err = validate_existing_child_cwd(workspace.path(), outside.path())
            .expect_err("outside cwd refused")
            .to_string();
        assert!(err.contains("Allowed cwd"), "{err}");
        assert!(err.contains(&root.display().to_string()), "{err}");

        let err = validate_existing_child_cwd(workspace.path(), Path::new("missing/dir"))
            .expect_err("missing cwd refused")
            .to_string();
        assert!(err.contains("Allowed cwd"), "{err}");
        assert!(err.contains(&root.display().to_string()), "{err}");

        std::fs::create_dir(workspace.path().join("sub")).expect("sub");
        assert_eq!(
            validate_existing_child_cwd(workspace.path(), Path::new("sub")).expect("inside"),
            root.join("sub")
        );
    }

    #[test]
    fn windows_paths_under_the_root_match_across_spellings() {
        let root = r"C:\Users\runner\.codewhale-worktrees\repo";
        for candidate in [
            r"\\?\C:\Users\runner\.codewhale-worktrees\repo\inside",
            r"c:/users/RUNNER/.codewhale-worktrees/repo/inside",
            r"C:\Users\runner\.codewhale-worktrees\repo\inside\",
        ] {
            assert_eq!(
                windows_relative_to_root(candidate, root).as_deref(),
                Some("inside"),
                "{candidate}"
            );
        }
        assert_eq!(
            windows_relative_to_root(r"\\?\C:\Users\runner\.codewhale-worktrees\repo", root)
                .as_deref(),
            Some("")
        );
        assert_eq!(
            windows_relative_to_root(
                r"\\?\UNC\server\share\wt\repo\a\b",
                r"\\server/share/wt/repo/"
            )
            .as_deref(),
            Some(r"a\b")
        );
        assert_eq!(
            windows_relative_to_root(r"C:\", r"c:\").as_deref(),
            Some("")
        );
    }

    #[test]
    fn windows_paths_outside_the_root_are_refused() {
        let root = r"C:\Users\runner\.codewhale-worktrees\repo";
        for candidate in [
            r"C:\Users\runner\.codewhale-worktrees\repo-evil\x",
            r"D:\Users\runner\.codewhale-worktrees\repo\x",
            r"\\?\C:\Users\runner\.codewhale-worktrees",
            r"C:\Users\runner\.codewhale-worktrees\repo\..\escaped",
            r"C:\Temp\escaped",
        ] {
            assert_eq!(
                windows_relative_to_root(candidate, root),
                None,
                "{candidate}"
            );
        }
    }
}

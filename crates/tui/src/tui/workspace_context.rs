//! Per-workspace git context shown in the composer header.
//!
//! The TUI shows a "branch | clean/N modified/…" badge. It is derived from
//! the one cached git probe ([`codewhale_engine::git_status`]) rather than a git
//! query of its own, so the badge, the Git view and the chrome never run two
//! `git status` processes for the same tick (#6565). The badge is re-read
//! every `REFRESH_SECS` seconds; the read takes the probe's cached snapshot
//! and only probes when that snapshot is stale.

#[cfg(test)]
use codewhale_dependencies::{ExternalTool, Git};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::tui::app::App;

/// How often (seconds) the workspace context badge is allowed to
/// re-query git. Exposed for tests that exercise the TTL.
pub(crate) const REFRESH_SECS: u64 = 15;

/// One completed background refresh, including an unavailable Git result.
#[derive(Debug)]
pub(crate) struct WorkspaceContextSnapshot {
    pub workspace: std::path::PathBuf,
    pub context: Option<String>,
    pub is_linked_worktree: bool,
    /// The workspace notes (`/note`), read off the render path with the git
    /// context on the same TTL, for the dock's NOTES view.
    pub notes: Vec<String>,
}

/// Read the badge from the shared git probe: its cached snapshot while that
/// is fresh, or a new probe. `force` re-probes regardless, for an explicit
/// refresh after something that may have changed the tree.
fn collect_snapshot(workspace: &Path, force: bool) -> WorkspaceContextSnapshot {
    let snap = if force {
        codewhale_engine::git_status::force_refresh(workspace)
    } else {
        codewhale_engine::git_status::refresh_if_stale(workspace)
    };
    let current = snap.probed_workspace.as_deref() == Some(workspace);
    WorkspaceContextSnapshot {
        workspace: workspace.to_path_buf(),
        context: current
            .then(|| codewhale_engine::git_status::context_line(&snap))
            .flatten(),
        is_linked_worktree: current && snap.is_linked_worktree,
        notes: crate::commands::read_notes(workspace, &crate::commands::notes_path(workspace))
            .unwrap_or_default(),
    }
}

fn apply_snapshot(app: &mut App, snapshot: WorkspaceContextSnapshot) {
    if snapshot.workspace != app.workspace {
        return;
    }
    if app.workspace_context != snapshot.context
        || app.workspace_is_linked_worktree != snapshot.is_linked_worktree
        || app.workspace_notes != snapshot.notes
    {
        app.needs_redraw = true;
    }
    app.workspace_context = snapshot.context;
    app.workspace_is_linked_worktree = snapshot.is_linked_worktree;
    app.workspace_notes = snapshot.notes;
}

/// Pull a fresh workspace context from disk if the cached value is
/// older than [`REFRESH_SECS`] and `allow_refresh` is true. Always
/// drains any pending async result into `app.workspace_context` first
/// so the render pass sees the latest value (#399 S1).
pub(super) fn refresh_if_needed(app: &mut App, now: Instant, allow_refresh: bool) {
    refresh_inner(app, now, allow_refresh, false);
}

fn refresh_inner(app: &mut App, now: Instant, allow_refresh: bool, force: bool) {
    // Completion is distinct from a missing result: losing a repository must
    // clear a stale branch, and an old workspace's refresh must not replace it.
    let completed = app
        .workspace_context_cell
        .lock()
        .ok()
        .and_then(|mut cell| cell.take());
    if let Some(snapshot) = completed {
        apply_snapshot(app, snapshot);
    }

    if app
        .workspace_context_refreshed_at
        .is_some_and(|refreshed_at| {
            now.duration_since(refreshed_at) < Duration::from_secs(REFRESH_SECS)
        })
    {
        return;
    }

    if !allow_refresh {
        return;
    }

    // The Session sidebar shows the memory file's size every frame it is
    // visible. Stat it here, on the same TTL as the git context, so the draw
    // closure reads a cached string instead of issuing a syscall per frame
    // (#3908). Cheap on a local disk; tens of ms on NFS/SSHFS/cloud-synced
    // home directories, which is exactly where the stutter was reported.
    refresh_memory_size_hint(app);

    // Offload git query to a background thread when a Tokio runtime is
    // available. Fall back to synchronous execution for tests and other
    // non-async contexts (#399 S1).
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let ctx = app.workspace_context_cell.clone();
        let workspace = app.workspace.clone();
        handle.spawn_blocking(move || {
            let result = collect_snapshot(&workspace, force);
            if let Ok(mut guard) = ctx.lock() {
                *guard = Some(result);
            }
        });
    } else {
        // No runtime — run synchronously so tests and one-shot callers
        // still get a result immediately.
        let snapshot = collect_snapshot(&app.workspace, force);
        apply_snapshot(app, snapshot);
    }
    app.workspace_context_refreshed_at = Some(now);
}

/// Re-read the memory file's size into [`App::memory_size_hint`].
///
/// A missing or unreadable file renders as an em dash, matching what the
/// sidebar showed when it stat-ed inline.
fn refresh_memory_size_hint(app: &mut App) {
    let hint = if app.use_memory {
        Some(
            std::fs::metadata(&app.memory_path)
                .map(|meta| format_size(meta.len()))
                .unwrap_or_else(|_| "\u{2014}".to_string()),
        )
    } else {
        None
    };
    if app.memory_size_hint != hint {
        app.needs_redraw = true;
        app.memory_size_hint = hint;
    }
}

/// Human-readable byte size, in the exact shape the sidebar rendered inline.
fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Force a workspace-context re-query on the next render tick, bypassing the
/// normal TTL. Keeps the current value visible while the background git query
/// is running.
pub(super) fn refresh_now(app: &mut App, now: Instant) {
    if let Ok(mut cell) = app.workspace_context_cell.lock() {
        *cell = None;
    }
    app.workspace_context_refreshed_at = None;
    refresh_inner(app, now, true, true);
}

pub(crate) fn branch_from_context(context: &str) -> Option<&str> {
    let (branch, _) = context.rsplit_once(" | ")?;
    (!branch.is_empty()).then_some(branch)
}

/// Concise, factual workspace identity for the footer status chip (#3188).
///
/// The identity is sourced from workspace/git detection only — never from
/// model narration or config text. `name` is the workspace basename, `branch`
/// is `Some` only when the workspace is a git repository (carrying the cached
/// `"detached:<hash>"` form for detached HEAD), and `is_git` distinguishes a
/// real repo from a plain directory so the footer can show an explicit
/// non-repo state instead of an empty `Repo:` label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceIdentity {
    pub name: String,
    pub branch: Option<String>,
    pub is_git: bool,
}

/// Basename used as the workspace identity. Falls back to a stable sentinel
/// when the path has no final component (filesystem root). Derived purely
/// from the workspace path, so it never spawns git on the render path.
pub(crate) fn workspace_basename(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(root)")
        .to_string()
}

/// Resolve the footer identity from the workspace path plus the cached
/// "branch | status" context string. `context` is `None` when the workspace
/// is not a git repository (or git is unavailable), which we surface as an
/// explicit non-repo state rather than hiding the chip.
pub(crate) fn identity_from_context(workspace: &Path, context: Option<&str>) -> WorkspaceIdentity {
    let branch = context.and_then(branch_from_context).map(str::to_string);
    WorkspaceIdentity {
        name: workspace_basename(workspace),
        is_git: branch.is_some(),
        branch,
    }
}

/// Hard display-column cap for the opt-in `workspace` / `git_branch`
/// metrics-line chips (#6112): the only status items whose value is
/// arbitrary-length text, so they are the ones that could reflow the row.
/// The full path stays in `/status` and the empty-state caption.
pub(crate) const STATUS_CHIP_MAX_WIDTH: usize = 24;

/// Left-truncate `text` to `max_width` display columns, keeping the tail —
/// the discriminating part of a directory name or branch — and marking the
/// cut with a leading `…`. Unicode-safe: widths come from `unicode_width`
/// and the cut never splits a `char`.
pub(crate) fn truncate_left(text: &str, max_width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    if max_width == 0 {
        return String::new();
    }
    let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if text.width() <= max_width {
        return text;
    }
    let mut width = 1; // ellipsis
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let next = width + grapheme.width();
        if next > max_width {
            break;
        }
        width = next;
        start = index;
    }
    format!("…{}", &text[start..])
}

/// Linked worktrees often repeat a repository leaf name. Include their parent
/// directory as a disambiguator, without reading the filesystem during draw.
pub(crate) fn status_workspace_name(workspace: &Path, is_linked_worktree: bool) -> String {
    let leaf = workspace_basename(workspace);
    if is_linked_worktree && let Some(parent) = workspace.parent().and_then(Path::file_name) {
        return format!("{}/{leaf}", parent.to_string_lossy());
    }
    leaf
}

#[cfg(test)]
fn run_git(workspace: &Path, args: &[&str]) -> std::io::Result<String> {
    let output = Git::output(args, workspace)?;
    if !output.status.success() {
        return Err(std::io::Error::other("git command failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_size_hint_is_cached_off_the_render_path() {
        // #3908: the Session sidebar rendered this by stat-ing the memory file
        // inside the draw closure, once per frame. The stat now happens here,
        // on the workspace-context TTL, so the sidebar reads a plain String.
        let dir = tempfile::tempdir().expect("temp dir");
        let memory = dir.path().join("MEMORY.md");
        std::fs::write(&memory, vec![b'x'; 2048]).unwrap();

        let mut app = crate::tui::app::App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &codewhale_runtime::config::Config::default(),
        );
        app.use_memory = true;
        app.memory_path = memory.clone();

        refresh_memory_size_hint(&mut app);
        assert_eq!(app.memory_size_hint.as_deref(), Some("2.0 KB"));

        // A file that is not there reads the same as one we cannot stat: the
        // sidebar's original em dash, not a crash or a stale number.
        std::fs::remove_file(&memory).unwrap();
        refresh_memory_size_hint(&mut app);
        assert_eq!(app.memory_size_hint.as_deref(), Some("\u{2014}"));

        // Memory off means nothing to show at all.
        app.use_memory = false;
        refresh_memory_size_hint(&mut app);
        assert_eq!(app.memory_size_hint, None);
    }

    #[test]
    fn memory_size_formats_match_the_sidebar_original() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn workspace_basename_handles_root_path() {
        assert_eq!(workspace_basename(Path::new("/")), "(root)");
        assert_eq!(workspace_basename(Path::new("/a/b/project")), "project");
    }

    #[test]
    fn truncate_left_keeps_the_tail_within_budget() {
        // Short values pass through untouched.
        assert_eq!(truncate_left("codewhale", 24), "codewhale");
        // Exactly at the cap is not a truncation.
        assert_eq!(truncate_left("abcdefghij", 10), "abcdefghij");
        // Long values keep the tail behind a one-column ellipsis.
        let cut = truncate_left("very-long-workspace-name", 10);
        assert_eq!(cut, "\u{2026}pace-name");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(cut.as_str()),
            10,
            "{cut}"
        );
        // Wide chars count by display columns and are never split.
        let cut = truncate_left("workspace-作業ディレクトリ", 10);
        assert!(cut.starts_with('\u{2026}'), "{cut}");
        assert!(
            unicode_width::UnicodeWidthStr::width(cut.as_str()) <= 10,
            "{cut}"
        );
    }
    #[test]
    fn workspace_chip_respects_zero_width_graphemes_and_terminal_controls() {
        use unicode_width::UnicodeWidthStr;
        for text in ["e\u{301}-family-👨‍👩‍👧‍👦", "作業-directory", "\x1b[31mname\n"]
        {
            for budget in 0..25 {
                let result = truncate_left(text, budget);
                assert!(result.width() <= budget, "{result:?} exceeds {budget}");
                assert!(!result.chars().any(char::is_control));
            }
        }
        assert_eq!(truncate_left("prefix-👨‍👩‍👧‍👦", 3), "…👨‍👩‍👧‍👦");
        assert_eq!(
            status_workspace_name(Path::new("/trees/feature/codewhale"), true),
            "feature/codewhale"
        );
        assert_eq!(
            status_workspace_name(Path::new("/trees/feature/codewhale"), false),
            "codewhale"
        );
    }

    #[test]
    fn workspace_snapshot_detects_linked_worktrees_and_detached_heads() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let linked = root.path().join("feature");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "--initial-branch=main"]).unwrap();
        run_git(
            &main,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        run_git(
            &main,
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
        )
        .unwrap();
        let ordinary = collect_snapshot(&main, false);
        assert!(!ordinary.is_linked_worktree);
        let linked_snapshot = collect_snapshot(&linked, false);
        assert!(linked_snapshot.is_linked_worktree);
        assert_eq!(
            linked_snapshot
                .context
                .as_deref()
                .and_then(branch_from_context),
            Some("feature")
        );
        run_git(&linked, &["checkout", "--detach"]).unwrap();
        // The badge reads the shared probe's cache; a checkout forces it.
        codewhale_engine::git_status::force_refresh(&linked);
        let detached = collect_snapshot(&linked, false);
        assert!(detached.is_linked_worktree);
        assert!(
            detached
                .context
                .as_deref()
                .and_then(branch_from_context)
                .unwrap()
                .starts_with("detached:")
        );
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let missing = collect_snapshot(&outside, false);
        assert!(missing.context.is_none());
        assert!(!missing.is_linked_worktree);
    }
}

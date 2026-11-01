//! File-tree pane — Ctrl+Shift+E toggles a left-side workspace file navigator.
//!
//! Shows the workspace directory tree with expandable directories. Up/Down
//! navigate, Enter expands/collapses directories or inserts `@path` for files,
//! Esc closes the pane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;
use codewhale_tui_kit::ui_text::truncate_line_to_width;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A single entry in the file tree.
#[derive(Debug, Clone)]
pub struct FileTreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

/// An in-flight background expand walk (#3900). The sequence number
/// distinguishes the latest walk for a directory from superseded ones so a
/// stale result can never be spliced in after a re-toggle.
#[derive(Debug, Clone)]
struct PendingExpand {
    seq: u64,
    cell: Arc<Mutex<Option<Vec<FileTreeEntry>>>>,
}

/// Mutable state for the file-tree pane.
#[derive(Debug, Clone)]
pub struct FileTreeState {
    /// Flat list of visible entries (respects expanded/collapsed state).
    pub entries: Vec<FileTreeEntry>,
    /// Index into `entries` for the cursor.
    pub cursor: usize,
    /// Scroll offset into `entries`.
    pub scroll_offset: usize,
    /// Set of expanded directory paths (normalised).
    pub expanded_dirs: HashSet<PathBuf>,
    /// Workspace root.
    pub workspace: PathBuf,
    /// Whether the tree is still building (async initial walk in progress).
    pub is_loading: bool,
    /// Shared cell for async tree-building results (#399 S3).
    loading_cell: Option<Arc<Mutex<Option<Vec<FileTreeEntry>>>>>,
    /// In-flight expand walks keyed by normalised directory path (#3900).
    pending_expands: HashMap<PathBuf, PendingExpand>,
    /// Monotonic counter identifying the latest expand walk per directory.
    expand_seq: u64,
}

impl FileTreeState {
    /// Build a fresh tree state by walking `workspace`.
    /// Spawns the initial walk on a background thread (#399 S3); without a
    /// tokio runtime (plain unit tests) the walk runs synchronously.
    pub fn new(workspace: &Path) -> Self {
        let expanded_dirs = HashSet::new();
        if tokio::runtime::Handle::try_current().is_err() {
            let entries = build_file_tree_inner(workspace, &expanded_dirs, None);
            return Self {
                entries,
                cursor: 0,
                scroll_offset: 0,
                expanded_dirs,
                workspace: workspace.to_path_buf(),
                is_loading: false,
                loading_cell: None,
                pending_expands: HashMap::new(),
                expand_seq: 0,
            };
        }
        let loading_cell = Arc::new(Mutex::new(None));
        let cell = loading_cell.clone();
        let ws = workspace.to_path_buf();
        codewhale_util::utils::spawn_blocking_supervised("file-tree-build", move || {
            let entries = build_file_tree_inner(&ws, &HashSet::new(), None);
            if let Ok(mut guard) = cell.lock() {
                *guard = Some(entries);
            }
        });
        Self {
            entries: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            expanded_dirs,
            workspace: workspace.to_path_buf(),
            is_loading: true,
            loading_cell: Some(loading_cell),
            pending_expands: HashMap::new(),
            expand_seq: 0,
        }
    }

    /// Poll for async build results. Call from the render loop.
    pub fn poll_loading(&mut self) {
        if !self.is_loading {
            return;
        }
        // Take the Arc out temporarily to avoid a double-borrow of self.
        let cell = match self.loading_cell.take() {
            Some(c) => c,
            None => return,
        };
        let mut done = false;
        if let Ok(mut guard) = cell.lock()
            && let Some(entries) = guard.take()
        {
            self.entries = entries;
            self.is_loading = false;
            self.clamp_cursor();
            done = true;
        }
        if !done {
            // Put the cell back so we can poll again next frame.
            self.loading_cell = Some(cell);
        }
    }

    /// Drain any completed background walks (initial build or expands).
    /// Returns `true` when new results were applied so the event loop can
    /// schedule a repaint — without this, a walk finishing while the loop
    /// is idle would leave the expanded directory looking empty until the
    /// next unrelated input event (#3900).
    pub fn poll_background(&mut self) -> bool {
        let was_loading = self.is_loading;
        self.poll_loading();
        let finished_loading = was_loading && !self.is_loading;
        let pending_before = self.pending_expands.len();
        self.poll_pending_expands();
        finished_loading || self.pending_expands.len() != pending_before
    }

    /// Poll for background expand-walk results and splice them in.
    /// Call from the render loop, after [`Self::poll_loading`] (#3900).
    pub fn poll_pending_expands(&mut self) {
        if self.pending_expands.is_empty() {
            return;
        }
        let mut ready: Vec<(PathBuf, u64, Vec<FileTreeEntry>)> = Vec::new();
        for (dir, pending) in &self.pending_expands {
            if let Ok(mut guard) = pending.cell.lock()
                && let Some(children) = guard.take()
            {
                ready.push((dir.clone(), pending.seq, children));
            }
        }
        for (dir, seq, children) in ready {
            self.apply_expand_result(&dir, seq, children);
        }
    }

    /// Move the cursor up by one.
    pub fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.clamp_scroll();
    }

    /// Move the cursor down by one.
    pub fn cursor_down(&mut self) {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
        self.clamp_scroll();
    }

    /// Activate the entry under the cursor.
    ///
    /// Returns `Some(path)` when the entry is a file that should be
    /// mentioned (`@path` inserted into the composer). Returns `None`
    /// after toggling a directory expand/collapse.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.cursor)?;
        if entry.is_dir {
            let norm = normalize_path(&entry.path);
            if self.expanded_dirs.contains(&norm) {
                self.collapse_dir_at(self.cursor);
            } else {
                self.expand_dir_at(self.cursor);
            }
            None
        } else {
            // Return the path relative to workspace.
            entry.path.strip_prefix(&self.workspace).ok().map(|rel| {
                let mut p = PathBuf::new();
                for comp in rel.components() {
                    p.push(comp);
                }
                p
            })
        }
    }

    /// Collapse the directory at `idx` by splicing its visible descendants
    /// out of the flat entry list — no filesystem I/O at all (#3900).
    ///
    /// Descendant directories stay in `expanded_dirs` so re-expanding the
    /// parent restores their expanded state, matching the previous
    /// full-rebuild behavior.
    fn collapse_dir_at(&mut self, idx: usize) {
        let Some(entry) = self.entries.get_mut(idx) else {
            return;
        };
        if !entry.is_dir {
            return;
        }
        let depth = entry.depth;
        entry.expanded = false;
        let norm = normalize_path(&entry.path);
        self.expanded_dirs.remove(&norm);
        // Drop any in-flight expand walk for this directory; its result must
        // not splice into a collapsed node.
        self.pending_expands.remove(&norm);

        let end = self.entries[idx + 1..]
            .iter()
            .position(|e| e.depth <= depth)
            .map_or(self.entries.len(), |offset| idx + 1 + offset);
        let removed = end - (idx + 1);
        self.entries.drain(idx + 1..end);
        if self.cursor > idx {
            self.cursor = if self.cursor < end {
                idx
            } else {
                self.cursor - removed
            };
        }
        self.clamp_cursor();
        self.clamp_scroll();
    }

    /// Expand the directory at `idx`. The subtree walk runs on a background
    /// thread and is spliced in by [`Self::poll_pending_expands`] (#3900);
    /// without a tokio runtime (plain unit tests) it runs synchronously.
    ///
    /// The entry is marked expanded immediately (▼) so the keypress is
    /// acknowledged; children appear when the walk completes.
    fn expand_dir_at(&mut self, idx: usize) {
        let Some(entry) = self.entries.get_mut(idx) else {
            return;
        };
        if !entry.is_dir {
            return;
        }
        entry.expanded = true;
        let dir = entry.path.clone();
        let norm = normalize_path(&dir);
        self.expanded_dirs.insert(norm.clone());
        self.expand_seq = self.expand_seq.wrapping_add(1);
        let seq = self.expand_seq;
        let ws = self.workspace.clone();
        let expanded_snapshot = self.expanded_dirs.clone();

        let cell = Arc::new(Mutex::new(None));
        self.pending_expands.insert(
            norm.clone(),
            PendingExpand {
                seq,
                cell: cell.clone(),
            },
        );
        if tokio::runtime::Handle::try_current().is_ok() {
            codewhale_util::utils::spawn_blocking_supervised("file-tree-expand", move || {
                let children = build_file_tree_inner(&ws, &expanded_snapshot, Some(&dir));
                if let Ok(mut guard) = cell.lock() {
                    *guard = Some(children);
                }
            });
        } else {
            let children = build_file_tree_inner(&ws, &expanded_snapshot, Some(&dir));
            self.apply_expand_result(&norm, seq, children);
        }
    }

    /// Splice a completed expand walk into the entry list, unless it has
    /// been superseded (newer walk for the same directory), the directory
    /// was collapsed while the walk was in flight, or the directory is no
    /// longer visible (an ancestor collapsed).
    fn apply_expand_result(&mut self, dir: &Path, seq: u64, children: Vec<FileTreeEntry>) {
        let is_current = self
            .pending_expands
            .get(dir)
            .is_some_and(|pending| pending.seq == seq);
        if !is_current {
            return;
        }
        self.pending_expands.remove(dir);
        if !self.expanded_dirs.contains(dir) {
            return;
        }
        let Some(idx) = self
            .entries
            .iter()
            .position(|e| e.is_dir && normalize_path(&e.path) == *dir)
        else {
            return;
        };
        let depth = self.entries[idx].depth;
        // Defensive: never splice a subtree in twice.
        if self.entries.get(idx + 1).is_some_and(|e| e.depth > depth) {
            return;
        }
        self.entries[idx].expanded = true;
        let inserted = children.len();
        self.entries.splice(idx + 1..idx + 1, children);
        if self.cursor > idx {
            self.cursor += inserted;
        }
        self.clamp_cursor();
        self.clamp_scroll();
    }

    /// Ensure the cursor is within bounds.
    fn clamp_cursor(&mut self) {
        if !self.entries.is_empty() && self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }

    /// Ensure the scroll offset keeps the cursor visible.
    fn clamp_scroll(&mut self) {
        let visible_height = 20usize; // will be overridden per render
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        }
        if self.scroll_offset + visible_height <= self.cursor {
            self.scroll_offset = self.cursor.saturating_add(1).saturating_sub(visible_height);
        }
    }

    /// Adjust scroll for a given visible height.
    #[cfg(test)]
    pub fn adjust_scroll(&mut self, visible: usize) {
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        }
        if visible > 0 && self.cursor >= self.scroll_offset + visible {
            self.scroll_offset = self.cursor.saturating_add(1).saturating_sub(visible);
        }
    }
}

// ---------------------------------------------------------------------------
// Tree building
// ---------------------------------------------------------------------------

/// Build the flat visible-entry list.
///
/// Walks the workspace directory recursively. Directories in `expanded_dirs`
/// have their children included; collapsed directories show only the directory
/// entry itself. Entries are sorted: directories first, then files, each group
/// alphabetically.
fn build_file_tree_inner(
    workspace: &Path,
    expanded_dirs: &HashSet<PathBuf>,
    single_root: Option<&Path>,
) -> Vec<FileTreeEntry> {
    let mut entries: Vec<FileTreeEntry> = Vec::new();

    // Determine which root to scan.
    let scan_root = single_root.unwrap_or(workspace);

    // Collect children of `scan_root`.
    let mut children: Vec<(String, PathBuf, bool)> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(scan_root) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            // Skip well-known ignored directories.
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && matches!(name, ".git" | "node_modules" | "target" | ".DS_Store")
            {
                continue;
            }
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let is_dir = ft.is_dir();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_string())
                .unwrap_or_default();
            children.push((name, path, is_dir));
        }
    }

    // Sort: dirs first, then files, alphabetical within each group.
    // Decorate-sort-undecorate: precompute lowercase names to avoid
    // allocating on every comparison.
    let mut decorated: Vec<_> = children
        .into_iter()
        .map(|(name, path, is_dir)| {
            let lower = name.to_lowercase();
            (lower, name, path, is_dir)
        })
        .collect();
    decorated.sort_by(
        |(a_lower, _, _, a_dir), (b_lower, _, _, b_dir)| match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a_lower.cmp(b_lower),
        },
    );
    children = decorated
        .into_iter()
        .map(|(_, name, path, is_dir)| (name, path, is_dir))
        .collect();

    // Compute depth for the current level.
    let depth = if single_root.is_some() {
        let rel = scan_root.strip_prefix(workspace).unwrap_or(scan_root);
        rel.components().count()
    } else {
        0
    };

    for (name, path, is_dir) in &children {
        let norm = normalize_path(path);
        let is_expanded = *is_dir && expanded_dirs.contains(&norm);

        entries.push(FileTreeEntry {
            name: name.clone(),
            path: path.clone(),
            is_dir: *is_dir,
            depth,
            expanded: is_expanded,
        });

        // If it's an expanded directory, recurse.
        if is_expanded {
            let sub = build_file_tree_inner(workspace, expanded_dirs, Some(path));
            entries.extend(sub);
        }
    }

    entries
}

/// Normalise a path for use as a HashSet key.
fn normalize_path(path: &Path) -> PathBuf {
    let components: Vec<_> = path.components().collect();
    // Try to strip workspace prefix.
    PathBuf::from_iter(components.iter().map(|c| c.as_os_str()))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const FILE_TREE_MIN_WIDTH: u16 = 20;

/// Render the file tree inside `area`.
/// Polls async loading state before rendering (#399 S3).
pub fn render_file_tree(
    f: &mut Frame,
    area: Rect,
    state: &mut FileTreeState,
    mode: palette::PaletteMode,
) {
    state.poll_loading();
    state.poll_pending_expands();
    if area.width < FILE_TREE_MIN_WIDTH || area.height < 3 {
        return;
    }

    let content_width = area.width.saturating_sub(4) as usize;
    let visible_rows = area.height.saturating_sub(3) as usize;

    let scroll = state.scroll_offset;
    let max_visible = visible_rows.max(1);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(max_visible + 1);

    if state.is_loading {
        lines.push(Line::from(Span::styled(
            "  Building file tree...",
            Style::default().fg(palette::TEXT_MUTED),
        )));
    } else if state.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (empty)",
            Style::default().fg(palette::TEXT_MUTED),
        )));
    } else {
        let render_end = (scroll + max_visible).min(state.entries.len());
        for idx in scroll..render_end {
            let entry = &state.entries[idx];
            let is_selected = idx == state.cursor;

            // Build the line prefix: indent + expand/collapse marker + icon.
            let indent = "  ".repeat(entry.depth);
            let expand_marker = if entry.is_dir {
                if entry.expanded {
                    "\u{25BC} "
                } else {
                    "\u{25B6} "
                } // ▼ / ▶
            } else {
                "  "
            };
            // No separate icon: the ▼/▶ expand marker already signals dirs,
            // and SMP emoji (📁/📄, U+1F4C1/U+1F4C4) render at inconsistent
            // column widths across terminals, breaking layout. See issue #1314.

            // Build the display text.
            let raw = format!("{indent}{expand_marker}{}", entry.name);
            let display = truncate_line_to_width(&raw, content_width.max(1));

            let style = if is_selected {
                menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };

            lines.push(Line::from(Span::styled(display, style)));
        }
    }

    // Pane chrome is the four-mode whale/light/grayscale/solarized ink; the
    // backend remap carries the dark tokens into community presets exactly as
    // it does for every other raw `palette` paint in this file. The pane floor
    // is the one slot those base themes never lifted into `UiTheme`: the dark
    // shells paint the raw ink field (remapped onto the live surface at draw
    // time), the light and grey shells paint their panel tint.
    let chrome = palette::UiTheme::for_mode(mode);
    let pane_bg = match mode {
        palette::PaletteMode::Dark => palette::WHALE_BG,
        palette::PaletteMode::Light => palette::LIGHT_PANEL,
        palette::PaletteMode::Grayscale | palette::PaletteMode::SolarizedLight => chrome.panel_bg,
    };
    // Horizontal padding only: `Padding::uniform(1)` ate two rows of a
    // compact pane and left zero rows for content (#63 follow-up).
    let section = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(Line::from(Span::styled(
                " Files ",
                Style::default().fg(chrome.accent_primary).bold(),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(chrome.border))
            .style(Style::default().bg(pane_bg))
            .padding(Padding::horizontal(1)),
    );

    f.render_widget(section, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture_workspace() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).expect("mkdir src/nested");
        std::fs::create_dir_all(root.join("docs")).expect("mkdir docs");
        std::fs::create_dir_all(root.join("node_modules/pkg")).expect("mkdir node_modules");
        std::fs::write(root.join("README.md"), "readme").expect("write README.md");
        std::fs::write(root.join("src/main.rs"), "fn main() {}").expect("write main.rs");
        std::fs::write(root.join("src/Lib.rs"), "").expect("write Lib.rs");
        std::fs::write(root.join("src/nested/mod.rs"), "").expect("write mod.rs");
        std::fs::write(root.join("docs/guide.md"), "").expect("write guide.md");
        dir
    }

    fn index_of(state: &FileTreeState, name: &str) -> usize {
        state
            .entries
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("entry {name} missing: {:?}", entry_names(state)))
    }

    fn entry_names(state: &FileTreeState) -> Vec<String> {
        state.entries.iter().map(|e| e.name.clone()).collect()
    }

    fn expand_by_name(state: &mut FileTreeState, name: &str) {
        state.cursor = index_of(state, name);
        assert!(state.activate().is_none(), "expanding {name} returns None");
    }

    /// The incremental expand path (splice) must produce exactly the flat
    /// list the old full rebuild produced, for several expansion orders.
    #[test]
    fn incremental_expand_matches_full_rebuild() {
        let ws = fixture_workspace();
        for order in [["src", "nested", "docs"], ["docs", "src", "nested"]] {
            // Plain test: no tokio runtime, so expand walks run synchronously.
            let mut state = FileTreeState::new(ws.path());
            assert!(!state.is_loading, "sync fallback builds immediately");
            for name in order {
                expand_by_name(&mut state, name);
            }

            let oracle = build_file_tree_inner(ws.path(), &state.expanded_dirs, None);
            assert_eq!(
                state.entries.len(),
                oracle.len(),
                "entry count parity for order {order:?}: {:?}",
                entry_names(&state)
            );
            for (spliced, rebuilt) in state.entries.iter().zip(oracle.iter()) {
                assert_eq!(spliced.name, rebuilt.name);
                assert_eq!(spliced.path, rebuilt.path);
                assert_eq!(spliced.is_dir, rebuilt.is_dir);
                assert_eq!(spliced.depth, rebuilt.depth);
                assert_eq!(spliced.expanded, rebuilt.expanded);
            }
        }
    }

    #[test]
    fn collapse_splices_out_subtree_without_io() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        expand_by_name(&mut state, "src");
        expand_by_name(&mut state, "nested");
        assert!(state.entries.iter().any(|e| e.name == "mod.rs"));

        // Collapse src: descendants leave the list, nested stays remembered.
        let src_idx = index_of(&state, "src");
        state.cursor = src_idx;
        assert!(state.activate().is_none());

        assert!(!state.entries[src_idx].expanded);
        assert!(!state.entries.iter().any(|e| e.name == "main.rs"));
        assert!(!state.entries.iter().any(|e| e.name == "mod.rs"));
        assert_eq!(state.cursor, src_idx, "cursor stays on the collapsed dir");
        let nested_norm = normalize_path(&ws.path().join("src/nested"));
        assert!(
            state.expanded_dirs.contains(&nested_norm),
            "collapsing a parent keeps descendant expansion state"
        );
    }

    #[test]
    fn re_expand_restores_descendant_expansion() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        expand_by_name(&mut state, "src");
        expand_by_name(&mut state, "nested");
        state.cursor = index_of(&state, "src");
        assert!(state.activate().is_none()); // collapse
        assert!(state.activate().is_none()); // re-expand

        let nested_idx = index_of(&state, "nested");
        assert!(state.entries[nested_idx].expanded);
        assert!(
            state.entries.iter().any(|e| e.name == "mod.rs"),
            "re-expanding the parent restores the expanded child subtree: {:?}",
            entry_names(&state)
        );
    }

    #[test]
    fn adjust_scroll_keeps_the_cursor_inside_the_visible_window() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        state.cursor = state.entries.len().saturating_sub(1);
        state.adjust_scroll(3);
        assert!(state.cursor < state.scroll_offset + 3);

        state.cursor = 0;
        state.adjust_scroll(3);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn stale_expand_results_are_discarded() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        expand_by_name(&mut state, "src");
        let src_norm = normalize_path(&ws.path().join("src"));

        // Collapse removes the pending walk; a result landing afterwards is
        // dropped instead of splicing into the collapsed node.
        state.cursor = index_of(&state, "src");
        assert!(state.activate().is_none());
        let ghost = vec![FileTreeEntry {
            name: "ghost.rs".to_string(),
            path: ws.path().join("src/ghost.rs"),
            is_dir: false,
            depth: 1,
            expanded: false,
        }];
        state.apply_expand_result(&src_norm, state.expand_seq, ghost.clone());
        assert!(!state.entries.iter().any(|e| e.name == "ghost.rs"));

        // A superseded sequence number is also dropped, and the newer
        // pending walk stays registered.
        state.pending_expands.insert(
            src_norm.clone(),
            PendingExpand {
                seq: 7,
                cell: Arc::new(Mutex::new(None)),
            },
        );
        state.expanded_dirs.insert(src_norm.clone());
        state.apply_expand_result(&src_norm, 6, ghost);
        assert!(!state.entries.iter().any(|e| e.name == "ghost.rs"));
        assert!(
            state.pending_expands.contains_key(&src_norm),
            "a stale result must not clear the newer pending walk"
        );
    }

    #[test]
    fn splice_shifts_cursor_positioned_after_the_expanded_dir() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        let src_norm = normalize_path(&ws.path().join("src"));
        let src_idx = index_of(&state, "src");
        let readme_idx = index_of(&state, "README.md");
        assert!(readme_idx > src_idx);

        state.expanded_dirs.insert(src_norm.clone());
        state.entries[src_idx].expanded = true;
        state.pending_expands.insert(
            src_norm.clone(),
            PendingExpand {
                seq: 1,
                cell: Arc::new(Mutex::new(None)),
            },
        );
        state.cursor = readme_idx;
        let children = build_file_tree_inner(
            ws.path(),
            &state.expanded_dirs,
            Some(&ws.path().join("src")),
        );
        let inserted = children.len();
        assert!(inserted > 0);

        state.apply_expand_result(&src_norm, 1, children);

        assert_eq!(state.cursor, readme_idx + inserted);
        assert_eq!(state.entries[state.cursor].name, "README.md");
    }

    #[tokio::test]
    async fn async_expand_splices_children_after_poll() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        for _ in 0..500 {
            state.poll_loading();
            if !state.is_loading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!state.is_loading, "initial async build completes");

        state.cursor = index_of(&state, "src");
        assert!(state.activate().is_none());
        assert!(
            state.entries[state.cursor].expanded,
            "expand acknowledges the keypress immediately"
        );

        for _ in 0..500 {
            state.poll_pending_expands();
            if state.entries.iter().any(|e| e.name == "main.rs") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            state.entries.iter().any(|e| e.name == "main.rs"),
            "background walk results are spliced in on poll: {:?}",
            entry_names(&state)
        );
        assert!(state.pending_expands.is_empty());
    }

    #[test]
    fn poll_background_reports_applied_expand_results() {
        let ws = fixture_workspace();
        let mut state = FileTreeState::new(ws.path());
        let src_norm = normalize_path(&ws.path().join("src"));
        let src_idx = index_of(&state, "src");

        state.expanded_dirs.insert(src_norm.clone());
        state.entries[src_idx].expanded = true;
        let children = build_file_tree_inner(
            ws.path(),
            &state.expanded_dirs,
            Some(&ws.path().join("src")),
        );
        state.pending_expands.insert(
            src_norm.clone(),
            PendingExpand {
                seq: 1,
                cell: Arc::new(Mutex::new(Some(children))),
            },
        );

        assert!(
            state.poll_background(),
            "a drained expand result must request a repaint"
        );
        assert!(state.entries.iter().any(|e| e.name == "main.rs"));
        assert!(
            state.pending_expands.is_empty(),
            "applied expand must clear pending state"
        );
        assert!(
            !state.poll_background(),
            "idle poll must not request a second repaint"
        );
    }
}

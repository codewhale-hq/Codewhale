//! Fuzzy file-picker modal (Ctrl+P).
//!
//! Opens an overlay populated with workspace-relative paths discovered by a
//! single-pass `WalkBuilder` walk (depth from `mention_walk_depth`, default
//! 10, `0` = unlimited; hidden=true, follow_links=false,
//! `.gitignore` honored). The walk keeps at most [`MAX_CANDIDATES`] paths in
//! walk order so opening the picker stays bounded on huge repos. Subsequent
//! keystrokes filter that cached list in memory using a small subsequence +
//! first-letter-bonus scorer — no per-keystroke disk traversal.
//!
//! When the typed query matches nothing in that truncated index, a targeted
//! rescan walks from the query's existing path prefix (or the workspace root)
//! and collects only matching files. Raising `mention_walk_depth` cannot
//! recover files past the 20k cutoff; the rescan can (#2488).
//!
//! Enter emits a [`ViewEvent::FilePickerSelected`] which the UI handler turns
//! into an `@<path>` insertion at the composer cursor.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ignore::WalkBuilder;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    render_panel_scroll_rail, render_underwater_surface,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::workspace_discovery::{
    DISCOVERY_ALWAYS_DIRS, path_is_excluded_from_discovery,
};
use codewhale_tui_kit::menu_style;

/// Maximum number of candidates collected from the initial walk. Keeps memory
/// bounded for very large monorepos; matches the limits codex-rs uses for the
/// equivalent overlay. Files past this cutoff are recovered by a query-targeted
/// rescan rather than by raising the cap or `mention_walk_depth` (#2488).
const MAX_CANDIDATES: usize = 20_000;

/// Cap on files a miss-rescan may add. The walk itself continues past
/// [`MAX_CANDIDATES`] looking for matches; only this many hits are merged.
const MAX_RESCAN_HITS: usize = 512;

/// Default walk depth used by the picker's own tests. Production callers pass
/// the configured `mention_walk_depth` (default 10, `0` = unlimited) through
/// [`FilePickerView::new_with_relevance_and_depth`], mirroring the `Workspace`
/// fuzzy index default (`DEFAULT_COMPLETIONS_WALK_DEPTH`).
#[cfg(test)]
const WALK_DEPTH: usize = 10;

/// Visible candidate rows in the overlay.
const VISIBLE_ROWS: usize = 14;

const MODIFIED_BOOST: i32 = 360;
const MENTIONED_BOOST: i32 = 240;
const TOOL_BOOST: i32 = 160;

/// Working-set hints captured when the picker opens.
///
/// The picker keeps this as plain path strings so filtering stays in-memory and
/// per-keystroke work remains the same shape as the original fuzzy search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePickerRelevance {
    modified: HashSet<String>,
    mentioned: HashSet<String>,
    tool: HashSet<String>,
}

impl FilePickerRelevance {
    pub fn mark_modified(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !path.is_empty() {
            self.modified.insert(path);
        }
    }

    pub fn mark_mentioned(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !path.is_empty() {
            self.mentioned.insert(path);
        }
    }

    pub fn mark_tool(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !path.is_empty() {
            self.tool.insert(path);
        }
    }

    fn boost_for(&self, path: &str) -> i32 {
        let mut boost = 0;
        if self.modified.contains(path) {
            boost += MODIFIED_BOOST;
        }
        if self.mentioned.contains(path) {
            boost += MENTIONED_BOOST;
        }
        if self.tool.contains(path) {
            boost += TOOL_BOOST;
        }
        boost
    }

    fn markers_for(&self, path: &str) -> String {
        let mut markers = String::with_capacity(3);
        markers.push(if self.modified.contains(path) {
            'M'
        } else {
            ' '
        });
        markers.push(if self.mentioned.contains(path) {
            '@'
        } else {
            ' '
        });
        markers.push(if self.tool.contains(path) { 'T' } else { ' ' });
        markers
    }
}

pub struct FilePickerView {
    /// All workspace-relative candidate paths, captured once at construction.
    candidates: Vec<String>,
    /// Working-set relevance hints, captured once at construction.
    relevance: FilePickerRelevance,
    /// Filtered indices into `candidates`, sorted by descending score.
    filtered: Vec<usize>,
    /// User's typed query (lowercased on each refilter).
    query: String,
    /// Selected row within `filtered`.
    selected: usize,
    /// Top of the visible window within `filtered`.
    scroll: usize,
    /// Exact visible row targets from the last render for mouse parity.
    last_row_hitboxes: RefCell<Vec<(u16, usize)>>,
    /// UI locale captured from the app at construction (#4057 wave 2).
    locale: Locale,
    /// True until the background workspace scan delivers (#3905). The picker
    /// paints immediately in this state instead of blocking the event loop on
    /// a `git status` subprocess and a 20k-file walk.
    is_loading: bool,
    /// True while a query-targeted rescan is in flight (#2488).
    is_rescanning: bool,
    /// Where the background scan drops its result. `None` once drained, or
    /// when the scan ran synchronously (no tokio runtime, i.e. unit tests).
    loading_cell: Option<Arc<Mutex<Option<PickerScan>>>>,
    /// Retained so a query that misses the truncated index can rescan.
    workspace_root: PathBuf,
    /// Depth used by the initial walk and by a miss-rescan (`None` = unlimited).
    max_depth: Option<usize>,
    /// True when the initial walk stopped at [`MAX_CANDIDATES`].
    index_truncated: bool,
    /// Lowercased query a rescan was last completed for. Prevents repeating
    /// a walk that already produced no extra hits.
    rescan_query: Option<String>,
}

/// What the off-thread workspace scan produces: the candidate paths and the
/// git-reported modified paths, which are the only two blocking parts of
/// building this picker.
struct WorkspaceScan {
    candidates: Vec<String>,
    modified: Vec<String>,
    truncated: bool,
}

/// Either the opening walk or a later query-targeted miss-rescan.
enum PickerScan {
    Initial(WorkspaceScan),
    Targeted { query: String, hits: Vec<String> },
}

struct CandidateWalk {
    paths: Vec<String>,
    truncated: bool,
}

impl FilePickerView {
    /// Build a picker with working-set relevance hints, using the default
    /// walk depth ([`WALK_DEPTH`]). Test-only convenience; production code uses
    /// [`FilePickerView::new_with_relevance_and_depth`] with the configured
    /// `mention_walk_depth`.
    #[cfg(test)]
    pub fn new_with_relevance(workspace_root: &Path, relevance: FilePickerRelevance) -> Self {
        Self::new_with_relevance_and_depth(workspace_root, relevance, WALK_DEPTH, Locale::En)
    }

    /// Build a picker with working-set relevance hints and an explicit walk
    /// depth. A depth of `0` disables the depth limit so files in deeply
    /// nested workspaces (>= 6 levels) remain discoverable. Files past the
    /// [`MAX_CANDIDATES`] walk-order cutoff are recovered by a targeted
    /// rescan when the typed query misses the index (#2488).
    pub fn new_with_relevance_and_depth(
        workspace_root: &Path,
        relevance: FilePickerRelevance,
        walk_depth: usize,
        locale: Locale,
    ) -> Self {
        let max_depth = if walk_depth == 0 {
            None
        } else {
            Some(walk_depth)
        };

        // Outside a tokio runtime (plain unit tests) do the work inline, so
        // tests keep observing a fully-populated picker from the constructor.
        if tokio::runtime::Handle::try_current().is_err() {
            let walk = collect_candidates_limited(workspace_root, max_depth, MAX_CANDIDATES);
            let mut relevance = relevance;
            for path in crate::tui::file_picker_relevance::modified_workspace_paths(workspace_root)
            {
                relevance.mark_modified(path);
            }
            let mut view = Self {
                candidates: walk.paths,
                relevance,
                filtered: Vec::new(),
                query: String::new(),
                selected: 0,
                scroll: 0,
                last_row_hitboxes: RefCell::new(Vec::new()),
                locale,
                is_loading: false,
                is_rescanning: false,
                loading_cell: None,
                workspace_root: workspace_root.to_path_buf(),
                max_depth,
                index_truncated: walk.truncated,
                rescan_query: None,
            };
            view.refilter();
            return view;
        }

        // Both halves of the scan are blocking: `git status` is a subprocess,
        // and the walk visits up to MAX_CANDIDATES paths. Neither belongs on
        // the event loop — Ctrl+P used to freeze the whole TUI until both
        // finished (#3905), the same failure #3899/#3900 fixed for the
        // adjacent @-mention and file-tree paths.
        let loading_cell = Arc::new(Mutex::new(None));
        let cell = loading_cell.clone();
        let root = workspace_root.to_path_buf();
        codewhale_util::utils::spawn_blocking_supervised("file-picker-scan", move || {
            let walk = collect_candidates_limited(&root, max_depth, MAX_CANDIDATES);
            let scan = PickerScan::Initial(WorkspaceScan {
                candidates: walk.paths,
                modified: crate::tui::file_picker_relevance::modified_workspace_paths(&root),
                truncated: walk.truncated,
            });
            if let Ok(mut guard) = cell.lock() {
                *guard = Some(scan);
            }
        });

        let mut view = Self {
            candidates: Vec::new(),
            relevance,
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            last_row_hitboxes: RefCell::new(Vec::new()),
            locale,
            is_loading: true,
            is_rescanning: false,
            loading_cell: Some(loading_cell),
            workspace_root: workspace_root.to_path_buf(),
            max_depth,
            index_truncated: false,
            rescan_query: None,
        };
        view.refilter();
        view
    }

    /// Test helper: a picker whose in-memory index is already known, including
    /// whether that index hit [`MAX_CANDIDATES`]. Used to exercise miss-rescan
    /// without creating 20k files.
    #[cfg(test)]
    fn from_preloaded(
        workspace_root: &Path,
        candidates: Vec<String>,
        truncated: bool,
        max_depth: Option<usize>,
    ) -> Self {
        let mut view = Self {
            candidates,
            relevance: FilePickerRelevance::default(),
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            last_row_hitboxes: RefCell::new(Vec::new()),
            locale: Locale::En,
            is_loading: false,
            is_rescanning: false,
            loading_cell: None,
            workspace_root: workspace_root.to_path_buf(),
            max_depth,
            index_truncated: truncated,
            rescan_query: None,
        };
        view.refilter();
        view
    }

    /// Drain the background scan if it has landed. Called from `tick`, which
    /// the view stack runs on the top view every loop iteration.
    fn poll_loading(&mut self) {
        if !self.is_loading && !self.is_rescanning {
            return;
        }
        // Take the Arc out temporarily to avoid a double-borrow of self.
        let Some(cell) = self.loading_cell.take() else {
            self.is_loading = false;
            self.is_rescanning = false;
            return;
        };
        let scan = cell.lock().ok().and_then(|mut guard| guard.take());
        match scan {
            Some(PickerScan::Initial(scan)) => {
                self.candidates = scan.candidates;
                self.index_truncated = scan.truncated;
                for path in scan.modified {
                    self.relevance.mark_modified(path);
                }
                self.is_loading = false;
                // The user may already have typed while the scan ran; refilter
                // against the query they actually have, not an empty one.
                self.refilter();
            }
            Some(PickerScan::Targeted { query, hits }) => {
                let current = self.query.trim().to_lowercase();
                self.is_rescanning = false;
                if current == query {
                    self.merge_rescan_hits(&query, hits);
                } else {
                    // Query moved on while the walk ran; try again for the
                    // query the user actually has.
                    self.maybe_rescan();
                }
            }
            None => self.loading_cell = Some(cell),
        }
    }

    fn refilter(&mut self) {
        self.refilter_from_index();
        self.maybe_rescan();
    }

    fn refilter_from_index(&mut self) {
        let query = self.query.trim().to_lowercase();
        let mut scored: Vec<(usize, i32, i32, i32)> = if query.is_empty() {
            self.candidates
                .iter()
                .enumerate()
                .map(|(idx, path)| {
                    let boost = self.relevance.boost_for(path);
                    (idx, boost, 0, boost)
                })
                .collect()
        } else {
            self.candidates
                .iter()
                .enumerate()
                .filter_map(|(idx, path)| {
                    score(&query, path).map(|fuzzy| {
                        let boost = self.relevance.boost_for(path);
                        (idx, fuzzy + boost, fuzzy, boost)
                    })
                })
                .collect()
        };

        // Higher scores first; tie-break by ascending path length, then lex order
        // so shorter / more central matches surface above deep nested ones.
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| b.3.cmp(&a.3))
                .then_with(|| self.candidates[a.0].len().cmp(&self.candidates[b.0].len()))
                .then_with(|| self.candidates[a.0].cmp(&self.candidates[b.0]))
        });

        self.filtered = scored.into_iter().map(|(idx, _, _, _)| idx).collect();
        if self.filtered.is_empty() {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
        self.adjust_scroll();
    }

    /// When the in-memory index is known-incomplete and the typed query
    /// matches nothing in it, walk from the query's existing path prefix
    /// (or the workspace root) collecting only matching files (#2488).
    fn maybe_rescan(&mut self) {
        if self.is_loading || self.is_rescanning || !self.index_truncated {
            return;
        }
        if !self.filtered.is_empty() {
            return;
        }
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        // A single letter almost never misses a 20k index; require a bit
        // more specificity so a stray miss does not walk a huge tree.
        let specific_enough =
            query.chars().count() >= 2 || query.contains('/') || query.contains('\\');
        if !specific_enough {
            return;
        }
        if self.rescan_query.as_deref() == Some(query.as_str()) {
            return;
        }

        if tokio::runtime::Handle::try_current().is_err() {
            let hits = collect_query_matches(
                &self.workspace_root,
                self.max_depth,
                &query,
                MAX_RESCAN_HITS,
            );
            self.merge_rescan_hits(&query, hits);
            return;
        }

        self.is_rescanning = true;
        let loading_cell = Arc::new(Mutex::new(None));
        let cell = loading_cell.clone();
        self.loading_cell = Some(loading_cell);
        let root = self.workspace_root.clone();
        let max_depth = self.max_depth;
        let query_for_scan = query.clone();
        codewhale_util::utils::spawn_blocking_supervised("file-picker-rescan", move || {
            let hits = collect_query_matches(&root, max_depth, &query_for_scan, MAX_RESCAN_HITS);
            if let Ok(mut guard) = cell.lock() {
                *guard = Some(PickerScan::Targeted {
                    query: query_for_scan,
                    hits,
                });
            }
        });
    }

    fn merge_rescan_hits(&mut self, query: &str, hits: Vec<String>) {
        self.rescan_query = Some(query.to_string());
        self.is_rescanning = false;
        if !hits.is_empty() {
            for hit in hits {
                if !self.candidates.iter().any(|existing| existing == &hit) {
                    self.candidates.push(hit);
                }
            }
        }
        self.refilter_from_index();
    }

    fn adjust_scroll(&mut self) {
        if self.filtered.is_empty() {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    /// Apply one [`list_nav`](codewhale_tui_kit::list_nav) motion (#6290), returning
    /// whether it was consumed. This is a typing surface, so only the
    /// typing-safe vocabulary applies — no letter alias may eat a query
    /// character. `Prev`/`Next` wrap; paging and Home/End clamp.
    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        if self.filtered.is_empty() {
            return false;
        }
        let Some(next) = codewhale_tui_kit::list_nav::apply(
            self.selected,
            self.filtered.len(),
            VISIBLE_ROWS,
            motion,
        ) else {
            return false;
        };
        self.selected = next;
        self.adjust_scroll();
        true
    }

    fn selected_path(&self) -> Option<&str> {
        let idx = *self.filtered.get(self.selected)?;
        self.candidates.get(idx).map(String::as_str)
    }

    /// Visible candidate count for tests / diagnostics.
    #[cfg(test)]
    pub fn visible_count(&self) -> usize {
        self.filtered.len()
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[cfg(test)]
    pub fn selected_for_test(&self) -> Option<&str> {
        self.selected_path()
    }

    #[cfg(test)]
    pub fn markers_for_test(&self, path: &str) -> String {
        self.relevance.markers_for(path)
    }
}

impl ModalView for FilePickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::FilePicker
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Movement keys come from the shared vocabulary (#6290), typing-safe
        // set only. This match owns the filter's own keys.
        if let Some(motion) = codewhale_tui_kit::list_nav::motion_while_typing(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => {
                if let Some(path) = self.selected_path() {
                    let path = path.to_string();
                    return ViewAction::EmitAndClose(ViewEvent::FilePickerSelected { path });
                }
                ViewAction::Close
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.scroll = 0;
                self.refilter();
                ViewAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
                self.selected = 0;
                self.scroll = 0;
                self.refilter();
                ViewAction::None
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !ch.is_control() =>
            {
                self.query.push(ch);
                self.selected = 0;
                self.scroll = 0;
                self.refilter();
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Prev);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Next);
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .last_row_hitboxes
                    .borrow()
                    .iter()
                    .find_map(|(y, idx)| (*y == mouse.row).then_some(*idx));
                let Some(idx) = hit else {
                    return ViewAction::None;
                };
                if idx == self.selected {
                    if let Some(path) = self.selected_path() {
                        return ViewAction::EmitAndClose(ViewEvent::FilePickerSelected {
                            path: path.to_string(),
                        });
                    }
                } else {
                    self.selected = idx;
                    self.adjust_scroll();
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn tick(&mut self) -> ViewAction {
        self.poll_loading();
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let match_count = self.filtered.len();
        let title = if match_count == 1 {
            tr(self.locale, MessageId::FilePickerMatchSingular).into_owned()
        } else {
            tr(self.locale, MessageId::FilePickerMatchesPlural)
                .replace("{count}", &match_count.to_string())
        };
        let inner = render_underwater_surface(area, buf, title);

        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("↑/↓", "move"),
                ActionHint::new("Enter", "insert @path"),
                ActionHint::new("Esc", "cancel"),
            ],
        );
        let visible = VISIBLE_ROWS.min(content.height.saturating_sub(2) as usize);
        let content = render_panel_scroll_rail(
            content,
            buf,
            self.filtered.len(),
            self.scroll,
            visible,
            true,
        );

        let mut lines: Vec<Line<'static>> = Vec::new();
        // Query line.
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(palette::WHALE_ACTION).bold()),
            // Explicit ink: the picker paints WHALE_BG, so an unstyled query
            // would inherit a dark terminal default on light-profile terminals.
            Span::styled(
                self.query.clone(),
                Style::default().fg(palette::TEXT_PRIMARY),
            ),
            Span::styled(
                " ",
                Style::default()
                    .fg(palette::WHALE_BG)
                    .bg(palette::WHALE_ACTION),
            ),
        ]));
        lines.push(Line::from(""));

        let end = (self.scroll + visible).min(self.filtered.len());
        self.last_row_hitboxes.borrow_mut().clear();
        if self.is_loading || (self.is_rescanning && self.filtered.is_empty()) {
            // "No matches" would be a lie while the walk is still running.
            lines.push(Line::from(Span::styled(
                format!("  {}", tr(self.locale, MessageId::FilePickerScanning)),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        } else if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No matches",
                Style::default().fg(palette::TEXT_MUTED),
            )));
        } else {
            for idx in self.scroll..end {
                let path = &self.candidates[self.filtered[idx]];
                let selected = idx == self.selected;
                let style = if selected {
                    menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
                } else {
                    Style::default().fg(palette::TEXT_PRIMARY)
                };
                let prefix = format!("{} ", codewhale_tui_kit::glyphs::selection_marker(selected));
                let marker_field = if content.width >= 18 {
                    format!("{} ", self.relevance.markers_for(path))
                } else {
                    String::new()
                };
                let reserved = prefix.chars().count() + marker_field.chars().count();
                let display =
                    truncate_path(path, (content.width as usize).saturating_sub(reserved));
                let mut line = Line::from(format!("{prefix}{marker_field}{display}"));
                line.style = style;
                let y = content
                    .y
                    .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
                self.last_row_hitboxes.borrow_mut().push((y, idx));
                lines.push(line);
            }
        }

        Paragraph::new(lines)
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .render(content, buf);
    }
}

/// Keep the path's tail within `max` display cells, cutting only between
/// grapheme clusters: counting chars overflowed CJK rows and could split a
/// ZWJ or combining sequence (U07-06). ASCII output is unchanged.
fn truncate_path(path: &str, max: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if max == 0 {
        return String::new();
    }
    if codewhale_tui_kit::ui_text::text_display_width(path) <= max {
        return path.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut used = 0usize;
    let mut start = path.len();
    for (idx, grapheme) in path.grapheme_indices(true).rev() {
        let width = codewhale_tui_kit::ui_text::text_display_width(grapheme);
        if used + width > budget {
            break;
        }
        used += width;
        start = idx;
    }
    format!("…{}", &path[start..])
}

/// Single-pass walk that collects workspace-relative paths. `max_depth` of
/// `None` walks the whole tree (still bounded by `MAX_CANDIDATES` and
/// `.gitignore`); `Some(n)` caps the recursion at `n` levels.
#[cfg(test)]
fn collect_candidates(root: &Path, max_depth: Option<usize>) -> Vec<String> {
    collect_candidates_limited(root, max_depth, MAX_CANDIDATES).paths
}

fn collect_candidates_limited(
    root: &Path,
    max_depth: Option<usize>,
    limit: usize,
) -> CandidateWalk {
    let mut out: Vec<String> = Vec::new();
    let mut truncated = push_matching_files(
        MatchingFileWalk {
            walk_root: root,
            display_root: root,
            max_depth,
            honor_gitignore: true,
            limit,
            matches: &|_| true,
        },
        &mut out,
        None,
    );
    if !truncated {
        // Whitelist AI-tool dot-directories so they're discoverable even when
        // gitignored. Walk each one separately with gitignore disabled.
        for dir in DISCOVERY_ALWAYS_DIRS {
            let dot_dir = root.join(dir);
            if !dot_dir.is_dir() {
                continue;
            }
            truncated = push_matching_files(
                MatchingFileWalk {
                    walk_root: &dot_dir,
                    display_root: root,
                    max_depth: max_depth.map(|d| d.saturating_sub(1)),
                    honor_gitignore: false,
                    limit,
                    matches: &|_| true,
                },
                &mut out,
                None,
            );
            if truncated {
                break;
            }
        }
    }
    out.sort();
    CandidateWalk {
        paths: out,
        truncated,
    }
}

/// Walk matching files for a query that missed the truncated index.
///
/// Starts at the longest existing directory prefix of `query` so a typed path
/// like `packages/app/lib/room_chat_shell` does not re-walk the first 20k
/// files. The walk continues past [`MAX_CANDIDATES`]; only `limit` hits are
/// kept.
fn collect_query_matches(
    root: &Path,
    max_depth: Option<usize>,
    query: &str,
    limit: usize,
) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let matches = |path: &str| score(&needle, path).is_some();
    let (start, depth) = targeted_walk_root(root, query, max_depth);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let under_always = always_dir_prefix(root, &start).is_some();
    let hit_cap = push_matching_files(
        MatchingFileWalk {
            walk_root: &start,
            display_root: root,
            max_depth: depth,
            honor_gitignore: !under_always,
            limit,
            matches: &matches,
        },
        &mut out,
        Some(&mut seen),
    );
    if start.as_path() == root && !hit_cap {
        for dir in DISCOVERY_ALWAYS_DIRS {
            let dot_dir = root.join(dir);
            if !dot_dir.is_dir() {
                continue;
            }
            if push_matching_files(
                MatchingFileWalk {
                    walk_root: &dot_dir,
                    display_root: root,
                    max_depth: max_depth.map(|d| d.saturating_sub(1)),
                    honor_gitignore: false,
                    limit,
                    matches: &matches,
                },
                &mut out,
                Some(&mut seen),
            ) {
                break;
            }
        }
    }
    out.sort();
    out
}

/// Longest existing directory prefix of `query` under `root`. Depth is
/// reduced by the number of consumed components so a targeted walk cannot
/// see farther than the original `mention_walk_depth` cap.
fn targeted_walk_root(
    root: &Path,
    query: &str,
    max_depth: Option<usize>,
) -> (PathBuf, Option<usize>) {
    let normalized = query.replace('\\', "/");
    let mut dir = root.to_path_buf();
    let mut consumed = 0usize;
    for component in normalized.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            break;
        }
        let next = dir.join(component);
        if next.is_dir() {
            dir = next;
            consumed += 1;
        } else {
            break;
        }
    }
    (dir, max_depth.map(|depth| depth.saturating_sub(consumed)))
}

fn always_dir_prefix(root: &Path, path: &Path) -> Option<&'static str> {
    DISCOVERY_ALWAYS_DIRS.iter().copied().find(|dir| {
        let always = root.join(dir);
        path == always || path.starts_with(&always)
    })
}

struct MatchingFileWalk<'a> {
    walk_root: &'a Path,
    display_root: &'a Path,
    max_depth: Option<usize>,
    honor_gitignore: bool,
    limit: usize,
    matches: &'a dyn Fn(&str) -> bool,
}

fn push_matching_files(
    walk: MatchingFileWalk<'_>,
    out: &mut Vec<String>,
    mut seen: Option<&mut HashSet<String>>,
) -> bool {
    let MatchingFileWalk {
        walk_root,
        display_root,
        max_depth,
        honor_gitignore,
        limit,
        matches,
    } = walk;
    if limit == 0 || out.len() >= limit {
        return true;
    }
    let mut builder = WalkBuilder::new(walk_root);
    builder
        .hidden(true)
        .follow_links(false)
        .max_depth(max_depth);
    if honor_gitignore {
        builder.git_ignore(true).git_exclude(true).git_global(true);
    } else {
        builder.git_ignore(false).ignore(false);
    }

    for entry in builder.build().flatten() {
        if !honor_gitignore && path_is_excluded_from_discovery(display_root, entry.path()) {
            continue;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(display_root).unwrap_or(path);
        // A pick becomes @-mention text that must resolve back to this file.
        // A non-UTF-8 name cannot round-trip through text, and its lossy form
        // can collide with another file's name, so it is not offered (U07-06).
        if rel.as_os_str().is_empty() || rel.to_str().is_none() {
            continue;
        }
        let display = path_to_workspace_string(rel);
        if display.is_empty() || !matches(&display) {
            continue;
        }
        if let Some(seen) = seen.as_mut()
            && !seen.insert(display.clone())
        {
            continue;
        }
        out.push(display);
        if out.len() >= limit {
            return true;
        }
    }
    false
}

fn path_to_workspace_string(path: &Path) -> String {
    // Use forward-slash separators for cross-platform display, matching how
    // @-mentions are spelled in the composer.
    let mut out = String::new();
    for (idx, comp) in path.components().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    out
}

/// Subsequence scorer with first-letter and boundary bonuses.
///
/// Returns `None` if `query` is not a subsequence of `path` (case-insensitive),
/// otherwise a positive score where higher is better.
///
/// Heuristics (kept deliberately small and predictable):
/// * +25 for each match that lands at the start of the path or right after a
///   boundary character (`/`, `_`, `-`, `.`, ` `).
/// * +10 if the very first character of the query matches the first character
///   of the path.
/// * +5 per consecutive match (rewards contiguous runs like typing "main" and
///   matching `main.rs`).
/// * Penalty proportional to the gap between consecutive matches keeps tightly
///   matched candidates above scattered ones.
pub fn score(query: &str, path: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let p: Vec<char> = path.chars().flat_map(char::to_lowercase).collect();
    if q.len() > p.len() {
        return None;
    }

    let mut qi = 0usize;
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut consecutive = 0i32;

    for (i, ch) in p.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if *ch == q[qi] {
            // Boundary / start bonus.
            if i == 0 {
                score += 25;
                if qi == 0 {
                    score += 10;
                }
            } else if matches!(p[i - 1], '/' | '_' | '-' | '.' | ' ') {
                score += 25;
            } else {
                score += 1;
            }

            // Consecutive bonus.
            if last_match == Some(i.saturating_sub(1)) {
                consecutive += 1;
                score += 5 * consecutive;
            } else {
                consecutive = 0;
            }

            // Gap penalty.
            if let Some(prev) = last_match {
                let gap = i - prev - 1;
                score -= gap as i32;
            }

            last_match = Some(i);
            qi += 1;
        }
    }

    if qi == q.len() { Some(score) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn score_subsequence_match() {
        // Identical query matches start with high bonus.
        let a = score("main", "main.rs").unwrap();
        let b = score("main", "src/very/deep/main.rs").unwrap();
        assert!(a > b, "a={a} b={b}");
    }

    #[test]
    fn score_rejects_non_subsequence() {
        assert!(score("zzz", "main.rs").is_none());
        assert!(score("xyz", "src/lib.rs").is_none());
    }

    #[test]
    fn query_line_carries_explicit_ink_on_the_dark_surface() {
        // The picker paints WHALE_BG, so the typed query must carry its own
        // fg: light-profile terminals default to black ink.
        let dir = TempDir::new().expect("tempdir");
        let mut picker =
            FilePickerView::new_with_relevance(dir.path(), FilePickerRelevance::default());
        picker.query = "main".to_string();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf);
        let mut checked = 0;
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if !row.contains("> main") {
                continue;
            }
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                let symbol = cell.symbol();
                if symbol.trim().is_empty() || symbol == ">" {
                    continue;
                }
                assert_eq!(
                    cell.style().fg,
                    Some(palette::TEXT_PRIMARY),
                    "query cell ({x}, {y}) must carry explicit body ink",
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected a rendered query line");
    }

    #[test]
    fn score_boundary_bonus_beats_substring() {
        // "fp" matches the boundary letters in "file_picker.rs" but only the
        // first letter in "filepicker.rs" — so the boundary candidate should
        // win.
        let boundary = score("fp", "src/file_picker.rs").unwrap();
        let inline = score("fp", "src/filepicker.rs");
        // inline doesn't even contain 'p' immediately following 'f'? It does:
        // f-i-l-e-p-i-c-k-e-r — 'p' is preceded by 'e' (no boundary), so it
        // gets only the +1 path score, while boundary gets +25 for the 'p'
        // following the underscore.
        if let Some(inline_score) = inline {
            assert!(
                boundary > inline_score,
                "boundary={boundary} inline={inline_score}"
            );
        }
    }

    #[test]
    fn score_case_insensitive() {
        assert!(score("MAIN", "main.rs").is_some());
        assert!(score("main", "MAIN.RS").is_some());
    }

    #[test]
    fn score_empty_query_returns_zero() {
        assert_eq!(score("", "anything").unwrap(), 0);
    }

    #[test]
    fn picker_typing_narrows_candidates() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();

        let mut view = FilePickerView::new_with_relevance(root, FilePickerRelevance::default());
        // Empty query -> all 4 files visible.
        assert_eq!(view.visible_count(), 4, "expected all 4 candidates");

        // Typing "main" should narrow to just src/main.rs.
        for ch in "main".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(view.query(), "main");
        let visible = view.visible_count();
        assert_eq!(visible, 1, "expected exactly 1 match for 'main'");
        let selected = view.selected_for_test().expect("selected path");
        assert!(selected.ends_with("main.rs"), "selected = {selected}");
    }

    #[test]
    fn picker_empty_query_prioritizes_working_set_files() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();

        let mut relevance = FilePickerRelevance::default();
        relevance.mark_modified("src/lib.rs");
        let view = FilePickerView::new_with_relevance(root, relevance);

        assert_eq!(view.selected_for_test(), Some("src/lib.rs"));
        assert_eq!(view.markers_for_test("src/lib.rs"), "M  ");
    }

    #[test]
    fn picker_fuzzy_query_keeps_working_set_boosts() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/alpha.rs"), "").unwrap();
        fs::write(root.join("src/zeta.rs"), "").unwrap();

        let mut relevance = FilePickerRelevance::default();
        relevance.mark_mentioned("src/zeta.rs");
        relevance.mark_tool("src/zeta.rs");
        let mut view = FilePickerView::new_with_relevance(root, relevance);
        for ch in "rs".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        assert_eq!(view.selected_for_test(), Some("src/zeta.rs"));
        assert_eq!(view.markers_for_test("src/zeta.rs"), " @T");
    }

    #[test]
    fn picker_backspace_widens_candidates() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("a.txt"), "").unwrap();
        fs::write(root.join("b.txt"), "").unwrap();

        let mut view = FilePickerView::new_with_relevance(root, FilePickerRelevance::default());
        view.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(view.visible_count(), 1);
        view.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(view.visible_count(), 2);
    }

    #[test]
    fn picker_enter_emits_event() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("only.txt"), "").unwrap();

        let mut view = FilePickerView::new_with_relevance(root, FilePickerRelevance::default());
        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            ViewAction::EmitAndClose(ViewEvent::FilePickerSelected { path }) => {
                assert!(path.ends_with("only.txt"));
            }
            other => panic!("expected EmitAndClose(FilePickerSelected), got {other:?}"),
        }
    }

    #[test]
    fn picker_esc_closes_without_emit() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("only.txt"), "").unwrap();

        let mut view = FilePickerView::new_with_relevance(root, FilePickerRelevance::default());
        let action = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(action, ViewAction::Close));
    }

    #[test]
    fn picker_honors_gitignore() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        // .gitignore filtering only kicks in inside a git repo or with an
        // explicit `.ignore` file. Use `.ignore` which `WalkBuilder` honors
        // even outside of git.
        fs::write(root.join(".ignore"), "skipme.txt\n").unwrap();
        fs::write(root.join("keepme.txt"), "").unwrap();
        fs::write(root.join("skipme.txt"), "").unwrap();

        let view = FilePickerView::new_with_relevance(root, FilePickerRelevance::default());
        let visible: Vec<_> = view
            .filtered
            .iter()
            .map(|i| view.candidates[*i].as_str())
            .collect();
        assert!(visible.iter().any(|p| p.ends_with("keepme.txt")));
        assert!(
            !visible.iter().any(|p| p.ends_with("skipme.txt")),
            "skipme.txt should be filtered by .ignore: {visible:?}"
        );
    }

    #[test]
    fn picker_finds_deeply_nested_files_within_walk_depth() {
        // #2488: a file inside a 6-level-deep directory sits at component depth
        // 7 and was excluded by the old depth-6 cap. The default depth (10) now
        // reaches it, and `0` (unlimited) reaches arbitrarily deep files.
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let nested = root.join("a/b/c/d/e/f");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("deep.rs"), "deep").unwrap();
        let deeper = root.join("a/b/c/d/e/f/g/h/i/j/k");
        fs::create_dir_all(&deeper).unwrap();
        fs::write(deeper.join("very_deep.rs"), "deeper").unwrap();

        // The old default (6) misses the depth-7 file — the reported bug.
        let shallow = collect_candidates(root, Some(6));
        assert!(
            !shallow.iter().any(|p| p == "a/b/c/d/e/f/deep.rs"),
            "depth-6 cap should miss the depth-7 file: {shallow:?}"
        );

        // The new default reaches files inside a 6-level-deep directory.
        let default = collect_candidates(root, Some(WALK_DEPTH));
        assert!(
            default.iter().any(|p| p == "a/b/c/d/e/f/deep.rs"),
            "default walk depth should reach depth-7 files: {default:?}"
        );

        // Unlimited (mention_walk_depth = 0) reaches arbitrarily deep files.
        let unlimited = collect_candidates(root, None);
        assert!(
            unlimited
                .iter()
                .any(|p| p == "a/b/c/d/e/f/g/h/i/j/k/very_deep.rs"),
            "unlimited walk should reach very deep files: {unlimited:?}"
        );
    }

    #[test]
    fn picker_skips_generated_worktree_bulk_inside_unignored_dot_dirs() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        fs::create_dir_all(root.join(".deepseek/commands")).unwrap();
        fs::write(root.join(".deepseek/commands/build.md"), "build").unwrap();
        fs::create_dir_all(root.join(".deepseek/snapshots/deadbeef/.git/objects")).unwrap();
        fs::write(
            root.join(".deepseek/snapshots/deadbeef/.git/objects/snapshot.pack"),
            "pack",
        )
        .unwrap();

        fs::create_dir_all(root.join(".claude/commands")).unwrap();
        fs::write(root.join(".claude/commands/test.md"), "test").unwrap();
        fs::create_dir_all(root.join(".claude/worktrees/agent/src")).unwrap();
        fs::write(
            root.join(".claude/worktrees/agent/src/agent-only.md"),
            "agent",
        )
        .unwrap();

        let candidates = collect_candidates(root, Some(WALK_DEPTH));

        assert!(candidates.iter().any(|path| path == "src/main.rs"));
        assert!(
            candidates
                .iter()
                .any(|path| path == ".deepseek/commands/build.md"),
            "normal .deepseek command files should stay discoverable: {candidates:?}",
        );
        assert!(
            candidates
                .iter()
                .any(|path| path == ".claude/commands/test.md"),
            "normal .claude command files should stay discoverable: {candidates:?}",
        );
        assert!(
            candidates
                .iter()
                .all(|path| !path.starts_with(".deepseek/snapshots/")),
            "snapshot side repo files must not enter picker candidates: {candidates:?}",
        );
        assert!(
            candidates
                .iter()
                .all(|path| !path.starts_with(".claude/worktrees/")),
            ".claude worktree files must not enter picker candidates: {candidates:?}",
        );
    }

    #[test]
    fn collect_candidates_limited_stops_at_the_cap_and_flags_truncation() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("pad")).unwrap();
        for i in 0..30 {
            fs::write(root.join("pad").join(format!("n{i:02}.txt")), "").unwrap();
        }

        let walk = collect_candidates_limited(root, Some(WALK_DEPTH), 12);
        assert!(
            walk.truncated,
            "hitting the cap must mark the index incomplete"
        );
        assert_eq!(walk.paths.len(), 12);
        assert!(
            !collect_candidates_limited(root, Some(WALK_DEPTH), 64).truncated,
            "a cap above the file count is a complete index"
        );
    }

    #[test]
    fn targeted_rescan_finds_a_file_the_candidate_cap_dropped() {
        // #2488: the opening walk keeps the first N files in walk order. A
        // later unique file must still be reachable once the user types it.
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("pad")).unwrap();
        for i in 0..40 {
            fs::write(root.join("pad").join(format!("n{i:02}.txt")), "").unwrap();
        }
        fs::create_dir_all(root.join("zzz")).unwrap();
        fs::write(root.join("zzz/room_chat_shell.dart"), "late").unwrap();

        let walk = collect_candidates_limited(root, Some(WALK_DEPTH), 15);
        assert!(walk.truncated);
        let hits = collect_query_matches(root, Some(WALK_DEPTH), "room_chat_shell", 64);
        assert!(
            hits.iter().any(|path| path == "zzz/room_chat_shell.dart"),
            "targeted rescan must recover the file past the cap: {hits:?}"
        );
    }

    #[test]
    fn targeted_walk_root_descends_into_an_existing_prefix() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::write(root.join("src/nested/hit.rs"), "").unwrap();
        let (start, depth) = targeted_walk_root(root, "src/nested/hit", Some(10));
        assert_eq!(start, root.join("src/nested"));
        assert_eq!(depth, Some(8));
    }

    #[test]
    fn picker_query_miss_rescans_a_truncated_index() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("zzz")).unwrap();
        fs::write(root.join("zzz/room_chat_shell.dart"), "late").unwrap();

        let mut view = FilePickerView::from_preloaded(
            root,
            vec!["pad/n00.txt".into(), "pad/n01.txt".into()],
            true,
            Some(WALK_DEPTH),
        );
        assert_eq!(
            view.visible_count(),
            2,
            "empty query shows the truncated index"
        );

        for ch in "room_chat_shell".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(
            view.selected_for_test(),
            Some("zzz/room_chat_shell.dart"),
            "a miss against the truncated index must rescan and surface the file"
        );
    }

    #[test]
    fn picker_complete_index_miss_does_not_rescan() {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("keep.txt"), "").unwrap();
        // A file on disk that is not in the (complete) index must stay
        // invisible — a complete walk already saw the whole tree.
        fs::write(root.join("secret.txt"), "").unwrap();

        let mut view =
            FilePickerView::from_preloaded(root, vec!["keep.txt".into()], false, Some(WALK_DEPTH));
        for ch in "secret".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(view.visible_count(), 0);
        assert_eq!(view.candidates, vec!["keep.txt".to_string()]);
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    #[test]
    fn file_picker_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        use ratatui::{buffer::Buffer, layout::Rect};
        use unicode_width::UnicodeWidthStr;

        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();

        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(FilePickerView::new_with_relevance(
                root,
                FilePickerRelevance::default(),
            ));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            for label in ["move", "insert @path", "cancel"] {
                assert!(text.contains(label), "{w}x{h}: missing footer '{label}'");
            }
            assert!(
                !text.contains('X'),
                "{w}x{h}: background bleed-through into modal surface"
            );
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    /// #3905: opening the picker used to block the event loop on a `git status`
    /// subprocess plus a walk of up to MAX_CANDIDATES paths, freezing the whole
    /// TUI between Ctrl+P and the picker appearing.
    ///
    /// Asserting "fast" by wall clock would be a flaky proxy for the real
    /// contract, so this asserts the structural property instead: inside a
    /// runtime the constructor returns a paintable view that has not yet done
    /// the scan, and the results arrive later through `tick`.
    #[tokio::test]
    async fn opening_the_picker_does_not_block_on_the_workspace_scan() {
        let ws = TempDir::new().unwrap();
        fs::create_dir_all(ws.path().join("src")).unwrap();
        for i in 0..200 {
            fs::write(ws.path().join("src").join(format!("f{i}.rs")), "x").unwrap();
        }

        let mut view = FilePickerView::new_with_relevance_and_depth(
            ws.path(),
            FilePickerRelevance::default(),
            WALK_DEPTH,
            Locale::En,
        );

        assert!(
            view.is_loading,
            "the constructor must hand back a paintable view, not a finished scan"
        );
        assert!(
            view.candidates.is_empty(),
            "no walk may have run on the calling thread"
        );

        // The view is renderable in the loading state — this is the frame the
        // user sees immediately after Ctrl+P.
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        for _ in 0..500 {
            view.tick();
            if !view.is_loading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert!(!view.is_loading, "the background scan must land via tick");
        assert_eq!(
            view.candidates.len(),
            200,
            "every workspace file is discovered once the scan lands"
        );
        assert_eq!(
            view.filtered.len(),
            200,
            "results are refiltered after the scan, not left empty"
        );
    }

    /// A query typed while the scan was still running must survive it.
    #[tokio::test]
    async fn a_query_typed_during_the_scan_is_applied_when_results_land() {
        let ws = TempDir::new().unwrap();
        fs::write(ws.path().join("alpha.rs"), "x").unwrap();
        fs::write(ws.path().join("beta.rs"), "x").unwrap();

        let mut view = FilePickerView::new_with_relevance_and_depth(
            ws.path(),
            FilePickerRelevance::default(),
            WALK_DEPTH,
            Locale::En,
        );
        assert!(view.is_loading);

        for ch in "alpha".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        for _ in 0..500 {
            view.tick();
            if !view.is_loading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert!(!view.is_loading);
        assert_eq!(view.query, "alpha");
        let matched: Vec<&str> = view
            .filtered
            .iter()
            .map(|i| view.candidates[*i].as_str())
            .collect();
        assert_eq!(
            matched,
            vec!["alpha.rs"],
            "the scan must refilter against the query the user already typed"
        );
    }

    /// #2488: a miss-rescan on a truncated index must not run on the event
    /// loop. The constructor-style property from #3905 applies here too:
    /// `handle_key` returns a paintable view and the extra file arrives via
    /// `tick`.
    #[tokio::test]
    async fn truncated_index_rescan_does_not_block_handle_key() {
        let ws = TempDir::new().unwrap();
        fs::write(ws.path().join("late_unique_file.rs"), "x").unwrap();

        let mut view = FilePickerView::from_preloaded(
            ws.path(),
            vec!["unrelated.rs".into()],
            true,
            Some(WALK_DEPTH),
        );
        for ch in "late_unique_file".chars() {
            view.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(
            view.is_rescanning
                || view
                    .candidates
                    .iter()
                    .any(|path| path == "late_unique_file.rs"),
            "rescan must start off-thread (or already have merged on a tiny race)"
        );

        for _ in 0..500 {
            view.tick();
            if view
                .candidates
                .iter()
                .any(|path| path == "late_unique_file.rs")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(view.selected_for_test(), Some("late_unique_file.rs"));
    }
}

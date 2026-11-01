//! Full-screen live transcript overlay with sticky-bottom auto-scroll (#94).
//!
//! Toggled with `Ctrl+Shift+T` while the engine is streaming (`Ctrl+T` now
//! cycles reasoning effort). Behaviour:
//!
//! - At-bottom (`sticky_to_bottom = true`) — every refresh re-pins scroll to
//!   the new tail, so streaming output appears to flow off the bottom edge.
//! - Scroll up — `sticky_to_bottom` flips to `false`; subsequent refreshes
//!   leave scroll position alone so the user can read history without being
//!   yanked back down.
//! - Scroll back to bottom (End / G / paging past the tail) — `sticky` flips
//!   to `true` again; auto-tail resumes.
//! - Esc / `q` — close, returning to the normal view. The engine never
//!   pauses while the overlay is open; new chunks accumulate in the cells
//!   exactly as they would on the normal screen.
//!
//! Cache strategy: the overlay holds its own `TranscriptCache` keyed by
//! `(CellId, width, revision)`. Revisions come from the same per-cell
//! counters the main transcript already maintains (`App.history_revisions`
//! and `App.active_cell_revision`). Resize invalidates the cells whose width
//! key just changed; revision bumps invalidate only the cells that mutated;
//! cells that didn't change reuse their existing wrap.

use std::cell::{Cell, RefCell};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap},
};

use crate::tui::app::App;
use crate::tui::backtrack::Direction;
use crate::tui::history::{HistoryCell, TranscriptRenderOptions};
use crate::tui::transcript_cache::{CachedTranscriptLine, CellId, TranscriptCache};
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
};
use codewhale_palette as palette;

/// Render mode for the overlay. `Tail` is the original sticky-tail
/// behaviour (#94). `BacktrackPreview` (#133) highlights the Nth-from-tail
/// `HistoryCell::User` so the user can see which turn Esc-Esc-Enter will
/// roll back to. The mode also disables sticky-tail (we want the user to
/// scan history, not be yanked to live output) and pins scroll near the
/// highlighted cell on transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Tail,
    BacktrackPreview {
        selected_idx: usize,
    },
}

/// Snapshot of one cell, refreshed every frame from `App`. Owns the cell so
/// the overlay's `render(&self)` can wrap without re-borrowing `App`.
#[derive(Debug, Clone)]
struct CellSnapshot {
    id: CellId,
    revision: u64,
    cell: HistoryCell,
}

struct FlattenedTranscript {
    lines: Vec<Line<'static>>,
    line_links: Vec<Vec<codewhale_tui_kit::osc8::LineLink>>,
    highlighted_range: Option<(usize, usize)>,
}

pub struct LiveTranscriptOverlay {
    /// Latest cell snapshots (history + active). Refreshed via
    /// `refresh_from_app` immediately before each render so streaming
    /// mutations show up on the next paint.
    snapshots: Vec<CellSnapshot>,
    /// Render options sampled from `App` at refresh time so toggles like
    /// `show_thinking` propagate into the overlay live.
    options: TranscriptRenderOptions,
    /// Wrapped-line cache. `RefCell` so `render(&self)` can write through.
    cache: RefCell<TranscriptCache>,
    /// Sticky-tail flag: when `true`, refresh re-pins scroll to the bottom.
    /// Flipped to `false` when the user scrolls up; flipped back to `true`
    /// when they scroll past the last visible line.
    sticky_to_bottom: Cell<bool>,
    /// Current top-of-viewport line offset into the flattened line list.
    scroll: Cell<usize>,
    /// Visible content height from the last render. Used by paging keys
    /// before the next render frame populates a fresh value.
    last_visible_height: Cell<usize>,
    /// Last total line count after wrapping; cached so `handle_key` can
    /// clamp scroll without re-wrapping. Updated by `render`.
    last_total_lines: Cell<usize>,
    /// Pending `gg` second keystroke for Vim-style jump-to-top.
    pending_g: bool,
    /// Render mode — `Tail` is the live-stream mode; `BacktrackPreview`
    /// highlights the selected user message (#133).
    mode: Mode,
    /// Set when a backtrack selection changes. The next render pins the
    /// selected cell into view once we know the wrapped line range.
    preview_pin_pending: Cell<bool>,
    /// Bumped by `refresh_from_app` whenever any snapshot actually changed.
    /// Lets the flatten cache below be keyed in O(1) (#3904).
    snapshots_generation: Cell<u64>,
    /// Cached flattened line vector. `flatten` used to re-clone every cell's
    /// cached wrapped lines into a fresh `Vec` on every frame; now it is
    /// rebuilt only when the snapshots, width, mode, or render options change.
    flat_cache: RefCell<Option<FlatCache>>,
    /// How many `HistoryCell` deep clones `refresh_from_app` has performed.
    /// Diagnostics + the #3904 regression tests.
    cell_clones: Cell<u64>,
    /// How many times `flatten` actually rebuilt the flat line vector.
    flattens: Cell<u64>,
}

/// Key that fully determines the output of `flatten`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlatKey {
    generation: u64,
    width: u16,
    mode: Mode,
    options: TranscriptRenderOptions,
}

struct FlatCache {
    key: FlatKey,
    flat: FlattenedTranscript,
}

impl LiveTranscriptOverlay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshots: Vec::new(),
            options: TranscriptRenderOptions::default(),
            cache: RefCell::new(TranscriptCache::new()),
            sticky_to_bottom: Cell::new(true),
            scroll: Cell::new(0),
            last_visible_height: Cell::new(0),
            last_total_lines: Cell::new(0),
            pending_g: false,
            mode: Mode::Tail,
            preview_pin_pending: Cell::new(false),
            snapshots_generation: Cell::new(0),
            flat_cache: RefCell::new(None),
            cell_clones: Cell::new(0),
            flattens: Cell::new(0),
        }
    }

    /// Switch the overlay into backtrack-preview mode. Sticky-tail is
    /// turned off so the highlighted cell stays in view while the user
    /// steps through prior turns. The wrap cache stays valid because the
    /// underlying snapshot data hasn't changed — only the post-wrap
    /// highlight overlay does.
    pub fn set_backtrack_preview(&mut self, selected_idx: usize) {
        self.mode = Mode::BacktrackPreview { selected_idx };
        self.sticky_to_bottom.set(false);
        self.preview_pin_pending.set(true);
    }

    /// Return the overlay to live-tail mode (used when backtrack is
    /// confirmed or canceled). Re-arms sticky-tail so streaming resumes.
    pub fn set_tail_mode(&mut self) {
        self.mode = Mode::Tail;
        self.sticky_to_bottom.set(true);
        self.preview_pin_pending.set(false);
    }

    /// For tests + UI: current mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Pull the latest cells + revisions from `App` so the next `render` shows
    /// streaming mutations. Must be called before `view_stack.render` while
    /// this overlay is on top; otherwise the cells stay frozen at whatever
    /// state they were in when the overlay was first opened.
    pub fn refresh_from_app(&mut self, app: &mut App) {
        app.resync_history_revisions();
        let mut slot = 0usize;
        let mut changed = false;
        for (idx, cell) in app.history.iter().enumerate() {
            let rev = app.history_revisions.get(idx).copied().unwrap_or(0);
            changed |= self.store_snapshot(slot, CellId::History(idx), rev, cell);
            slot += 1;
        }
        if let Some(active) = app.active_cell.as_ref() {
            let active_rev = app.active_cell_revision;
            for (idx, cell) in active.entries().iter().enumerate() {
                let salt = (idx as u64).wrapping_add(1);
                // Share the main transcript's revision derivation instead of
                // keeping an inline copy of the same mixing constant (#3904).
                let revision = crate::tui::widgets::active_entry_revision(active_rev, salt);
                changed |= self.store_snapshot(slot, CellId::Active(idx), revision, cell);
                slot += 1;
            }
        }
        if self.snapshots.len() != slot {
            self.snapshots.truncate(slot);
            changed = true;
        }
        let options = app.transcript_render_options();
        if self.options != options {
            self.options = options;
            // Per-cell wraps are keyed by (cell, width, revision) only, so a
            // theme, locale or thinking toggle would otherwise reuse lines
            // rendered under the old options (U04-m1).
            self.cache.get_mut().clear();
            changed = true;
        }
        if changed {
            self.snapshots_generation
                .set(self.snapshots_generation.get().wrapping_add(1));
        }
    }

    /// Write the snapshot for `slot`, cloning the cell **only** when its
    /// `(CellId, revision)` differs from what is already stored (#3904).
    ///
    /// Returns whether anything changed. The overlay tails a live stream, so
    /// this runs at streaming cadence; the old code deep-cloned every history
    /// cell every frame even though revisions were already available.
    fn store_snapshot(
        &mut self,
        slot: usize,
        id: CellId,
        revision: u64,
        cell: &HistoryCell,
    ) -> bool {
        match self.snapshots.get_mut(slot) {
            Some(existing) if existing.id == id && existing.revision == revision => false,
            Some(existing) => {
                existing.id = id;
                existing.revision = revision;
                existing.cell = cell.clone();
                self.cell_clones.set(self.cell_clones.get() + 1);
                true
            }
            None => {
                self.snapshots.push(CellSnapshot {
                    id,
                    revision,
                    cell: cell.clone(),
                });
                self.cell_clones.set(self.cell_clones.get() + 1);
                true
            }
        }
    }

    /// Wrap each cell (using the cache) and return the flat line vector.
    /// In `BacktrackPreview` mode the lines belonging to the selected
    /// `HistoryCell::User` are decorated with a leading `▶` marker on the
    /// first line and reverse-video styling on every line so the eye
    /// snaps to them at a glance. The decoration is applied *after* the
    /// cache lookup so toggling preview mode never invalidates wraps.
    /// Cached wrapper around [`Self::flatten`] (#3904).
    ///
    /// The overlay renders at streaming cadence while the main transcript
    /// renders underneath in the same frame, so re-flattening the whole
    /// transcript per frame roughly doubled per-frame allocations. The flat
    /// vector only depends on the snapshots, the width, the mode, and the
    /// render options — all of which are in `FlatKey`.
    fn flattened(&self, width: u16) -> std::cell::Ref<'_, FlattenedTranscript> {
        let key = FlatKey {
            generation: self.snapshots_generation.get(),
            width: width.max(1),
            mode: self.mode,
            options: self.options,
        };
        {
            let mut cache = self.flat_cache.borrow_mut();
            let stale = cache.as_ref().is_none_or(|cached| cached.key != key);
            if stale {
                let flat = self.flatten(key.width);
                self.flattens.set(self.flattens.get() + 1);
                *cache = Some(FlatCache { key, flat });
            }
        }
        std::cell::Ref::map(self.flat_cache.borrow(), |cache| {
            &cache.as_ref().expect("flat cache populated above").flat
        })
    }

    fn flatten(&self, width: u16) -> FlattenedTranscript {
        let width = width.max(1);
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut out_links: Vec<Vec<codewhale_tui_kit::osc8::LineLink>> = Vec::new();
        let mut highlighted_range = None;

        // Pre-compute which cell index (in `self.snapshots`) is the one
        // the user has selected via Esc-Esc. We walk snapshots backwards
        // counting User cells; the snapshot index whose count matches
        // `selected_idx + 1` is the highlighted one.
        let highlighted_cell_idx: Option<usize> = match self.mode {
            Mode::BacktrackPreview { selected_idx } => {
                let mut count = 0usize;
                let mut hit = None;
                for (idx, snap) in self.snapshots.iter().enumerate().rev() {
                    if matches!(snap.cell, HistoryCell::User { .. }) {
                        if count == selected_idx {
                            hit = Some(idx);
                            break;
                        }
                        count += 1;
                    }
                }
                hit
            }
            Mode::Tail => None,
        };

        let mut cache = self.cache.borrow_mut();
        // Room for every cell plus a new revision of each, so the sweep
        // below never evicts entries it is about to read (a 1,000-cell
        // transcript against the old fixed 512 cap missed every lookup).
        cache.ensure_capacity(self.snapshots.len().saturating_mul(2));
        for (cell_idx, snap) in self.snapshots.iter().enumerate() {
            // Borrow the cached slice and clone each line (and its links)
            // exactly once into the flattened output.
            let split = |cached: &[CachedTranscriptLine]| -> (Vec<Line<'static>>, Vec<_>) {
                cached
                    .iter()
                    .map(|rendered| (rendered.line.clone(), rendered.links.clone()))
                    .unzip()
            };
            let (lines, mut line_links) = match cache.get(snap.id, width, snap.revision) {
                Some(cached) => split(cached),
                None => {
                    let rendered = snap
                        .cell
                        .lines_with_copy_metadata(width, self.options)
                        .into_iter()
                        .map(|rendered| CachedTranscriptLine {
                            line: rendered.line,
                            links: rendered.links,
                        })
                        .collect::<Vec<_>>();
                    let split_lines = split(&rendered);
                    cache.insert(snap.id, width, snap.revision, rendered);
                    split_lines
                }
            };

            if Some(cell_idx) == highlighted_cell_idx {
                let start = out.len();
                let lines = decorate_highlight(lines);
                if let Some(first_links) = line_links.first_mut() {
                    *first_links = first_links.iter().map(|link| link.shifted(2)).collect();
                }
                out.extend(lines);
                out_links.extend(line_links);
                let end = out.len();
                if end > start {
                    highlighted_range = Some((start, end));
                }
            } else {
                out.extend(lines);
                out_links.extend(line_links);
            }
        }
        FlattenedTranscript {
            lines: out,
            line_links: out_links,
            highlighted_range,
        }
    }

    fn page_height(&self) -> usize {
        let cached = self.last_visible_height.get();
        if cached == 0 { 10 } else { cached }
    }

    fn half_page_height(&self) -> usize {
        self.page_height().div_ceil(2).max(1)
    }

    fn max_scroll(&self) -> usize {
        let total = self.last_total_lines.get();
        let visible = self.page_height();
        total.saturating_sub(visible)
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll.set(self.scroll.get().saturating_sub(amount));
        // Any upward motion exits sticky-tail; explicit user intent.
        self.sticky_to_bottom.set(false);
        self.preview_pin_pending.set(false);
    }

    fn scroll_down(&mut self, amount: usize) {
        let max = self.max_scroll();
        let scroll = self.scroll.get().saturating_add(amount).min(max);
        self.scroll.set(scroll);
        self.preview_pin_pending.set(false);
        if scroll >= max && matches!(self.mode, Mode::Tail) {
            self.sticky_to_bottom.set(true);
        }
    }

    fn jump_to_top(&mut self) {
        self.scroll.set(0);
        self.sticky_to_bottom.set(false);
        self.preview_pin_pending.set(false);
    }

    fn jump_to_bottom(&mut self) {
        self.scroll.set(self.max_scroll());
        self.sticky_to_bottom.set(matches!(self.mode, Mode::Tail));
        self.preview_pin_pending.set(false);
    }

    /// For tests: snapshot count.
    #[cfg(test)]
    fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// For tests: whether sticky-tail is currently armed.
    #[cfg(test)]
    pub fn is_sticky(&self) -> bool {
        self.sticky_to_bottom.get()
    }

    /// For tests: current scroll offset.
    #[cfg(test)]
    pub fn scroll_offset(&self) -> usize {
        self.scroll.get()
    }
}

impl Default for LiveTranscriptOverlay {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply a backtrack-preview highlight to the lines belonging to a single
/// `HistoryCell::User`. The first line gets a `▶ ` prefix in accent color
/// (so the marker remains visible even on terminals where reverse-video
/// is washed out); every line in the cell gets `Modifier::REVERSED` so
/// the cell visually pops out of the surrounding transcript. Internal
/// span structure is preserved so syntax/role coloring underneath the
/// reverse stays readable.
fn decorate_highlight(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return lines;
    }
    for line in &mut lines {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::REVERSED);
        }
    }
    let marker = Span::styled(
        "\u{25B6} ",
        Style::default()
            .fg(palette::TEXT_ACCENT)
            .add_modifier(Modifier::BOLD),
    );
    if let Some(first) = lines.first_mut() {
        first.spans.insert(0, marker);
    }
    lines
}

fn scroll_to_show_range(
    current: usize,
    start: usize,
    end: usize,
    visible_height: usize,
    max_scroll: usize,
) -> usize {
    if visible_height == 0 {
        return 0;
    }
    let end = end.max(start.saturating_add(1));
    if start < current {
        start.min(max_scroll)
    } else if end > current.saturating_add(visible_height) {
        end.saturating_sub(visible_height).min(max_scroll)
    } else {
        current.min(max_scroll)
    }
}

impl ModalView for LiveTranscriptOverlay {
    fn kind(&self) -> ModalKind {
        ModalKind::LiveTranscript
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Backtrack-preview mode (#133) intercepts Left/Right/Enter/Esc
        // before the normal scroll handlers so the user can step through
        // prior user messages without their input being interpreted as
        // pager navigation. Other keys (page up/down, gg/G, etc.) still
        // fall through so the user can scroll the transcript while
        // previewing.
        if matches!(self.mode, Mode::BacktrackPreview { .. }) {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') if !ctrl => {
                    return ViewAction::Emit(ViewEvent::BacktrackStep {
                        direction: Direction::Left,
                    });
                }
                KeyCode::Right | KeyCode::Char('l') if !ctrl => {
                    return ViewAction::Emit(ViewEvent::BacktrackStep {
                        direction: Direction::Right,
                    });
                }
                KeyCode::Enter => {
                    return ViewAction::EmitAndClose(ViewEvent::BacktrackConfirm);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    return ViewAction::EmitAndClose(ViewEvent::BacktrackCancel);
                }
                _ => {}
            }
        }

        if ctrl {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.scroll_down(self.half_page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    self.scroll_up(self.half_page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.scroll_down(self.page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.scroll_up(self.page_height());
                    self.pending_g = false;
                    return ViewAction::None;
                }
                // Ctrl+Shift+T toggles the overlay closed when already open.
                KeyCode::Char('t') | KeyCode::Char('T')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    return ViewAction::Close;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_up(1);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_down(1);
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::PageUp => {
                self.scroll_up(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::PageDown => {
                self.scroll_down(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char(' ') if shift => {
                self.scroll_up(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char(' ') => {
                self.scroll_down(self.page_height());
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Home => {
                self.jump_to_top();
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::End => {
                self.jump_to_bottom();
                self.pending_g = false;
                ViewAction::None
            }
            KeyCode::Char('g') => {
                if self.pending_g {
                    self.jump_to_top();
                    self.pending_g = false;
                } else {
                    self.pending_g = true;
                }
                ViewAction::None
            }
            KeyCode::Char('G') => {
                self.jump_to_bottom();
                self.pending_g = false;
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup_width = area.width.saturating_sub(2).max(1);
        let popup_height = area.height.saturating_sub(2).max(1);
        let popup_area = Rect {
            x: 1,
            y: 1,
            width: popup_width,
            height: popup_height,
        };

        Clear.render(popup_area, buf);

        let title: String = match self.mode {
            Mode::BacktrackPreview { selected_idx } => format!(
                " Backtrack preview — turn {} (\u{2190}/\u{2192} step, Enter rewind, Esc cancel) ",
                selected_idx + 1
            ),
            Mode::Tail => {
                if self.sticky_to_bottom.get() {
                    " Live transcript (tailing) ".to_string()
                } else {
                    " Live transcript (paused) ".to_string()
                }
            }
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Wrapping action footer along the bottom of the inner area; the body
        // fills the rows above it.
        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("j/k", "scroll"),
                ActionHint::new("Space/C-b", "page"),
                ActionHint::new("g/G", "top/bottom"),
                ActionHint::new("End", "resume tail"),
                ActionHint::new("q/Esc", "close"),
            ],
        );

        // `content` already excludes the border, padding, and footer rows.
        let visible_height = content.height as usize;
        self.last_visible_height.set(visible_height);

        // Wrap content using the per-cell cache at the body width.
        let content_width = content.width;
        let flat = self.flattened(content_width);
        let FlattenedTranscript {
            lines,
            line_links,
            highlighted_range,
        } = &*flat;
        let highlighted_range = *highlighted_range;
        self.last_total_lines.set(lines.len());

        let max_scroll = lines.len().saturating_sub(visible_height);
        // Sticky-tail: every render re-pins scroll to the bottom unless the
        // user has explicitly scrolled away. Without this, streaming new
        // content would push the visible window backwards as `scroll` stays
        // fixed against a growing total.
        let scroll = if self.sticky_to_bottom.get() {
            self.scroll.set(max_scroll);
            max_scroll
        } else if self.preview_pin_pending.replace(false) {
            let next = highlighted_range
                .map(|(start, end)| {
                    scroll_to_show_range(self.scroll.get(), start, end, visible_height, max_scroll)
                })
                .unwrap_or_else(|| self.scroll.get().min(max_scroll));
            self.scroll.set(next);
            next
        } else {
            let next = self.scroll.get().min(max_scroll);
            self.scroll.set(next);
            next
        };
        let end = (scroll + visible_height).min(lines.len());
        let visible_lines: Vec<Line<'static>> = if lines.is_empty() {
            vec![Line::from(Span::styled(
                "(no transcript yet)",
                Style::default().fg(palette::TEXT_DIM),
            ))]
        } else {
            lines[scroll..end].to_vec()
        };
        let visible_line_links = if lines.is_empty() {
            vec![Vec::new()]
        } else {
            line_links[scroll..end].to_vec()
        };

        let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
        paragraph.render(content, buf);

        // Targets stay beside the visible lines, so the popup never writes an
        // escape payload into the buffer. Replace the opaque popup's portion
        // of the frame map so links in the obscured transcript cannot leak
        // through unrelated modal text.
        let regions = codewhale_tui_kit::osc8::link_regions_for_lines(content, &visible_line_links);
        codewhale_tui_kit::osc8::overlay_frame_links(popup_area, regions);
    }
}

// Frozen production source boundary.

impl LiveTranscriptOverlay {
    pub(super) fn frozen_snapshot_count(&self) -> usize {
        self.snapshot_count()
    }
    pub(super) fn frozen_metrics(&self) -> (usize, usize, usize, bool) {
        (
            self.scroll.get(),
            self.last_visible_height.get(),
            self.last_total_lines.get(),
            self.sticky_to_bottom.get(),
        )
    }
}

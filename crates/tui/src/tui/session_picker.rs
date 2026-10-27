//! Session resume picker view for the TUI.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::session_manager::{
    SavedSession, SessionListFilter, SessionManager, SessionMetadata, extract_title,
    extract_user_prompt, strip_thinking_tags,
};
use crate::session_projection::{MAX_PROJECTED_SESSIONS, SessionQuery, SessionSortMode};
use crate::tui::menu_style;
use crate::tui::views::{
    ActionHint, action_footer_lines, render_modal_footer, render_panel_scroll_rail,
    render_underwater_surface,
};
use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::Role;
use codewhale_palette as palette;

fn section_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Line::from(vec![Span::styled(
            title.to_string(),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(palette::BORDER_COLOR))
        .style(Style::default().bg(palette::WHALE_BG))
        .padding(Padding::uniform(1))
}

/// Previews kept in memory. Each entry is a whole rendered transcript, so the
/// cache is bounded: scrolling a long list must not retain every session.
const PREVIEW_CACHE_CAPACITY: usize = 16;

/// Small least-recently-used cache of rendered previews keyed by session id.
#[derive(Default)]
struct PreviewCache {
    /// Oldest first; a hit moves its entry to the back.
    entries: VecDeque<(String, Vec<String>)>,
}

impl PreviewCache {
    fn get(&mut self, id: &str) -> Option<&Vec<String>> {
        let index = self.entries.iter().position(|(key, _)| key == id)?;
        let entry = self.entries.remove(index)?;
        self.entries.push_back(entry);
        self.entries.back().map(|(_, lines)| lines)
    }

    fn remove(&mut self, id: &str) {
        self.entries.retain(|(key, _)| key != id);
    }

    fn insert(&mut self, id: String, lines: Vec<String>) {
        self.entries.retain(|(key, _)| *key != id);
        self.entries.push_back((id, lines));
        while self.entries.len() > PREVIEW_CACHE_CAPACITY {
            self.entries.pop_front();
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Outcome of reading one session from disk for the preview pane.
struct PreviewLoad {
    lines: Vec<String>,
    /// Only a successful load is cached; a failure is retried on reselect.
    cacheable: bool,
}

/// A preview load running off the event loop. The result is only applied if
/// it still matches the selection that requested it.
struct PendingPreview {
    session_id: String,
    generation: u64,
    cell: Arc<Mutex<Option<PreviewLoad>>>,
}

/// One session-store listing, or why the store could not be listed.
type SessionListing = Result<Vec<SessionMetadata>, String>;

/// The session-store listing running off the event loop (U08-09). The
/// store's metadata scan grows with every saved session, so the picker opens
/// at once and fills in when the listing lands.
struct PendingSessionList {
    cell: Arc<Mutex<Option<SessionListing>>>,
    /// Row to land on once the list arrives (`new_selecting`).
    select: Option<String>,
}

/// What the list pane says in place of rows it does not have yet.
#[derive(Debug, Clone, Copy)]
enum ListNotice<'a> {
    Loading,
    Failed(&'a str),
}

pub struct SessionPickerView {
    /// Every session loaded from disk. The picker filters from this set.
    sessions: Vec<SessionMetadata>,
    /// The store listing still in flight, if any.
    pending_list: Option<PendingSessionList>,
    /// The store could not be listed. Shown instead of "no saved sessions",
    /// which would claim an empty store nobody observed.
    load_error: Option<String>,
    filtered: Vec<SessionMetadata>,
    selected: usize,
    list_scroll: Cell<usize>,
    list_visible_rows: Cell<usize>,
    history_scroll: Cell<usize>,
    history_pinned_to_latest: Cell<bool>,
    history_visible_rows: Cell<usize>,
    search_input: String,
    search_mode: bool,
    sort_mode: SessionSortMode,
    preview_cache: PreviewCache,
    current_preview: Vec<String>,
    /// Bumped on every preview refresh; a background load tagged with an
    /// older generation is stale and dropped.
    preview_generation: u64,
    pending_preview: Option<PendingPreview>,
    confirm_delete: bool,
    delete_target: Option<String>,
    rename_mode: bool,
    rename_input: String,
    status: Option<String>,
    /// Canonical workspace path used as the per-project scope filter
    /// (#1395). `None` opts out of scoping (e.g. when the caller can't
    /// resolve a workspace).
    workspace_scope: Option<PathBuf>,
    /// When `true`, the picker shows sessions from every workspace; when
    /// `false`, only sessions whose recorded `workspace` matches the
    /// canonicalised `workspace_scope`.
    show_all_workspaces: bool,
    /// When `true`, archived sessions are listed alongside active ones
    /// (#2934 / #4397). Defaults to `false`: archiving is the user putting a
    /// session away, and the browse default should honour that.
    show_archived: bool,
    /// Hide empty auto-created sessions — the same filter the launch list
    /// and `--continue` already apply (#6014). The picker exists to resume
    /// work, and a session with no messages has none to resume.
    hide_empty_sessions: bool,
    /// The session the caller is running in, when it hands one over. Rows
    /// mark it `current` so the user can tell which session they are inside
    /// (#6014). Never inferred from disk state.
    current_session_id: Option<String>,
    /// Screen rows owned by the visible session list. Keeping this local to
    /// the view gives mouse and keyboard the same selection/resume contract.
    last_row_hitboxes: RefCell<Vec<(u16, usize)>>,
    /// UI locale captured from the app at construction (#4057 wave 2).
    locale: Locale,
}

impl SessionPickerView {
    /// Construct a picker scoped to `workspace`. Sessions belonging to
    /// other workspaces are hidden by default — press `a` inside the
    /// picker to expand to all workspaces (#1395).
    pub fn new(workspace: &Path, locale: Locale) -> Self {
        Self::load(workspace, locale, None)
    }

    /// Open over the session store. Listing it is blocking disk I/O that
    /// grows with the store, so inside a runtime it runs on the blocking pool
    /// and lands through `tick`; outside one (unit tests, headless callers)
    /// there is no loop to stall, so it loads inline.
    fn load(workspace: &Path, locale: Locale, select: Option<String>) -> Self {
        let mut view = Self::from_session_list(workspace, locale, Vec::new());
        if tokio::runtime::Handle::try_current().is_err() {
            view.apply_session_list(list_session_store(), select);
            return view;
        }
        let cell = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&cell);
        codewhale_util::utils::spawn_blocking_supervised("session-picker-list", move || {
            let listed = list_session_store();
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(listed);
            }
        });
        view.pending_list = Some(PendingSessionList { cell, select });
        view
    }

    /// Install a finished store listing. A failure is shown as a failure —
    /// never as an empty store — and the list stays empty.
    fn apply_session_list(&mut self, listed: SessionListing, select: Option<String>) {
        match listed {
            Ok(sessions) => {
                self.sessions = sessions;
                self.load_error = None;
                self.apply_sort_and_filter();
                if let Some(session_id) = select {
                    self.select_with_fallback(&session_id);
                }
            }
            Err(error) => {
                self.sessions.clear();
                self.load_error =
                    Some(tr(self.locale, MessageId::SessionsOpenFailed).replace("{error}", &error));
                self.apply_sort_and_filter();
            }
        }
    }

    /// Apply the store listing once it lands. Returns whether the view changed.
    fn poll_session_list(&mut self) -> bool {
        let Some(pending) = self.pending_list.as_ref() else {
            return false;
        };
        let Some(listed) = pending.cell.lock().ok().and_then(|mut guard| guard.take()) else {
            return false;
        };
        let select = self.pending_list.take().and_then(|pending| pending.select);
        self.apply_session_list(listed, select);
        true
    }

    /// Construct a picker scoped to `workspace` over an explicit session list.
    ///
    /// Test seam for acceptance coverage (#5929): production always resolves
    /// the store via [`Self::new`], but redirecting `CODEWHALE_HOME` to point
    /// that resolution at a temp directory is process-global state — a
    /// concurrent test that resolves the store without the env barrier can
    /// observe the redirect mid-test and write sessions into it, so the two
    /// listings under comparison drift apart. Handing the picker the list from
    /// a private store removes the shared-state window without touching what
    /// the picker does with the list.
    #[cfg(test)]
    pub fn new_with_sessions(
        workspace: &Path,
        locale: Locale,
        sessions: Vec<SessionMetadata>,
    ) -> Self {
        Self::from_session_list(workspace, locale, sessions)
    }

    /// Shared constructor: everything except where the session list came from.
    fn from_session_list(workspace: &Path, locale: Locale, sessions: Vec<SessionMetadata>) -> Self {
        let mut view = Self {
            sessions,
            pending_list: None,
            load_error: None,
            filtered: Vec::new(),
            selected: 0,
            list_scroll: Cell::new(0),
            list_visible_rows: Cell::new(8),
            history_scroll: Cell::new(0),
            history_pinned_to_latest: Cell::new(true),
            history_visible_rows: Cell::new(12),
            search_input: String::new(),
            search_mode: false,
            sort_mode: SessionSortMode::Recent,
            preview_cache: PreviewCache::default(),
            current_preview: Vec::new(),
            preview_generation: 0,
            pending_preview: None,
            confirm_delete: false,
            delete_target: None,
            rename_mode: false,
            rename_input: String::new(),
            status: None,
            workspace_scope: Some(canonical_or_self(workspace.to_path_buf())),
            show_all_workspaces: false,
            show_archived: false,
            hide_empty_sessions: true,
            current_session_id: None,
            last_row_hitboxes: RefCell::new(Vec::new()),
            locale,
        };
        view.apply_sort_and_filter();
        view.refresh_preview();
        view
    }

    /// As [`Self::new`], but with `session_id` preselected.
    ///
    /// This is how the sidebar Sessions rail hands off (#2934): the rail
    /// navigates, the picker keeps ownership of preview, resume, rename,
    /// archive, and delete. A row whose session is outside the current
    /// workspace scope, or archived, widens the corresponding filter rather
    /// than silently landing on the wrong row — but it never *resumes*
    /// anything, so widening the view cannot cross a workspace boundary
    /// behind the user's back.
    pub fn new_selecting(workspace: &Path, locale: Locale, session_id: &str) -> Self {
        Self::load(workspace, locale, Some(session_id.to_string()))
    }

    /// Land the selection on `session_id`, widening one browse filter at a
    /// time when the row is outside the default view.
    fn select_with_fallback(&mut self, session_id: &str) {
        if self.select_session_id(session_id) {
            return;
        }
        if !self.show_archived {
            self.show_archived = true;
            self.apply_sort_and_filter();
            if self.select_session_id(session_id) {
                self.status =
                    Some(tr(self.locale, MessageId::SessionsShowingArchived).into_owned());
                return;
            }
        }
        if !self.show_all_workspaces {
            self.show_all_workspaces = true;
            self.apply_sort_and_filter();
            if self.select_session_id(session_id) {
                self.status =
                    Some(tr(self.locale, MessageId::SessionsShowingAllWorkspaces).into_owned());
                return;
            }
        }
        // Last resort: an empty auto-created session is hidden by default
        // but a rail handoff may still target it — widen that filter too
        // rather than report "no results" for a session that exists (#6014).
        if self.hide_empty_sessions {
            self.hide_empty_sessions = false;
            self.apply_sort_and_filter();
            if self.select_session_id(session_id) {
                return;
            }
        }
        // Not found at all: leave the default view rather than pretending.
        self.status = Some(tr(self.locale, MessageId::SessionsNoResults).into_owned());
    }

    /// Mark `session_id` as the session the caller is running in. Rows mark
    /// it `current` (#6014); `None` leaves every row unmarked.
    #[must_use]
    pub fn with_current_session(mut self, session_id: Option<&str>) -> Self {
        self.current_session_id = session_id.map(str::to_string);
        self
    }

    /// Move the selection onto `session_id` if it is in the filtered list.
    fn select_session_id(&mut self, session_id: &str) -> bool {
        let Some(index) = self.filtered.iter().position(|s| s.id == session_id) else {
            return false;
        };
        self.selected = index;
        self.ensure_selected_visible();
        self.refresh_preview();
        true
    }

    /// The query this picker's current view represents.
    ///
    /// Built here and handed to [`crate::session_projection::select_sessions`]
    /// so the picker's list is literally the same selection the rail and
    /// `/v1/sessions` compute — filter, workspace scope, fuzzy search, sort,
    /// and tie-breaks included. The picker used to own private copies of all
    /// five; that was the second backend.
    fn view_query(&self) -> SessionQuery {
        let mut query = SessionQuery::default()
            .with_filter(self.archive_filter())
            .with_sort(self.sort_mode)
            .with_search(self.search_input.trim().to_string())
            .with_limit(MAX_PROJECTED_SESSIONS);
        if self.hide_empty_sessions {
            query = query.without_empty_auto_created();
        }
        if !self.show_all_workspaces
            && let Some(scope) = self.workspace_scope.as_deref()
        {
            query = query.scoped_to(scope);
        }
        query
    }

    /// Flip between current-workspace-only and all-workspaces view
    /// (#1395). Used by the `a` keybinding inside the picker; also
    /// callable from tests.
    pub fn toggle_all_workspaces(&mut self) {
        self.show_all_workspaces = !self.show_all_workspaces;
        let label = if self.show_all_workspaces {
            tr(self.locale, MessageId::SessionsShowingAllWorkspaces)
        } else {
            tr(self.locale, MessageId::SessionsScopedToWorkspace)
        };
        self.status = Some(label.into_owned());
        self.selected = 0;
        self.apply_sort_and_filter();
    }

    /// Which archive states the list currently admits.
    fn archive_filter(&self) -> SessionListFilter {
        if self.show_archived {
            SessionListFilter::IncludeArchived
        } else {
            SessionListFilter::ActiveOnly
        }
    }

    /// Ids currently visible in the list, top to bottom.
    ///
    /// Exposed so the shared acceptance matrix can compare the picker's real
    /// filtered view against the API's projection instead of comparing the
    /// projection to itself.
    #[cfg(test)]
    pub fn visible_session_ids(&self) -> Vec<String> {
        self.filtered.iter().map(|s| s.id.clone()).collect()
    }

    /// The query behind [`Self::visible_session_ids`].
    #[cfg(test)]
    pub fn view_query_for_test(&self) -> SessionQuery {
        self.view_query()
    }

    #[cfg(test)]
    pub fn cycle_sort_for_test(&mut self) {
        self.cycle_sort();
    }

    #[cfg(test)]
    pub fn set_search_for_test(&mut self, query: &str) {
        self.search_input = query.to_string();
        self.apply_sort_and_filter();
    }

    fn apply_sort_and_filter(&mut self) {
        let query = self.view_query();
        self.filtered = crate::session_projection::select_sessions(&self.sessions, &query)
            .into_iter()
            .cloned()
            .collect();

        if self.selected >= self.filtered.len() {
            self.selected = 0;
        }
        self.ensure_selected_visible();

        self.refresh_preview();
    }

    /// Apply one [`list_nav`](crate::tui::list_nav) motion (#6290), returning
    /// whether it was consumed. `Prev`/`Next` wrap (existing behavior);
    /// paging and Home/End clamp — a paging key asks to travel, not to
    /// teleport. The horizontal axis has nowhere to go on this surface.
    fn apply_motion(&mut self, motion: crate::tui::list_nav::Motion) -> bool {
        if self.filtered.is_empty() {
            return false;
        }
        let page = self.list_visible_rows.get().max(1);
        let Some(next) =
            crate::tui::list_nav::apply(self.selected, self.filtered.len(), page, motion)
        else {
            return false;
        };
        self.selected = next;
        self.ensure_selected_visible();
        self.refresh_preview();
        true
    }

    fn select_visible_shortcut(&mut self, c: char) -> bool {
        let Some(slot) = c.to_digit(10) else {
            return false;
        };
        if !(1..=9).contains(&slot) {
            return false;
        }
        let index = self.list_scroll.get().saturating_add(slot as usize - 1);
        if index >= self.filtered.len() {
            return false;
        }
        self.selected = index;
        self.ensure_selected_visible();
        self.refresh_preview();
        if let Some(session) = self.selected_session() {
            self.status = Some(
                tr(self.locale, MessageId::SessionsOpenedHistory)
                    .replace("{id}", crate::session_manager::truncate_id(&session.id)),
            );
        }
        true
    }

    fn update_list_viewport(&self, visible_rows: usize) {
        self.list_visible_rows.set(visible_rows.max(1));
        self.ensure_selected_visible();
    }

    fn update_history_viewport(&self, visible_rows: usize) {
        self.history_visible_rows.set(visible_rows.max(1));
        self.ensure_history_scroll_in_bounds();
    }

    fn scroll_history(&self, delta: isize) {
        let max_scroll =
            max_history_scroll_for(&self.current_preview, self.history_visible_rows.get());
        let current = self.history_scroll.get();
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize)
        };
        let next = next.min(max_scroll);
        self.history_scroll.set(next);
        self.history_pinned_to_latest.set(next == max_scroll);
    }

    fn ensure_history_scroll_in_bounds(&self) {
        let max_scroll =
            max_history_scroll_for(&self.current_preview, self.history_visible_rows.get());
        if self.history_pinned_to_latest.get() {
            self.history_scroll.set(max_scroll);
        } else {
            self.history_scroll
                .set(self.history_scroll.get().min(max_scroll));
        }
    }

    fn scroll_history_to_latest(&self) {
        let max_scroll =
            max_history_scroll_for(&self.current_preview, self.history_visible_rows.get());
        self.history_scroll.set(max_scroll);
        self.history_pinned_to_latest.set(true);
    }

    fn ensure_selected_visible(&self) {
        if self.filtered.is_empty() {
            self.list_scroll.set(0);
            return;
        }

        let visible_rows = self.list_visible_rows.get().max(1);
        let max_scroll = self.filtered.len().saturating_sub(visible_rows);
        let mut scroll = self.list_scroll.get().min(max_scroll);

        if self.selected < scroll {
            scroll = self.selected;
        } else if self.selected >= scroll.saturating_add(visible_rows) {
            scroll = self.selected.saturating_add(1).saturating_sub(visible_rows);
        }

        self.list_scroll.set(scroll.min(max_scroll));
    }

    fn selected_session(&self) -> Option<&SessionMetadata> {
        self.filtered.get(self.selected)
    }

    fn cycle_sort(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.apply_sort_and_filter();
        self.status = Some(
            tr(self.locale, MessageId::SessionsSortStatus).replace("{sort}", &self.sort_label()),
        );
    }

    /// What the list pane shows in place of rows it has not got: the listing
    /// is still in flight, or the store could not be listed.
    fn list_notice(&self) -> Option<ListNotice<'_>> {
        if self.pending_list.is_some() {
            Some(ListNotice::Loading)
        } else {
            self.load_error.as_deref().map(ListNotice::Failed)
        }
    }

    fn sort_label(&self) -> String {
        match self.sort_mode {
            SessionSortMode::Recent => tr(self.locale, MessageId::SessionsSortRecent),
            SessionSortMode::Name => tr(self.locale, MessageId::SessionsSortName),
            SessionSortMode::Size => tr(self.locale, MessageId::SessionsSortSize),
        }
        .into_owned()
    }

    fn enter_search(&mut self) {
        self.search_mode = true;
        self.search_input.clear();
        self.status = Some(tr(self.locale, MessageId::SessionsSearchPrompt).into_owned());
    }

    fn exit_search(&mut self) {
        self.search_mode = false;
        self.apply_sort_and_filter();
        self.status = None;
    }

    fn delete_selected(&mut self) -> Option<ViewEvent> {
        let session = self.selected_session().cloned()?;
        let manager = SessionManager::default_location().ok()?;
        // A deleted document the live session still owns refuses every later
        // save, so the conversation on screen would silently stop persisting.
        let refusal = if self.current_session_id.as_deref() == Some(session.id.as_str()) {
            Some(MessageId::SessionsDeleteOpenHere)
        } else if manager.is_session_live_anywhere(&session.id) {
            Some(MessageId::SessionsDeleteOpenElsewhere)
        } else {
            None
        };
        if let Some(reason) = refusal {
            self.status = Some(tr(self.locale, reason).into_owned());
            return None;
        }
        if let Err(err) = manager.delete_session(&session.id) {
            self.status = Some(
                tr(self.locale, MessageId::SessionsDeleteFailed)
                    .replace("{error}", &err.to_string()),
            );
            return None;
        }
        self.sessions.retain(|s| s.id != session.id);
        self.apply_sort_and_filter();
        self.refresh_preview();
        self.status = Some(
            tr(self.locale, MessageId::SessionsDeleted)
                .replace("{id}", crate::session_manager::truncate_id(&session.id)),
        );
        Some(ViewEvent::SessionDeleted {
            session_id: session.id,
            title: session.title,
        })
    }

    /// Archive or restore the selected session (#2934 / #4397).
    ///
    /// Writes through [`SessionManager::set_session_archived`] — the same
    /// single writer `PATCH /v1/sessions/{id}` uses — so the picker and the
    /// dashboard cannot disagree about what "archived" means. Emits a
    /// `SessionRenamed` event carrying the saved metadata so the app-level
    /// caches (and the sidebar rail) see the new lifecycle state without
    /// re-reading disk.
    fn toggle_archive_selected(&mut self) -> ViewAction {
        let Some(session) = self.selected_session().cloned() else {
            self.status = Some(tr(self.locale, MessageId::SessionsNoSelection).into_owned());
            return ViewAction::None;
        };
        let manager = match SessionManager::default_location() {
            Ok(manager) => manager,
            Err(err) => {
                self.status = Some(
                    tr(self.locale, MessageId::SessionsOpenFailed)
                        .replace("{error}", &err.to_string()),
                );
                return ViewAction::None;
            }
        };
        let archived = !session.archived;
        let metadata = match manager.set_session_archived(
            &session.id,
            archived,
            crate::session_manager::SessionMutator::Owner,
        ) {
            Ok(metadata) => metadata,
            Err(err) => {
                self.status = Some(
                    tr(self.locale, MessageId::SessionsArchiveFailed)
                        .replace("{error}", &err.to_string()),
                );
                return ViewAction::None;
            }
        };

        if let Some(local) = self.sessions.iter_mut().find(|s| s.id == session.id) {
            local.archived = metadata.archived;
        }
        self.apply_sort_and_filter();
        let message_id = if archived {
            MessageId::SessionsArchived
        } else {
            MessageId::SessionsRestored
        };
        self.status = Some(
            tr(self.locale, message_id)
                .replace("{id}", crate::session_manager::truncate_id(&session.id)),
        );
        ViewAction::Emit(ViewEvent::SessionArchived {
            metadata: Box::new(metadata),
        })
    }

    /// Flip whether archived sessions appear in the list.
    fn toggle_show_archived(&mut self) {
        self.show_archived = !self.show_archived;
        let label = if self.show_archived {
            tr(self.locale, MessageId::SessionsShowingArchived)
        } else {
            tr(self.locale, MessageId::SessionsHidingArchived)
        };
        self.status = Some(label.into_owned());
        self.selected = 0;
        self.apply_sort_and_filter();
    }

    fn rename_selected(&mut self, new_title: &str) -> ViewAction {
        let Some(session) = self.selected_session().cloned() else {
            self.status = Some(tr(self.locale, MessageId::SessionsNoSelection).into_owned());
            return ViewAction::None;
        };
        if new_title.is_empty() || new_title.len() > 100 {
            self.status = Some(tr(self.locale, MessageId::SessionsTitleLength).into_owned());
            return ViewAction::None;
        }
        let manager = match SessionManager::default_location() {
            Ok(m) => m,
            Err(e) => {
                self.status = Some(
                    tr(self.locale, MessageId::SessionsOpenFailed)
                        .replace("{error}", &e.to_string()),
                );
                return ViewAction::None;
            }
        };
        let mut saved = match manager.load_session(&session.id) {
            Ok(s) => s,
            Err(e) => {
                self.status = Some(
                    tr(self.locale, MessageId::SessionsLoadFailed)
                        .replace("{error}", &e.to_string()),
                );
                return ViewAction::None;
            }
        };
        saved.metadata.title = new_title.to_string();
        if let Err(e) = manager.save_session(&saved) {
            self.status = Some(
                tr(self.locale, MessageId::SessionsRenameFailed).replace("{error}", &e.to_string()),
            );
            return ViewAction::None;
        }
        // Update our local metadata cache. The cached preview renders the old
        // title, so it is dropped rather than shown again (U08-09).
        if let Some(meta) = self.sessions.iter_mut().find(|s| s.id == session.id) {
            meta.title = new_title.to_string();
        }
        self.preview_cache.remove(&session.id);
        self.apply_sort_and_filter();
        self.refresh_preview();
        self.status =
            Some(tr(self.locale, MessageId::SessionsRenamed).replace("{title}", new_title));
        ViewAction::Emit(ViewEvent::SessionRenamed {
            metadata: Box::new(saved.metadata),
        })
    }

    fn refresh_preview(&mut self) {
        // Any load still in flight belongs to the previous selection.
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.pending_preview = None;

        let Some(session) = self.selected_session() else {
            self.current_preview = vec![tr(self.locale, MessageId::SessionsNoResults).into_owned()];
            self.scroll_history_to_latest();
            return;
        };
        let session_id = session.id.clone();

        if let Some(lines) = self.preview_cache.get(&session_id) {
            self.current_preview = lines.clone();
            self.scroll_history_to_latest();
            return;
        }

        // Reading and parsing a saved session is blocking disk I/O that grows
        // with the transcript; arrowing through the list must not stall the
        // event loop on it. Outside a runtime (unit tests, headless callers)
        // there is no loop to stall, so load inline.
        if tokio::runtime::Handle::try_current().is_err() {
            let load = load_preview(&session_id, self.locale);
            self.apply_preview_load(session_id, load);
            return;
        }

        if let Some(session) = self.selected_session() {
            self.current_preview = loading_preview_lines(session, self.locale);
        }
        self.scroll_history_to_latest();

        let cell = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&cell);
        let locale = self.locale;
        let id = session_id.clone();
        codewhale_util::utils::spawn_blocking_supervised("session-picker-preview", move || {
            let load = load_preview(&id, locale);
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(load);
            }
        });
        self.pending_preview = Some(PendingPreview {
            session_id,
            generation: self.preview_generation,
            cell,
        });
    }

    /// Apply a background preview load if it has landed and still belongs to
    /// the current selection. Called from `tick`; returns whether the visible
    /// preview changed, so the host knows to repaint.
    fn poll_preview(&mut self) -> bool {
        let Some(pending) = self.pending_preview.as_ref() else {
            return false;
        };
        let landed = pending.cell.lock().ok().and_then(|mut guard| guard.take());
        let Some(load) = landed else {
            return false;
        };
        let Some(pending) = self.pending_preview.take() else {
            return false;
        };
        let still_selected = self
            .selected_session()
            .is_some_and(|session| session.id == pending.session_id);
        if pending.generation != self.preview_generation || !still_selected {
            // Stale: keep a good result for later, never show it now.
            if load.cacheable {
                self.preview_cache.insert(pending.session_id, load.lines);
            }
            return false;
        }
        self.apply_preview_load(pending.session_id, load);
        true
    }

    fn apply_preview_load(&mut self, session_id: String, load: PreviewLoad) {
        if load.cacheable {
            self.preview_cache.insert(session_id, load.lines.clone());
        }
        self.current_preview = load.lines;
        self.scroll_history_to_latest();
    }
}

/// List the saved-session store. Blocking; runs on the blocking pool when a
/// runtime is available.
fn list_session_store() -> SessionListing {
    SessionManager::default_location()
        .and_then(|manager| manager.list_sessions())
        .map_err(|error| error.to_string())
}

/// Read one saved session and render its preview. Blocking; runs on the
/// blocking pool when a runtime is available.
fn load_preview(session_id: &str, locale: Locale) -> PreviewLoad {
    let manager = match SessionManager::default_location() {
        Ok(manager) => manager,
        Err(_) => {
            return PreviewLoad {
                lines: vec![tr(locale, MessageId::SessionsDirectoryFailed).into_owned()],
                cacheable: false,
            };
        }
    };
    match manager.load_session(session_id) {
        Ok(saved) => PreviewLoad {
            lines: build_preview_lines(&saved, locale),
            cacheable: true,
        },
        Err(_) => PreviewLoad {
            lines: vec![tr(locale, MessageId::SessionsPreviewFailed).into_owned()],
            cacheable: false,
        },
    }
}

/// What the preview pane shows while the transcript loads: the header facts
/// the list row already knows, then an ellipsis where the transcript goes.
/// Built from existing localized strings so no locale falls back to English.
fn loading_preview_lines(session: &SessionMetadata, locale: Locale) -> Vec<String> {
    vec![
        tr(locale, MessageId::SessionsPreviewId).replace("{id}", &session.id),
        tr(locale, MessageId::SessionsPreviewTitle).replace("{title}", &session.title),
        String::new(),
        "\u{2026}".to_string(),
    ]
}

impl ModalView for SessionPickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::SessionPicker
    }

    fn tick(&mut self) -> ViewAction {
        let listed = self.poll_session_list();
        if self.poll_preview() || listed {
            ViewAction::Redraw
        } else {
            ViewAction::None
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if self.confirm_delete {
            return ViewAction::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.apply_motion(crate::tui::list_nav::Motion::Prev);
            }
            MouseEventKind::ScrollDown => {
                self.apply_motion(crate::tui::list_nav::Motion::Next);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self
                    .last_row_hitboxes
                    .borrow()
                    .iter()
                    .find_map(|(y, index)| (*y == mouse.row).then_some(*index));
                if let Some(index) = clicked {
                    if self.selected == index {
                        if let Some(session) = self.filtered.get(index) {
                            return ViewAction::EmitAndClose(ViewEvent::SessionSelected {
                                session_id: session.id.clone(),
                            });
                        }
                    } else {
                        self.selected = index;
                        self.ensure_selected_visible();
                        self.refresh_preview();
                    }
                }
            }
            _ => {}
        }
        ViewAction::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if self.search_mode {
            match key.code {
                KeyCode::Enter => {
                    self.exit_search();
                }
                KeyCode::Esc => {
                    self.exit_search();
                    return ViewAction::None;
                }
                KeyCode::Backspace => {
                    self.search_input.pop();
                    self.apply_sort_and_filter();
                    return ViewAction::None;
                }
                KeyCode::Char(c) => {
                    self.search_input.push(c);
                    self.apply_sort_and_filter();
                    return ViewAction::None;
                }
                _ => {}
            }
        }

        if self.confirm_delete {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm_delete = false;
                    let target = self.delete_target.take();
                    if target.is_none()
                        || self.selected_session().map(|session| session.id.as_str())
                            != target.as_deref()
                    {
                        self.status =
                            Some(tr(self.locale, MessageId::SessionsDeleteCancelled).into_owned());
                        return ViewAction::None;
                    }
                    if let Some(event) = self.delete_selected() {
                        return ViewAction::Emit(event);
                    }
                    return ViewAction::None;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_delete = false;
                    self.delete_target = None;
                    self.status =
                        Some(tr(self.locale, MessageId::SessionsDeleteCancelled).into_owned());
                    return ViewAction::None;
                }
                _ => return ViewAction::None,
            }
        }

        if self.rename_mode {
            match key.code {
                KeyCode::Enter => {
                    self.rename_mode = false;
                    let new_title = self.rename_input.trim().to_string();
                    self.rename_input.clear();
                    return self.rename_selected(&new_title);
                }
                KeyCode::Esc => {
                    self.rename_mode = false;
                    self.rename_input.clear();
                    self.status =
                        Some(tr(self.locale, MessageId::SessionsRenameCancelled).into_owned());
                    return ViewAction::None;
                }
                KeyCode::Backspace => {
                    self.rename_input.pop();
                    return ViewAction::None;
                }
                KeyCode::Char(c) if !c.is_control() => {
                    self.rename_input.push(c);
                    return ViewAction::None;
                }
                _ => return ViewAction::None,
            }
        }

        // Shift-modified paging belongs to the preview pane (#6014) and must
        // be claimed before the shared vocabulary sees the bare keys.
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => {
                    let rows = self.history_visible_rows.get().max(1);
                    self.scroll_history(-(rows as isize));
                    return ViewAction::None;
                }
                KeyCode::PageDown => {
                    let rows = self.history_visible_rows.get().max(1);
                    self.scroll_history(rows as isize);
                    return ViewAction::None;
                }
                _ => {}
            }
        }
        // Movement keys come from the shared vocabulary (#6290); this match
        // owns only the picker's own verbs.
        if let Some(motion) = crate::tui::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Char('/') => {
                self.enter_search();
                ViewAction::None
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.cycle_sort();
                ViewAction::None
            }
            // `a`/`A` toggles the per-workspace scope filter (#1395). The
            // picker defaults to showing only sessions for the current
            // workspace so Ctrl+R never restores a different project's
            // history by surprise; press `a` to broaden to every saved
            // session.
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.toggle_all_workspaces();
                ViewAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.rename_mode = true;
                self.rename_input.clear();
                self.status = None;
                ViewAction::None
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.confirm_delete = true;
                self.delete_target = self.selected_session().map(|session| session.id.clone());
                self.status = Some(tr(self.locale, MessageId::SessionsDeletePrompt).into_owned());
                ViewAction::None
            }
            // `e` archives or restores the selected session, `x` toggles
            // whether archived sessions are listed at all. Archive is
            // deliberately undestructive and needs no confirmation — unlike
            // `d`, nothing is lost and `e` puts it straight back.
            KeyCode::Char('e') | KeyCode::Char('E') => self.toggle_archive_selected(),
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.toggle_show_archived();
                ViewAction::None
            }
            KeyCode::Char(c) if self.select_visible_shortcut(c) => ViewAction::None,
            KeyCode::Enter => {
                if let Some(session) = self.selected_session() {
                    ViewAction::EmitAndClose(ViewEvent::SessionSelected {
                        session_id: session.id.clone(),
                    })
                } else {
                    ViewAction::None
                }
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let surface =
            render_underwater_surface(area, buf, tr(self.locale, MessageId::SessionsSurfaceTitle));
        let full_hints = [
            ActionHint::new("Enter", tr(self.locale, MessageId::SessionsActionResume)),
            ActionHint::new("/", tr(self.locale, MessageId::SessionsActionSearch)),
            ActionHint::new("s", tr(self.locale, MessageId::SessionsActionSort)),
            ActionHint::new("r", tr(self.locale, MessageId::SessionsActionRename)),
            ActionHint::new("a", tr(self.locale, MessageId::SessionsActionAllWorkspaces)),
            ActionHint::new("e", tr(self.locale, MessageId::SessionsActionArchive)),
            ActionHint::new("x", tr(self.locale, MessageId::SessionsActionShowArchived)),
            ActionHint::new("d", tr(self.locale, MessageId::SessionsActionDelete)),
            ActionHint::new("Esc", tr(self.locale, MessageId::SessionsActionClose)),
        ];
        // The two bordered panes spend five rows on chrome before either can
        // show a content row. When the body cannot afford that, this room
        // keeps only the object it exists for — a selectable session list
        // with a usable resume action — and trims the action rail to match.
        let full_footer_rows = action_footer_lines(&full_hints, surface.width).len();
        let compact = usize::from(surface.height).saturating_sub(full_footer_rows) < 12;
        if compact {
            let content = render_modal_footer(
                surface,
                buf,
                &[
                    ActionHint::new("Enter", tr(self.locale, MessageId::SessionsActionResume)),
                    ActionHint::new("/", tr(self.locale, MessageId::SessionsActionSearch)),
                    ActionHint::new("Esc", tr(self.locale, MessageId::SessionsActionClose)),
                ],
            );
            let header_rows = 1 + usize::from(self.confirm_delete || self.status.is_some());
            let footer_rows = usize::from(!self.filtered.is_empty());
            let visible_rows = usize::from(content.height)
                .saturating_sub(header_rows + footer_rows)
                .max(1);
            self.update_list_viewport(visible_rows);
            let list_scroll = self.list_scroll.get();
            let list_content = render_panel_scroll_rail(
                content,
                buf,
                self.filtered.len().saturating_add(header_rows),
                list_scroll,
                visible_rows,
                true,
            );
            let list_lines = build_list_lines(
                &self.filtered,
                self.selected,
                list_content.width,
                list_scroll,
                visible_rows,
                self.search_mode,
                &self.search_input,
                &self.sort_label(),
                self.confirm_delete,
                self.rename_mode,
                &self.rename_input,
                self.status.as_deref(),
                self.list_notice(),
                self.current_session_id.as_deref(),
                self.locale,
            );
            *self.last_row_hitboxes.borrow_mut() = (0..visible_rows)
                .filter_map(|row| {
                    let index = list_scroll.saturating_add(row);
                    (index < self.filtered.len()).then_some((
                        list_content
                            .y
                            .saturating_add(header_rows as u16)
                            .saturating_add(row as u16),
                        index,
                    ))
                })
                .collect();
            Paragraph::new(list_lines)
                .wrap(Wrap { trim: false })
                .render(list_content, buf);
            return;
        }
        let content = render_modal_footer(surface, buf, &full_hints);
        let narrow = content.width < 95;
        let chunks = Layout::default()
            .direction(if narrow {
                Direction::Vertical
            } else {
                Direction::Horizontal
            })
            .constraints(if narrow {
                [Constraint::Percentage(42), Constraint::Percentage(58)]
            } else {
                // 36% was too thin for session rows — the list exists to be
                // scanned, so give it a fairer share on wide terminals
                // (#6014).
                [Constraint::Percentage(56), Constraint::Percentage(44)]
            })
            .split(content);
        let (history_area, list_area) = if narrow {
            (chunks[1], chunks[0])
        } else {
            (chunks[0], chunks[1])
        };

        let list_block = section_block(&tr(self.locale, MessageId::SessionsPaneTitle));
        let list_inner = list_block.inner(list_area);
        let header_rows = 1 + usize::from(self.confirm_delete || self.status.is_some());
        let footer_rows = usize::from(!self.filtered.is_empty());
        let visible_rows = usize::from(list_inner.height)
            .saturating_sub(header_rows + footer_rows)
            .max(1);
        self.update_list_viewport(visible_rows);
        let list_scroll = self.list_scroll.get();
        list_block.render(list_area, buf);
        let list_content = render_panel_scroll_rail(
            list_inner,
            buf,
            self.filtered.len().saturating_add(header_rows),
            list_scroll,
            visible_rows,
            true,
        );

        let list_lines = build_list_lines(
            &self.filtered,
            self.selected,
            list_content.width,
            list_scroll,
            visible_rows,
            self.search_mode,
            &self.search_input,
            &self.sort_label(),
            self.confirm_delete,
            self.rename_mode,
            &self.rename_input,
            self.status.as_deref(),
            self.list_notice(),
            self.current_session_id.as_deref(),
            self.locale,
        );
        *self.last_row_hitboxes.borrow_mut() = (0..visible_rows)
            .filter_map(|row| {
                let index = list_scroll.saturating_add(row);
                (index < self.filtered.len()).then_some((
                    list_content
                        .y
                        .saturating_add(header_rows as u16)
                        .saturating_add(row as u16),
                    index,
                ))
            })
            .collect();
        Paragraph::new(list_lines)
            .wrap(Wrap { trim: false })
            .render(list_content, buf);

        let history_block = section_block(&tr(self.locale, MessageId::SessionsHistoryPaneTitle));
        let history_inner = history_block.inner(history_area);
        self.update_history_viewport(history_inner.height as usize);
        history_block.render(history_area, buf);
        let history_content = render_panel_scroll_rail(
            history_inner,
            buf,
            self.current_preview.len(),
            self.history_scroll.get(),
            history_inner.height as usize,
            false,
        );
        let visible_preview = visible_preview_lines(
            &self.current_preview,
            self.history_scroll.get(),
            history_content.height as usize,
        );
        let preview_lines = format_preview(&visible_preview);

        Paragraph::new(preview_lines)
            .wrap(Wrap { trim: false })
            .render(history_content, buf);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_list_lines(
    sessions: &[SessionMetadata],
    selected: usize,
    width: u16,
    scroll: usize,
    visible_rows: usize,
    search_mode: bool,
    search_input: &str,
    sort_label: &str,
    confirm_delete: bool,
    rename_mode: bool,
    rename_input: &str,
    status: Option<&str>,
    notice: Option<ListNotice<'_>>,
    current_session_id: Option<&str>,
    locale: Locale,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let header = if search_mode {
        format!("/{search_input}")
    } else if rename_mode {
        format!(
            "{}{rename_input}_",
            tr(locale, MessageId::SessionsNewTitlePrompt)
        )
    } else {
        tr(locale, MessageId::SessionsScopeSortHeader).replace("{sort}", sort_label)
    };
    lines.push(Line::from(Span::styled(
        truncate(&header, width),
        Style::default().fg(palette::TEXT_MUTED),
    )));

    if confirm_delete {
        lines.push(Line::from(Span::styled(
            tr(locale, MessageId::SessionsConfirmDelete),
            Style::default()
                .fg(palette::STATUS_WARNING)
                .add_modifier(Modifier::BOLD),
        )));
    } else if let Some(status) = status {
        lines.push(Line::from(Span::styled(
            truncate(status, width),
            Style::default().fg(palette::WHALE_ACTION),
        )));
    }

    if sessions.is_empty() {
        match notice {
            Some(ListNotice::Loading) => {
                lines.push(Line::from(Span::styled(
                    "\u{2026}",
                    Style::default().fg(palette::TEXT_MUTED),
                )));
                return lines;
            }
            Some(ListNotice::Failed(error)) => {
                lines.push(Line::from(Span::styled(
                    truncate(error, width),
                    Style::default().fg(palette::STATUS_ERROR),
                )));
                return lines;
            }
            None => {}
        }
        lines.push(Line::from(Span::styled(
            tr(locale, MessageId::SessionsEmptyTitle),
            Style::default().fg(palette::TEXT_MUTED),
        )));
        lines.push(Line::from(Span::styled(
            tr(locale, MessageId::SessionsEmptyHint),
            Style::default().fg(palette::TEXT_HINT),
        )));
        return lines;
    }

    for (idx, session) in sessions.iter().enumerate().skip(scroll).take(visible_rows) {
        let slot = idx.saturating_sub(scroll).saturating_add(1);
        let prefix = if slot <= 9 {
            format!("{slot}. ")
        } else {
            "   ".to_string()
        };
        // The row for the session the user is inside gets a text label, not
        // colour alone — the same monochrome-safe convention as the
        // archived/fork labels (#6014).
        let is_current = current_session_id == Some(session.id.as_str());
        let mut line = format!(
            "{prefix}{}",
            format_session_line(session, is_current, locale)
        );
        line = truncate(&line, width);
        let style = if idx == selected {
            menu_style::selected_row_style()
        } else if is_current {
            Style::default().fg(palette::WHALE_ACTION)
        } else {
            Style::default().fg(palette::TEXT_PRIMARY)
        };
        lines.push(Line::from(Span::styled(line, style)));
    }

    if sessions.len() > visible_rows {
        let start = scroll.saturating_add(1);
        let end = (scroll + visible_rows).min(sessions.len());
        lines.push(Line::from(Span::styled(
            truncate(
                &tr(locale, MessageId::SessionsShowingRange)
                    .replace("{start}", &start.to_string())
                    .replace("{end}", &end.to_string())
                    .replace("{total}", &sessions.len().to_string()),
                width,
            ),
            Style::default().fg(palette::TEXT_DIM),
        )));
    }

    lines
}

pub(super) fn format_message_count(count: usize, locale: Locale) -> String {
    if locale == Locale::En && count == 1 {
        "1 msg".to_string()
    } else {
        tr(locale, MessageId::SessionsMessageCountCompact).replace("{count}", &count.to_string())
    }
}

fn format_session_line(session: &SessionMetadata, is_current: bool, locale: Locale) -> String {
    let age = format_relative_time(&session.updated_at, locale);
    let updated = crate::session_manager::format_session_updated_at(&session.updated_at, &age);
    let raw_title = extract_title(&session.title);
    let title = if raw_title == "Session" {
        truncate(crate::session_manager::truncate_id(&session.id), 32)
    } else {
        truncate(raw_title, 32)
    };
    let mode = session
        .mode
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| tr(locale, MessageId::SessionsUnknownMode).into_owned());
    let message_count = format_message_count(session.message_count, locale);
    let fork_label = if session.parent_session_id.is_some() {
        format!(" | {}", tr(locale, MessageId::SessionsForkCompact))
    } else {
        String::new()
    };
    // Archived rows are labelled in text, not by colour alone, so the state
    // survives monochrome terminals and screen readers.
    let fork_label = if session.archived {
        format!(
            "{fork_label} | {}",
            tr(locale, MessageId::SessionsArchivedCompact)
        )
    } else {
        fork_label
    };
    let current_label = if is_current {
        format!(" | {}", tr(locale, MessageId::SessionsCurrentCompact))
    } else {
        String::new()
    };
    format!(
        "{} | {} | {}{}{} | {} | {}",
        crate::session_manager::truncate_id(&session.id),
        title,
        message_count,
        current_label,
        fork_label,
        mode,
        updated
    )
}

fn build_preview_lines(session: &SavedSession, locale: Locale) -> Vec<String> {
    // Runtime control traffic is persisted with `role = "user"` because strict
    // chat templates reject any other role mid-conversation. Both surfaces that
    // already render a session drop it on this same predicate -- the live
    // transcript in `history_cells_from_message` and the read-only pane in
    // `session_peek::build_peek`. Showing it here attributed the runtime's own
    // bookkeeping to the person: an Operate session previewed as `USER:`
    // followed by the whole `<codewhale:runtime_event kind="operate_contract">`
    // envelope.
    //
    // Render the turns first so the header can count exactly what the body
    // draws. Filtering alone is not enough: a message can survive the filter
    // and still draw nothing, because `message_text_for_history` yields an
    // empty string for a thinking-only assistant turn (and for a user turn
    // that `extract_user_prompt` reduces to nothing). Counting the filtered
    // vector instead of the rendered turns reintroduced the same
    // unaccountable total this function exists to remove.
    let mut rendered_turns = 0usize;
    let mut body: Vec<String> = Vec::new();
    for message in session
        .messages
        .iter()
        .filter(|message| !crate::runtime_handoff::is_internal_runtime_handoff(message))
    {
        let text = message_text_for_history(message, locale);
        if text.trim().is_empty() {
            continue;
        }
        rendered_turns += 1;
        body.push(format!("{}:", message.role.as_str().to_ascii_uppercase()));
        for line in text.lines() {
            body.push(format!("  {line}"));
        }
        body.push(String::new());
    }

    let mut out = vec![
        tr(locale, MessageId::SessionsPreviewTitle)
            .replace("{title}", extract_title(&session.metadata.title)),
    ];
    // The full id is the handle `codewhale exec --session` and `/resume`
    // need — the list row only has room for the truncated form (#6014).
    out.push(tr(locale, MessageId::SessionsPreviewId).replace("{id}", &session.metadata.id));
    out.push(
        tr(locale, MessageId::SessionsPreviewUpdated).replace(
            "{updated}",
            &session
                .metadata
                .updated_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        ),
    );
    // Count what the preview actually shows. `session_peek` already reports
    // the conversation this way, for the same reason: a total the pane cannot
    // account for is not a useful number. The persisted
    // `metadata.message_count` stays untouched -- it is an index quantity
    // (`forked_from_message_count` stores it as a position, and the
    // empty-session tests key on it), not a display one.
    //
    // Known limitation: the list row still shows the persisted total, because
    // rows are rendered from the session index without loading messages. For
    // an Operate session the row can therefore read higher than the preview.
    out.push(
        tr(locale, MessageId::SessionsPreviewMessagesModel)
            .replace("{count}", &rendered_turns.to_string())
            .replace("{model}", &session.metadata.model),
    );
    if let Some(mode) = session.metadata.mode.as_deref() {
        out.push(tr(locale, MessageId::SessionsPreviewMode).replace("{mode}", mode));
    }
    out.push("".to_string());

    out.extend(body);
    if out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

fn message_text_for_history(message: &codewhale_models::Message, locale: Locale) -> String {
    let mut text = String::new();
    for block in &message.content {
        let part = match block {
            codewhale_models::ContentBlock::Text { text: body, .. } => {
                if message.role == Role::User {
                    extract_user_prompt(body).to_string()
                } else {
                    strip_thinking_tags(body)
                }
            }
            codewhale_models::ContentBlock::Thinking { .. } => String::new(),
            codewhale_models::ContentBlock::ToolUse { name, input, .. } => {
                tr(locale, MessageId::SessionsToolCall)
                    .replace("{name}", name)
                    .replace("{input}", &truncate(&input.to_string(), 180))
            }
            codewhale_models::ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                let id = if is_error.unwrap_or(false) {
                    MessageId::SessionsToolError
                } else {
                    MessageId::SessionsToolResult
                };
                tr(locale, id).replace("{content}", &truncate(&content.replace('\n', " "), 220))
            }
            codewhale_models::ContentBlock::ServerToolUse { name, input, .. } => {
                tr(locale, MessageId::SessionsServerTool)
                    .replace("{name}", name)
                    .replace("{input}", &truncate(&input.to_string(), 180))
            }
            codewhale_models::ContentBlock::ToolSearchToolResult { content, .. }
            | codewhale_models::ContentBlock::CodeExecutionToolResult { content, .. } => {
                tr(locale, MessageId::SessionsToolResult)
                    .replace("{content}", &truncate(&content.to_string(), 220))
            }
            codewhale_models::ContentBlock::ImageUrl { .. } => {
                tr(locale, MessageId::SessionsImage).into_owned()
            }
        };
        let part = part.trim();
        if !part.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part);
        }
    }
    text
}

fn format_preview(lines: &[String]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for line in lines {
        out.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
    out
}

fn preview_body_start(lines: &[String], visible_rows: usize) -> Option<usize> {
    let visible_rows = visible_rows.max(1);
    let body_start = lines
        .iter()
        .position(|line| line.is_empty())
        .map(|idx| idx + 1)?;
    (body_start < visible_rows).then_some(body_start)
}

fn max_history_scroll_for(lines: &[String], visible_rows: usize) -> usize {
    let visible_rows = visible_rows.max(1);
    let Some(body_start) = preview_body_start(lines, visible_rows) else {
        return lines.len().saturating_sub(visible_rows);
    };
    let body_visible_rows = visible_rows.saturating_sub(body_start).max(1);
    lines
        .len()
        .saturating_sub(body_start)
        .saturating_sub(body_visible_rows)
}

fn visible_preview_lines(lines: &[String], scroll: usize, visible_rows: usize) -> Vec<String> {
    let visible_rows = visible_rows.max(1);
    let max_scroll = max_history_scroll_for(lines, visible_rows);
    let scroll = scroll.min(max_scroll);
    let Some(body_start) = preview_body_start(lines, visible_rows) else {
        return lines
            .iter()
            .skip(scroll)
            .take(visible_rows)
            .cloned()
            .collect();
    };

    let body_visible_rows = visible_rows.saturating_sub(body_start).max(1);
    let mut out = Vec::with_capacity(visible_rows);
    out.extend(lines.iter().take(body_start).cloned());
    out.extend(
        lines
            .iter()
            .skip(body_start + scroll)
            .take(body_visible_rows)
            .cloned(),
    );
    out
}

/// Localized "2h ago" label. Shared with the sidebar Sessions rail so both
/// surfaces age a session with the same words.
pub(crate) fn format_relative_time(dt: &DateTime<chrono::Utc>, locale: Locale) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);
    if duration.num_minutes() < 1 {
        tr(locale, MessageId::SessionsTimeJustNow).into_owned()
    } else if duration.num_hours() < 1 {
        tr(locale, MessageId::SessionsTimeMinutesAgo)
            .replace("{count}", &duration.num_minutes().to_string())
    } else if duration.num_days() < 1 {
        tr(locale, MessageId::SessionsTimeHoursAgo)
            .replace("{count}", &duration.num_hours().to_string())
    } else {
        tr(locale, MessageId::SessionsTimeDaysAgo)
            .replace("{count}", &duration.num_days().to_string())
    }
}

fn truncate(text: &str, width: u16) -> String {
    let max = width.max(1) as usize;
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut current = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if current + w >= max.saturating_sub(3) {
            break;
        }
        out.push(ch);
        current += w;
    }
    out.push_str("...");
    out
}

/// Best-effort canonicalisation of a path so two recordings of the same
/// workspace match even when one is symlinked or relative. Falls back to
/// the input path when canonicalisation fails (e.g. for a deleted dir or
/// during tests with tmp paths that have already been cleaned up).
fn canonical_or_self(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use unicode_width::UnicodeWidthStr;

    fn test_session(idx: usize, title: &str) -> SessionMetadata {
        SessionMetadata {
            id: format!("session-{idx:02}"),
            title: title.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            message_count: idx + 1,
            total_tokens: 100,
            model: "deepseek-v4-pro".to_string(),
            model_provider: "deepseek".to_string(),
            model_provider_id: None,
            workspace: std::path::PathBuf::from("/tmp"),
            mode: Some("agent".to_string()),
            cost: crate::session_manager::SessionCostSnapshot::default(),
            parent_session_id: None,
            forked_from_message_count: None,
            runtime_store: None,
            cumulative_turn_secs: 0,
            archived: false,
            spawn_depth: 0,
        }
    }

    fn test_session_in(idx: usize, title: &str, workspace: &str) -> SessionMetadata {
        let mut s = test_session(idx, title);
        s.workspace = std::path::PathBuf::from(workspace);
        s
    }

    fn text_message(role: &str, text: &str) -> codewhale_models::Message {
        codewhale_models::Message {
            role: Role::from(role),
            content: vec![codewhale_models::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn saved_session_with_messages(messages: Vec<codewhale_models::Message>) -> SavedSession {
        let mut session = crate::session_manager::create_saved_session(
            &messages,
            "deepseek-v4-pro",
            std::path::Path::new("/tmp"),
            100,
            None,
        );
        session.metadata.title = "<turn_meta>{}</turn_meta>\nClean session title".to_string();
        session
    }

    fn picker_with(sessions: Vec<SessionMetadata>, scope: Option<&str>) -> SessionPickerView {
        let workspace_scope = scope.map(PathBuf::from);
        let mut view = SessionPickerView {
            sessions: sessions.clone(),
            pending_list: None,
            load_error: None,
            filtered: sessions,
            selected: 0,
            list_scroll: Cell::new(0),
            list_visible_rows: Cell::new(8),
            history_scroll: Cell::new(0),
            history_pinned_to_latest: Cell::new(true),
            history_visible_rows: Cell::new(12),
            search_input: String::new(),
            search_mode: false,
            sort_mode: SessionSortMode::Recent,
            preview_cache: PreviewCache::default(),
            current_preview: Vec::new(),
            preview_generation: 0,
            pending_preview: None,
            confirm_delete: false,
            delete_target: None,
            rename_mode: false,
            rename_input: String::new(),
            status: None,
            workspace_scope,
            show_all_workspaces: false,
            show_archived: false,
            hide_empty_sessions: true,
            current_session_id: None,
            last_row_hitboxes: RefCell::new(Vec::new()),
            locale: Locale::En,
        };
        view.apply_sort_and_filter();
        view
    }

    /// Deleting the session open in this window would leave it unable to
    /// save anything afterwards, with nothing on screen saying so.
    #[test]
    fn delete_refuses_the_session_open_here() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
        saved.metadata.id = "session-open-here".to_string();
        manager.save_session(&saved).expect("save session");
        let mut view = picker_with(vec![saved.metadata.clone()], None);
        view.current_session_id = Some("session-open-here".to_string());

        assert!(view.delete_selected().is_none(), "no deletion event");

        assert_eq!(
            view.status.as_deref(),
            Some(tr(view.locale, MessageId::SessionsDeleteOpenHere).as_ref())
        );
        assert_eq!(view.sessions.len(), 1, "the row stays listed");
        assert!(
            manager.load_session("session-open-here").is_ok(),
            "the document is not deleted"
        );
    }

    /// A session another Codewhale window holds open would be written back
    /// by that window's next autosave, so deleting it here is refused.
    #[test]
    fn delete_refuses_a_session_open_in_another_window() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
        saved.metadata.id = "session-open-elsewhere".to_string();
        manager.save_session(&saved).expect("save session");
        let _lease = manager.hold_live_lease_elsewhere("session-open-elsewhere");
        let mut view = picker_with(vec![saved.metadata.clone()], None);

        assert!(view.delete_selected().is_none(), "no deletion event");

        assert_eq!(
            view.status.as_deref(),
            Some(tr(view.locale, MessageId::SessionsDeleteOpenElsewhere).as_ref())
        );
        assert!(
            manager.load_session("session-open-elsewhere").is_ok(),
            "the document is not deleted"
        );
    }

    #[test]
    fn rename_selected_persists_and_emits_saved_metadata() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
        saved.metadata.id = "session-01".to_string();
        saved.metadata.title = "Before".to_string();
        manager.save_session(&saved).expect("save session");
        let mut view = picker_with(vec![saved.metadata.clone()], None);
        assert!(
            view.current_preview
                .iter()
                .any(|line| line == "Title: Before"),
            "{:?}",
            view.current_preview
        );

        let action = view.rename_selected("After");
        // U08-09: the cached preview rendered the old title; it must not be
        // shown again after the rename.
        assert!(
            view.current_preview
                .iter()
                .any(|line| line == "Title: After"),
            "{:?}",
            view.current_preview
        );

        let ViewAction::Emit(ViewEvent::SessionRenamed { metadata }) = action else {
            panic!("expected SessionRenamed event");
        };
        assert_eq!(metadata.id, "session-01");
        assert_eq!(metadata.title, "After");
        assert_eq!(view.sessions[0].title, "After");
        assert_eq!(
            manager
                .load_session("session-01")
                .expect("load renamed session")
                .metadata
                .title,
            "After"
        );
    }

    #[test]
    fn archive_toggle_persists_hides_the_row_and_emits_an_archive_receipt() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
        saved.metadata.id = "session-01".to_string();
        saved.metadata.title = "Finished work".to_string();
        manager.save_session(&saved).expect("save session");
        let mut view = picker_with(vec![saved.metadata.clone()], None);

        let action = view.toggle_archive_selected();

        // The event is `SessionArchived`, not `SessionRenamed`: the receipt has
        // to describe what actually happened.
        let ViewAction::Emit(ViewEvent::SessionArchived { metadata }) = action else {
            panic!("expected SessionArchived event");
        };
        assert!(metadata.archived);
        assert!(
            manager
                .load_session("session-01")
                .expect("reload")
                .metadata
                .archived,
            "archive must be durable, not view-local"
        );
        assert!(
            view.filtered.is_empty(),
            "an archived session leaves the default (active-only) list"
        );

        // `x` brings archived rows back into view without un-archiving them.
        view.toggle_show_archived();
        assert_eq!(view.filtered.len(), 1);
        assert!(view.filtered[0].archived);

        // And archiving is reversible from the same key.
        let restored = view.toggle_archive_selected();
        let ViewAction::Emit(ViewEvent::SessionArchived { metadata }) = restored else {
            panic!("expected SessionArchived event");
        };
        assert!(!metadata.archived);
        assert!(
            !manager
                .load_session("session-01")
                .expect("reload")
                .metadata
                .archived
        );
    }

    #[test]
    fn preselecting_a_row_lands_on_it_without_resuming_anything() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        for (id, title) in [("session-01", "First"), ("session-02", "Second")] {
            let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
            saved.metadata.id = id.to_string();
            saved.metadata.title = title.to_string();
            saved.metadata.workspace.clone_from(&workspace);
            manager.save_session(&saved).expect("save session");
        }

        let view = SessionPickerView::new_selecting(&workspace, Locale::En, "session-02");

        assert_eq!(
            view.selected_session().map(|s| s.id.as_str()),
            Some("session-02"),
            "the rail hands off a target row; the picker must land on it"
        );
    }

    #[test]
    fn preselecting_an_archived_row_widens_the_archive_filter_only() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut saved = saved_session_with_messages(vec![text_message("user", "hello")]);
        saved.metadata.id = "session-01".to_string();
        saved.metadata.title = "Put away".to_string();
        saved.metadata.workspace.clone_from(&workspace);
        saved.metadata.archived = true;
        manager.save_session(&saved).expect("save session");

        let view = SessionPickerView::new_selecting(&workspace, Locale::En, "session-01");

        assert_eq!(
            view.selected_session().map(|s| s.id.as_str()),
            Some("session-01")
        );
        assert!(view.show_archived, "archived rows had to be revealed");
        assert!(
            !view.show_all_workspaces,
            "revealing an archived row must not also broaden the workspace scope"
        );
    }

    #[test]
    fn empty_auto_created_sessions_stay_out_of_the_default_view() {
        let mut empty = test_session(0, crate::session_manager::DEFAULT_SESSION_TITLE);
        empty.message_count = 0;
        let view = SessionPickerView::new_with_sessions(
            std::path::Path::new("/tmp"),
            Locale::En,
            vec![test_session(1, "Real work"), empty],
        );

        assert_eq!(
            view.visible_session_ids(),
            vec!["session-01".to_string()],
            "a session with no messages has nothing to resume (#6014)"
        );
    }

    #[test]
    fn preselecting_an_empty_row_widens_the_empty_filter_only() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut saved = saved_session_with_messages(vec![]);
        saved.metadata.id = "session-01".to_string();
        saved.metadata.title = crate::session_manager::DEFAULT_SESSION_TITLE.to_string();
        saved.metadata.message_count = 0;
        saved.metadata.workspace.clone_from(&workspace);
        manager.save_session(&saved).expect("save session");

        let view = SessionPickerView::new_selecting(&workspace, Locale::En, "session-01");

        assert_eq!(
            view.selected_session().map(|s| s.id.as_str()),
            Some("session-01"),
            "an explicit handoff must still land on a hidden empty row (#6014)"
        );
        assert!(
            !view.hide_empty_sessions,
            "the empty-session filter had to be lifted to reach the row"
        );
    }

    #[test]
    fn current_session_row_carries_a_text_label() {
        let sessions = vec![
            test_session(1, "first session"),
            test_session(2, "second session"),
        ];
        let lines = build_list_lines(
            &sessions,
            1,
            80,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            Some("session-01"),
            Locale::En,
        );
        let row_for = |needle: &str| {
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.to_string())
                        .collect::<String>()
                })
                .find(|text| text.contains(needle))
                .unwrap_or_default()
        };
        assert!(
            row_for("first session").contains("current"),
            "the active row needs a monochrome-safe label (#6014): {:?}",
            row_for("first session")
        );
        assert!(!row_for("second session").contains("current"));
    }

    #[test]
    fn preview_lists_the_full_session_id() {
        let mut saved = saved_session_with_messages(vec![text_message("user", "hi")]);
        saved.metadata.id = "session-with-a-long-identifier".to_string();
        let lines = build_preview_lines(&saved, Locale::En);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("session-with-a-long-identifier")),
            "the row truncates the id; the preview is where the full handle lives (#6014)"
        );
    }

    /// The preview is a read-only view of a saved session, and runtime
    /// control traffic rides `role = "user"` on disk. Rendering it verbatim
    /// opened every Operate-mode preview with `USER:` and the whole internal
    /// `operate_contract` envelope, attributing the runtime's own bookkeeping
    /// to the person.
    #[test]
    fn preview_hides_internal_runtime_traffic() {
        let saved = saved_session_with_messages(vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            text_message("user", "ship the release"),
            text_message("assistant", "on it"),
        ]);

        let preview = build_preview_lines(&saved, Locale::En).join("\n");

        assert!(
            !preview.contains("codewhale:runtime_event"),
            "internal runtime traffic must not be shown to a person: {preview}"
        );
        assert!(
            !preview.contains("Input provenance:"),
            "the runtime provenance envelope must not leak either: {preview}"
        );
        assert!(
            preview.contains("ship the release") && preview.contains("on it"),
            "the person's own conversation still belongs in the preview: {preview}"
        );
        // The header must count what the body shows. The persisted
        // `metadata.message_count` is 3 here (it counts the runtime event);
        // printing that beside two visible turns is a total the pane cannot
        // account for.
        assert_eq!(
            saved.metadata.message_count, 3,
            "guard the premise: the persisted count still includes runtime traffic"
        );
        assert!(
            preview.contains("Messages: 2"),
            "the preview counts the conversation it renders, not the persisted \
             total: {preview}"
        );
    }

    /// The header must count the turns the body actually draws, not the
    /// messages that merely survived the runtime-traffic filter. A
    /// thinking-only assistant turn renders nothing (`message_text_for_history`
    /// yields an empty string for `ContentBlock::Thinking`), so counting the
    /// filtered vector printed a total the pane could not account for --
    /// the same defect the filter was added to remove.
    #[test]
    fn preview_count_excludes_turns_that_render_nothing() {
        let thinking_only = codewhale_models::Message {
            role: Role::from("assistant"),
            content: vec![codewhale_models::ContentBlock::Thinking {
                thinking: "silent deliberation".to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            }],
        };
        let saved = saved_session_with_messages(vec![
            crate::runtime_handoff::operate_contract_runtime_message(),
            thinking_only,
            text_message("user", "ship the release"),
            text_message("assistant", "on it"),
        ]);

        let preview = build_preview_lines(&saved, Locale::En).join("\n");

        assert!(
            preview.contains("Messages: 2"),
            "two turns are drawn, so the header must say two: {preview}"
        );
        assert!(
            !preview.contains("silent deliberation"),
            "a thinking block is not a rendered turn: {preview}"
        );
        // The rendered `ROLE:` headings are the ground truth for the count.
        let drawn = preview
            .lines()
            .filter(|line| *line == "USER:" || *line == "ASSISTANT:")
            .count();
        assert_eq!(drawn, 2, "header count must equal drawn turns: {preview}");
    }

    #[test]
    fn page_keys_page_the_list_and_clamp_at_the_end() {
        let sessions: Vec<SessionMetadata> = (0..20)
            .map(|i| test_session(i, &format!("work {i}")))
            .collect();
        let mut view = picker_with(sessions, None);
        // Seed the preview cache so refresh_preview never reaches the store.
        for session in &view.sessions {
            view.preview_cache
                .insert(session.id.clone(), vec!["preview".to_string()]);
        }

        view.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(view.selected, 8, "one viewport of eight rows");
        view.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(view.selected, 16);
        view.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(view.selected, 19, "paging clamps at the last row");
        view.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(view.selected, 11);
    }

    /// #6290 step 2: the picker navigates on the shared `list_nav` vocabulary,
    /// so Home/End exist here and the `j`/`k` aliases keep working — the two
    /// behaviors this surface previously hand-rolled (or lacked).
    #[test]
    fn home_end_and_letter_aliases_come_from_the_shared_vocabulary() {
        let sessions: Vec<SessionMetadata> = (0..20)
            .map(|i| test_session(i, &format!("work {i}")))
            .collect();
        let mut view = picker_with(sessions, None);
        for session in &view.sessions {
            view.preview_cache
                .insert(session.id.clone(), vec!["preview".to_string()]);
        }

        view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(view.selected, 19, "End lands on the last row");
        view.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(view.selected, 0, "Home lands on the first row");
        view.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(view.selected, 1, "`j` is still the Down alias");
        view.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(view.selected, 0, "`k` is still the Up alias");
    }

    #[test]
    fn shift_page_keys_still_scroll_the_history_preview() {
        let mut view = picker_with(vec![test_session(1, "only session")], None);
        view.current_preview = (0..30).map(|i| format!("line {i}")).collect();
        view.history_visible_rows.set(10);

        view.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::SHIFT));
        assert_eq!(view.history_scroll.get(), 10);
        view.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT));
        assert_eq!(view.history_scroll.get(), 0);
    }

    fn buffer_row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.x..area.x.saturating_add(area.width))
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    fn row_containing(buf: &Buffer, area: Rect, needle: &str) -> Option<u16> {
        (area.y..area.y.saturating_add(area.height))
            .find(|&y| buffer_row_text(buf, area, y).contains(needle))
    }

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn workspace_scope_filters_sessions_to_current_project() {
        // #1395 reproduction: Ctrl+R in project B must not surface sessions
        // from project A.
        let sessions = vec![
            test_session_in(1, "project-a chat", "/tmp/project-a"),
            test_session_in(2, "project-b chat", "/tmp/project-b"),
            test_session_in(3, "another project-a chat", "/tmp/project-a"),
        ];
        let view = picker_with(sessions, Some("/tmp/project-b"));
        assert_eq!(view.filtered.len(), 1, "only project-b session should show");
        assert_eq!(view.filtered[0].title, "project-b chat");
    }

    #[test]
    fn workspace_scope_toggle_a_expands_to_all_workspaces() {
        let sessions = vec![
            test_session_in(1, "a", "/tmp/project-a"),
            test_session_in(2, "b", "/tmp/project-b"),
            test_session_in(3, "c", "/tmp/project-c"),
        ];
        let mut view = picker_with(sessions, Some("/tmp/project-b"));
        assert_eq!(view.filtered.len(), 1);

        view.toggle_all_workspaces();
        assert_eq!(view.filtered.len(), 3, "after toggle, every session shows");
        assert!(view.show_all_workspaces);
        assert!(
            view.status
                .as_deref()
                .map(|s| s.contains("every workspace"))
                .unwrap_or(false),
            "status should announce the new mode, got {:?}",
            view.status
        );

        view.toggle_all_workspaces();
        assert_eq!(view.filtered.len(), 1, "toggling back restores the scope");
    }

    #[test]
    fn workspace_scope_none_means_show_all() {
        // An unscoped picker (no workspace) lists everything — matches the
        // pre-#1395 behaviour for any caller that opts out.
        let sessions = vec![
            test_session_in(1, "a", "/tmp/project-a"),
            test_session_in(2, "b", "/tmp/project-b"),
        ];
        let view = picker_with(sessions, None);
        assert_eq!(view.filtered.len(), 2);
    }

    #[test]
    fn build_list_lines_truncates_to_list_pane_width() {
        let sessions = vec![test_session(
            1,
            "A very long title that should be truncated by the list pane width",
        )];
        let width = 24;
        let lines = build_list_lines(
            &sessions,
            0,
            width,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            None,
            Locale::En,
        );

        for line in lines {
            let rendered_width: usize = line.spans.iter().map(|span| span.content.width()).sum();
            assert!(
                rendered_width <= width as usize,
                "line width {rendered_width} exceeded pane width {width}"
            );
        }
    }

    #[test]
    fn build_list_lines_selected_row_uses_muted_selection_highlight() {
        let sessions = vec![
            test_session(1, "first session"),
            test_session(2, "second session"),
        ];
        let lines = build_list_lines(
            &sessions,
            1,
            80,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            None,
            Locale::En,
        );

        let selected_line = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("second session"))
            })
            .expect("selected session should render");
        let span = selected_line
            .spans
            .first()
            .expect("selected row should have a span");

        assert_eq!(span.style.fg, Some(palette::SELECTION_TEXT));
        assert_eq!(span.style.bg, Some(palette::SELECTION_BG));
        assert_ne!(span.style.bg, Some(palette::WHALE_ACTION));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn session_picker_selected_row_renders_readable_selection_contrast() {
        let mut first = test_session(1, "first contrast fixture");
        first.id = "alpha-contrast-fixture".to_string();
        let mut second = test_session(2, "second contrast fixture");
        second.id = "bravo-contrast-fixture".to_string();
        let sessions = vec![first, second];
        let mut view = picker_with(sessions, None);
        view.selected = 1;
        view.ensure_selected_visible();
        view.current_preview = vec!["preview".to_string()];
        let selected_id = crate::session_manager::truncate_id(&view.filtered[view.selected].id);
        let area = Rect::new(0, 0, 120, 28);
        let mut buf = Buffer::empty(area);

        view.render(area, &mut buf);

        let y =
            row_containing(&buf, area, selected_id).expect("selected session row should render");
        let rendered_row = buffer_row_text(&buf, area, y);
        let highlighted_cells = (area.x..area.x.saturating_add(area.width))
            .filter(|&x| {
                let cell = &buf[(x, y)];
                !cell.symbol().trim().is_empty()
                    && cell.bg == palette::SELECTION_BG
                    && cell.fg == palette::SELECTION_TEXT
            })
            .count();

        assert!(
            highlighted_cells >= 4,
            "selected /sessions row should use readable selection text; got {highlighted_cells} highlighted cells on {rendered_row:?}"
        );
        assert!(
            !(area.x..area.x.saturating_add(area.width))
                .any(|x| buf[(x, y)].bg == palette::WHALE_ACTION),
            "selected /sessions row should not use the bright accent background"
        );
    }

    /// 40x12/60x16 regression: when two bordered panes cannot both show a
    /// content row, the picker keeps a single focused session list — with the
    /// selected session, its resume action, and truthful mouse hitboxes —
    /// instead of two empty headings over a wrapped footer.
    #[test]
    fn session_picker_compact_heights_keep_a_selectable_session() {
        let sessions = (0..6)
            .map(|idx| {
                let mut session = test_session(idx, "compact fixture session");
                session.id = format!("compact-fixture-{idx:02}");
                session
            })
            .collect::<Vec<_>>();
        let mut view = picker_with(sessions, None);
        view.selected = 4;
        view.ensure_selected_visible();

        for (width, height, label) in [(40u16, 12u16, "40x12"), (60, 16, "60x16")] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);

            view.render(area, &mut buf);

            let dump = buffer_text(&buf, area);
            let selected_id = crate::session_manager::truncate_id(&view.filtered[view.selected].id);
            assert!(
                row_containing(&buf, area, selected_id).is_some(),
                "{label} should render the selected session row:\n{dump}"
            );
            assert!(
                dump.contains("resume"),
                "{label} should keep the resume action visible:\n{dump}"
            );
            let hitboxes = view.last_row_hitboxes.borrow();
            assert!(
                !hitboxes.is_empty(),
                "{label} should register session hitboxes:\n{dump}"
            );
            for (y, idx) in hitboxes.iter() {
                let row = buffer_row_text(&buf, area, *y);
                let id = crate::session_manager::truncate_id(&view.filtered[*idx].id);
                assert!(
                    row.contains(id),
                    "{label} hitbox at y={y} should map to session {id}; got {row:?}"
                );
            }
        }
    }

    #[test]
    fn session_picker_visual_matrix_covers_narrow_and_medium_rendering() {
        let base_time = DateTime::parse_from_rfc3339("2026-06-25T10:30:00Z")
            .expect("visual matrix timestamp")
            .with_timezone(&Utc);
        let sessions = (0..12)
            .map(|idx| {
                let title = if idx == 6 {
                    "selected visual matrix target with 中文内容 and suffix that must truncate"
                } else {
                    "A very long terminal visual regression session title with 中文内容 and suffix that must truncate"
                };
                let mut session = test_session(idx, title);
                session.id = format!("visual-matrix-{idx:02}");
                session.created_at = base_time - chrono::Duration::seconds(idx as i64);
                session.updated_at = session.created_at;
                session
            })
            .collect::<Vec<_>>();
        let mut view = picker_with(sessions, None);
        view.selected = view
            .filtered
            .iter()
            .position(|session| session.id == "visual-matrix-06")
            .expect("visual matrix target session should be filtered");
        view.ensure_selected_visible();
        view.current_preview = vec![
            "Title: terminal visual matrix".to_string(),
            "Updated: 2026-06-25 10:30".to_string(),
            "Messages: 3 | Model: deepseek-v4-pro".to_string(),
            String::new(),
            "USER: narrow panes should keep long CJK text readable 中文中文中文".to_string(),
            "ASSISTANT: overlays should keep borders and truncate rows predictably".to_string(),
        ];

        for (width, height, label) in [(72, 20, "narrow"), (120, 28, "medium")] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);

            view.render(area, &mut buf);

            let dump = buffer_text(&buf, area);
            assert!(
                dump.contains("sessions (1-9, PgUp/PgDn)"),
                "{label} sessions pane missing:\n{dump}"
            );
            assert!(
                dump.contains("history (Shift+PgUp/PgDn)"),
                "{label} history pane missing:\n{dump}"
            );
            assert!(dump.contains('─'), "{label} hairline missing:\n{dump}");
            assert!(
                !dump.contains('┌') && !dump.contains('┘'),
                "{label} should use open hairlines, not boxed rooms:\n{dump}"
            );
            assert!(
                !dump.contains("suffix that must truncate"),
                "{label} long title tail leaked instead of truncating:\n{dump}"
            );
            assert!(
                dump.contains("..."),
                "{label} should show an explicit ellipsis for truncated rows:\n{dump}"
            );
            assert!(
                !dump.contains('\u{fffd}'),
                "{label} render emitted replacement characters:\n{dump}"
            );

            assert!(
                row_containing(&buf, area, "selected visual").is_some(),
                "{label} selected session row missing:\n{dump}"
            );
        }
    }

    #[test]
    fn build_list_lines_includes_absolute_updated_timestamp() {
        let mut session = test_session(1, "last friday thread");
        session.updated_at = DateTime::parse_from_rfc3339("2026-06-01T12:34:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let lines = build_list_lines(
            &[session],
            0,
            120,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            None,
            Locale::En,
        );

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("2026-06-01 12:34 UTC"),
            "session picker should include an absolute timestamp, got {rendered:?}"
        );
    }

    #[test]
    fn build_list_lines_marks_fork_lineage() {
        let mut forked = test_session(1, "forked path");
        forked.parent_session_id = Some("parent-session-abcdef".to_string());
        forked.forked_from_message_count = Some(3);
        let lines = build_list_lines(
            &[forked],
            0,
            120,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            None,
            Locale::En,
        );

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("fork"));
        assert!(!rendered.contains("parent-session-abcdef"));
    }

    #[test]
    fn build_list_lines_numbers_visible_rows_for_shortcuts() {
        let sessions = vec![
            test_session(1, "first session"),
            test_session(2, "second session"),
        ];
        let lines = build_list_lines(
            &sessions,
            0,
            80,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            None,
            None,
            Locale::En,
        );

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("1. session-"));
        assert!(rendered.contains("2. session-"));
    }

    #[test]
    fn digit_shortcut_selects_visible_session_for_history() {
        let sessions = vec![
            test_session(1, "first session"),
            test_session(2, "second session"),
            test_session(3, "third session"),
        ];
        let mut view = picker_with(sessions, None);

        assert!(view.select_visible_shortcut('2'));
        assert_eq!(view.selected, 1);
        assert!(
            view.status
                .as_deref()
                .is_some_and(|status| status.contains("Opened history"))
        );
        assert!(!view.select_visible_shortcut('9'));
    }

    #[test]
    fn history_scroll_pages_and_clamps() {
        let mut view = picker_with(vec![test_session(1, "first")], None);
        view.current_preview = (0..20).map(|idx| format!("line {idx}")).collect();
        view.history_visible_rows.set(5);

        view.scroll_history(6);
        assert_eq!(view.history_scroll.get(), 6);
        view.scroll_history(100);
        assert_eq!(view.history_scroll.get(), 15);
        view.scroll_history(-200);
        assert_eq!(view.history_scroll.get(), 0);
    }

    #[test]
    fn history_preview_keeps_header_while_scrolling_transcript() {
        let lines = vec![
            "Title: version".to_string(),
            "Updated: 2026-05-14 01:02".to_string(),
            "Messages: 100 | Model: auto".to_string(),
            "Mode: agent".to_string(),
            String::new(),
            "USER: oldest prompt".to_string(),
            "ASSISTANT: oldest answer".to_string(),
            "USER: middle prompt".to_string(),
            "ASSISTANT: middle answer".to_string(),
            "USER: newest prompt".to_string(),
            "ASSISTANT: newest answer".to_string(),
        ];

        let max_scroll = max_history_scroll_for(&lines, 8);
        assert_eq!(max_scroll, 3);

        let rendered = visible_preview_lines(&lines, max_scroll, 8).join("\n");
        assert!(rendered.contains("Title: version"));
        assert!(rendered.contains("Updated: 2026-05-14 01:02"));
        assert!(!rendered.contains("oldest prompt"));
        assert!(rendered.contains("newest prompt"));
        assert!(rendered.contains("newest answer"));
    }

    #[test]
    fn history_refresh_starts_at_latest_transcript_messages() {
        let mut view = picker_with(vec![test_session(1, "first")], None);
        view.current_preview = vec![
            "Title: first".to_string(),
            "Updated: 2026-05-14 01:02".to_string(),
            "Messages: 10 | Model: auto".to_string(),
            String::new(),
            "line 0".to_string(),
            "line 1".to_string(),
            "line 2".to_string(),
            "line 3".to_string(),
            "line 4".to_string(),
            "line 5".to_string(),
        ];
        view.history_visible_rows.set(6);

        view.scroll_history_to_latest();

        assert_eq!(view.history_scroll.get(), 4);
        assert!(view.history_pinned_to_latest.get());
    }

    #[test]
    fn build_preview_lines_shows_full_clean_history() {
        let messages = vec![
            text_message(
                "user",
                "<turn_meta>{\"cache\":\"x\"}</turn_meta>\nFirst visible prompt",
            ),
            text_message(
                "assistant",
                "<thinking>hidden reasoning</thinking>\nFirst visible answer",
            ),
            text_message("user", "Second prompt"),
            text_message("assistant", "Second answer"),
            text_message("user", "Third prompt"),
            text_message("assistant", "Third answer"),
            text_message("user", "Fourth prompt beyond old six-message preview"),
        ];
        let session = saved_session_with_messages(messages);
        let lines = build_preview_lines(&session, Locale::En).join("\n");

        assert!(lines.contains("Title: Clean session title"));
        assert!(lines.contains("First visible prompt"));
        assert!(lines.contains("First visible answer"));
        assert!(lines.contains("Fourth prompt beyond old six-message preview"));
        assert!(!lines.contains("turn_meta"));
        assert!(!lines.contains("hidden reasoning"));
    }

    #[test]
    fn ensure_selected_visible_updates_scroll_window() {
        let sessions = (0..10)
            .map(|idx| test_session(idx, &format!("Session {idx}")))
            .collect::<Vec<_>>();

        let mut view = SessionPickerView {
            sessions: sessions.clone(),
            pending_list: None,
            load_error: None,
            filtered: sessions,
            selected: 0,
            list_scroll: Cell::new(0),
            list_visible_rows: Cell::new(3),
            history_scroll: Cell::new(0),
            history_pinned_to_latest: Cell::new(true),
            history_visible_rows: Cell::new(12),
            search_input: String::new(),
            search_mode: false,
            sort_mode: SessionSortMode::Recent,
            preview_cache: PreviewCache::default(),
            current_preview: Vec::new(),
            preview_generation: 0,
            pending_preview: None,
            confirm_delete: false,
            delete_target: None,
            rename_mode: false,
            rename_input: String::new(),
            status: None,
            workspace_scope: None,
            show_all_workspaces: true,
            show_archived: false,
            hide_empty_sessions: false,
            current_session_id: None,
            last_row_hitboxes: RefCell::new(Vec::new()),
            locale: Locale::En,
        };

        view.selected = 6;
        view.ensure_selected_visible();
        assert_eq!(view.list_scroll.get(), 4);

        view.selected = 1;
        view.ensure_selected_visible();
        assert_eq!(view.list_scroll.get(), 1);

        view.selected = 9;
        view.ensure_selected_visible();
        assert_eq!(view.list_scroll.get(), 7);
    }

    #[test]
    fn session_picker_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;

        const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];
        for (w, h) in BLOCKER_SIZES {
            let sessions = vec![
                test_session(1, "first session"),
                test_session(2, "second session"),
            ];
            let mut view = picker_with(sessions, None);
            view.current_preview = vec![
                "Title: preview".to_string(),
                "Updated: 2026-06-25 10:30".to_string(),
                String::new(),
                "USER: hello".to_string(),
            ];

            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(view);
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let text = rows.join("\n");

            // Both panes and their key hints survive at every size. The long
            // in-pane action header truncates to the (sometimes narrow) list
            // pane width, so assert the pane titles, which carry the digit-jump
            // and paging shortcuts and always fit.
            assert!(text.contains("sessions"), "{w}x{h}: missing sessions pane");
            assert!(text.contains("history"), "{w}x{h}: missing history pane");
            assert!(text.contains("1-9"), "{w}x{h}: missing 1-9 shortcut hint");
            assert!(text.contains("PgUp/PgDn"), "{w}x{h}: missing paging hint");

            // Composited frame is fully opaque.
            assert!(!text.contains('X'), "{w}x{h}: background bleed-through");
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );

            // No horizontal overflow.
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    /// Drive `tick` until the background preview load has been applied.
    fn wait_for_preview(view: &mut SessionPickerView) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while view.pending_preview.is_some() {
            view.tick();
            assert!(
                std::time::Instant::now() < deadline,
                "preview load never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn select_id(view: &mut SessionPickerView, id: &str) {
        view.selected = view
            .filtered
            .iter()
            .position(|session| session.id == id)
            .expect("session listed");
    }

    #[test]
    fn preview_cache_is_a_bounded_lru() {
        let mut cache = PreviewCache::default();
        for idx in 0..PREVIEW_CACHE_CAPACITY {
            cache.insert(format!("s{idx}"), vec![format!("line {idx}")]);
        }
        // Touch the oldest so it survives the next eviction.
        assert!(cache.get("s0").is_some());
        cache.insert("new".to_string(), vec!["new".to_string()]);
        assert_eq!(cache.len(), PREVIEW_CACHE_CAPACITY);
        assert!(cache.get("s0").is_some(), "recently used entry survives");
        assert!(cache.get("s1").is_none(), "least recently used is evicted");
        for idx in 0..40 {
            cache.insert(format!("more{idx}"), Vec::new());
        }
        assert_eq!(cache.len(), PREVIEW_CACHE_CAPACITY);
    }

    /// Selecting a session must not read and parse its transcript on the
    /// event loop: inside a runtime the pane shows a loading placeholder at
    /// once, and the transcript arrives through `tick`. A load that finishes
    /// after the selection moved on is never shown.
    #[tokio::test]
    async fn preview_loads_off_the_event_loop_and_drops_stale_results() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut first = saved_session_with_messages(vec![text_message("user", "alpha body")]);
        first.metadata.id = "session-alpha".to_string();
        let mut second = saved_session_with_messages(vec![text_message("user", "beta body")]);
        second.metadata.id = "session-beta".to_string();
        manager.save_session(&first).expect("save first");
        manager.save_session(&second).expect("save second");
        let mut view = picker_with(vec![first.metadata.clone(), second.metadata.clone()], None);

        select_id(&mut view, "session-alpha");
        view.refresh_preview();
        assert!(
            view.pending_preview.is_some(),
            "load must run in the background"
        );
        assert!(
            view.current_preview.iter().any(|line| line == "\u{2026}")
                && view
                    .current_preview
                    .iter()
                    .any(|line| line.contains("session-alpha")),
            "placeholder names the session and marks the pending body: {:?}",
            view.current_preview
        );
        let alpha_generation = view.preview_generation;

        // Move on before alpha lands: alpha's result must never be shown.
        select_id(&mut view, "session-beta");
        view.refresh_preview();
        assert_ne!(view.preview_generation, alpha_generation);
        wait_for_preview(&mut view);
        let shown = view.current_preview.join("\n");
        assert!(shown.contains("beta body"), "{shown}");
        assert!(!shown.contains("alpha body"), "{shown}");

        // Returning to alpha loads it again, and a second visit is cached.
        select_id(&mut view, "session-alpha");
        view.refresh_preview();
        wait_for_preview(&mut view);
        assert!(view.current_preview.join("\n").contains("alpha body"));
        select_id(&mut view, "session-beta");
        view.refresh_preview();
        assert!(
            view.pending_preview.is_none(),
            "a cached preview is shown without another load"
        );
        assert!(view.current_preview.join("\n").contains("beta body"));
    }

    /// A preview that lands in the background must repaint the frame on its
    /// own; otherwise the placeholder stays up until the next key press.
    #[tokio::test]
    async fn landed_preview_requests_a_redraw_through_the_view_stack() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let manager = SessionManager::default_location().expect("session manager");
        let mut saved = saved_session_with_messages(vec![text_message("user", "gamma body")]);
        saved.metadata.id = "session-gamma".to_string();
        manager.save_session(&saved).expect("save session");
        let mut view = picker_with(vec![saved.metadata.clone()], None);
        select_id(&mut view, "session-gamma");
        view.refresh_preview();
        assert!(
            view.pending_preview.is_some(),
            "load runs in the background"
        );

        let mut stack = crate::tui::views::ViewStack::new();
        stack.push(view);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut redraws = 0;
        loop {
            let tick = stack.tick();
            assert!(tick.events.is_empty(), "a preview load emits no event");
            if tick.redraw {
                redraws += 1;
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "preview load never requested a redraw"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(redraws, 1);
        assert!(
            !stack.tick().redraw,
            "an idle tick after the preview landed must not keep repainting"
        );
    }

    /// U08-09: a session store that cannot be listed is reported as such,
    /// never painted as "No saved sessions yet".
    #[test]
    fn unreadable_session_store_is_reported_not_shown_empty() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        // A regular file where the store directory belongs cannot be listed.
        std::fs::write(tmp.path().join("sessions"), b"not a directory").expect("file");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let view = SessionPickerView::new(tmp.path(), Locale::En);
        assert!(view.load_error.is_some(), "the listing failure was dropped");
        let lines = build_list_lines(
            &view.filtered,
            0,
            120,
            0,
            5,
            false,
            "",
            "recent",
            false,
            false,
            "",
            None,
            view.list_notice(),
            None,
            Locale::En,
        );
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Could not open sessions"), "{text}");
        assert!(!text.contains("No saved sessions yet"), "{text}");
    }
}

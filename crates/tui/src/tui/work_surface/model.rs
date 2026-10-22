use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use codewhale_localization::{Locale, MessageId, tr};
use ratatui::layout::Rect;

use crate::settings::InlineDiffMode;
use crate::tools::canonical_action::canonical_action_alias;
use crate::tools::subagent::{AgentWorkerStatus, SubAgentResult, SubAgentStatus};
use crate::tui::app::{
    AgentCurrentActivityStatus, AgentProgressMeta, App, SidebarRowAction, TaskPanelEntry,
    TaskPanelEntryKind,
};
use crate::tui::background_indicator::is_live_shell_entry;
use crate::tui::history::{
    FileActivityKind, FileActivitySummary, FileMutationReceipt, HistoryCell, ToolCell,
};
use crate::tui::menu_style::{StatusKind, status_mark};
use crate::work_graph::{
    AcceptanceRequirement, EdgeKind, EvidenceKind, EvidenceKindTag, NodeKind, NodeState,
    OperationBinding, OwnerState, Provenance, WorkGraphSnapshot, WorkNode,
};

/// Persisted Ocean work-surface placement. Bottom is deliberately absent: the
/// composer and phase footer own the shell's lower edge. `Off` hides the rail
/// outright (rail unification, 0.9.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkSurfacePlacement {
    /// Under the composer — the default (round 3, 2026-09-01: the bar's
    /// information lives below the composer, so scrolling up reads as
    /// intentional history).
    #[default]
    Bottom,
    Top,
    Left,
    Right,
    Off,
}

impl WorkSurfacePlacement {
    /// Strip placements render as a horizontal band; side placements as a
    /// rail. Bottom shares Top's auto-fit height math — only its position
    /// in the frame differs.
    #[must_use]
    pub const fn is_strip(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }
}

/// Which view the bottom dock shows. Orthogonal to placement: the user picks
/// *where* the dock sits and *what* it shows.
///
/// One bottom view, cycled in this order (founder redirect, 2026-09-02):
/// agents → tasks → background → files → notepad → context → git → price.
/// `Pinned` folded into `Tasks` (the goal heading rides the tasks view on
/// side placements); `pinned` stays accepted as a setting word.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RailPanel {
    /// The agent roster: one row per sub-agent, retained after completion.
    Agents,
    /// The to-do list: plan-step rows from the work graph.
    #[default]
    Tasks,
    /// Background shells and durable tasks. Scheduled work has its own manager.
    Background,
    /// Live workspace PTYs shared with the model, with explicit input focus.
    Terminal,
    /// Files edited this session (`+/−`) and files read into context.
    Files,
    /// Per-workspace plain-text notes (`.codewhale/notes.md`).
    Notepad,
    /// The context budget: used/limit, compaction threshold, breakdown.
    Context,
    /// Branch, ahead/behind, dirty count, last commits.
    Git,
    /// Session cost, per-agent cost, cache hit %, model rate.
    Price,
}

impl RailPanel {
    /// Cycle order — also the tab order in the dock strip.
    pub const ORDER: [RailPanel; 9] = [
        Self::Tasks,
        Self::Agents,
        Self::Background,
        Self::Terminal,
        Self::Files,
        Self::Notepad,
        Self::Context,
        Self::Git,
        Self::Price,
    ];

    /// Views the dock opens on its own when they have content and the user
    /// has not picked one: the to-do list first, then live agents, then
    /// background work (founder, 2026-09-03: "To-do and Agents as the first
    /// two, opening on To-do"). The others open only when cycled to.
    pub const AUTO_ORDER: [RailPanel; 3] = [Self::Tasks, Self::Agents, Self::Background];

    #[must_use]
    pub fn next(self) -> Self {
        let index = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0);
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    #[must_use]
    pub fn prev(self) -> Self {
        let index = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0);
        Self::ORDER[(index + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DockTabTarget {
    Panel(RailPanel),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DockTabHitbox {
    pub(super) target: DockTabTarget,
    pub(super) area: Rect,
}

impl RailPanel {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "agents" | "subagents" | "sub-agents" => Self::Agents,
            "tasks" | "todo" | "todos" => Self::Tasks,
            "background" | "shells" | "jobs" => Self::Background,
            "terminal" | "terminals" => Self::Terminal,
            "files" | "changes" => Self::Files,
            "notepad" | "notes" => Self::Notepad,
            "context" | "session" => Self::Context,
            "git" | "branch" => Self::Git,
            "price" | "cost" => Self::Price,
            _ => Self::Tasks,
        }
    }

    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Tasks => "tasks",
            Self::Background => "background",
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Notepad => "notepad",
            Self::Context => "context",
            Self::Git => "git",
            Self::Price => "price",
        }
    }

    /// Tab label. CAPS on purpose (founder ruling 2026-09-03): the tabs are
    /// the dock's primary navigation and lowercase nouns read as status
    /// text beside them. The plan-step list reads TODO, not TASKS — the
    /// Background panel already holds running tasks, so TASKS collides
    /// with it. `as_setting()` stays lowercase — it is the persisted
    /// value, never a label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Agents => "AGENTS",
            Self::Tasks => "TODO",
            Self::Background => "BACKGROUND",
            Self::Terminal => "TERMINAL",
            Self::Files => "FILES",
            Self::Notepad => "NOTEPAD",
            Self::Context => "CONTEXT",
            Self::Git => "GIT",
            Self::Price => "PRICE",
        }
    }
}

impl WorkSurfacePlacement {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "top" => Self::Top,
            "left" => Self::Left,
            "right" => Self::Right,
            "off" => Self::Off,
            _ => Self::Bottom,
        }
    }

    #[must_use]
    pub const fn as_setting(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkRowId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkTone {
    Heading,
    Live,
    /// Consequential and waiting on someone — Cognition, not Failure. A to-do
    /// blocked on your answer has not failed.
    Attention,
    /// Something actually failed. The only tone that spends Failure red.
    Failure,
    Success,
    Muted,
}

#[derive(Debug, Clone)]
pub(super) struct WorkRow {
    pub id: WorkRowId,
    pub mark: &'static str,
    pub label: String,
    pub detail: String,
    pub tone: WorkTone,
    pub selectable: bool,
    pub primary_action: Option<SidebarRowAction>,
    /// Present only on sub-agent rows. Carries the fields the fleet row paints
    /// beyond `label`, so the renderer can drop them one at a time as the
    /// surface narrows instead of truncating one pre-joined string.
    pub agent: Option<AgentRowFacts>,
}

/// The parts of a sub-agent row that are laid out as their own columns.
///
/// `label` already carries the preferred identity column (nesting indent,
/// nickname when the agent has one, `(+N)` child count). This carries the
/// rest: the role-only spelling of that same column, what the agent is doing,
/// and the right-aligned receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AgentRowFacts {
    /// The identity column spelled with the fleet role instead of the
    /// nickname. Equal to `label` when the agent has no nickname. The
    /// renderer falls back to this when a nickname is too wide for the
    /// identity column — a name is shown whole or not at all.
    pub role_label: String,
    /// The status word (`running`, `completed`, `failed`, …) painted as its
    /// own column. The glyph carries the same fact for scanning; the word is
    /// what makes the row legible without memorizing glyph vocabulary
    /// (owner regression report, 2026-08-04).
    pub status: String,
    /// What the agent was sent to do.
    pub objective: String,
    /// Wall-clock seconds, frozen once the agent is observed terminal so a
    /// finished agent stops ticking. `None` when no duration is known.
    pub elapsed_secs: Option<u64>,
    /// Model from the child's frozen spawn route or a later effective-route
    /// usage envelope; never inferred from the parent session's model.
    pub model: Option<String>,
    /// Tokens used by the child. `None` means *genuinely unknown* —
    /// the row then renders no token figure rather than claiming zero.
    pub tokens: Option<u64>,
    /// Unsettled items on this child's to-do list. `None` when no list has
    /// been published; `Some(0)` means the list exists and is fully settled
    /// (the strip still hides a zero chip — see `agent_receipt`).
    pub todos_remaining: Option<u32>,
    /// Whether this row on its own keeps the work dock open
    /// ([`live_agent_row_count`]). Running, queued and answerable work does;
    /// a finished agent does not.
    ///
    /// Typed at every construction site, never sniffed back out of `status` —
    /// a renderer must not infer lifecycle from an English word
    /// (`crates/tui/AGENTS.md`). The derivation deliberately differs by
    /// source because the sources carry different facts: a card in the
    /// 45-second live cache is *news*, so a fresh failure re-opens the dock
    /// long enough to be seen, while the same worker's retained receipt stays
    /// readable for an hour (`COMPLETED_AGENT_RETENTION`) and must not pin the
    /// dock open for the rest of the session.
    pub holds_dock_open: bool,
}

#[derive(Debug, Clone)]
pub(super) struct WorkHitbox {
    pub id: WorkRowId,
    pub row_y: u16,
}

#[derive(Debug, Clone)]
enum WorkSourceState {
    Error(String),
    Disconnected,
}

impl WorkSourceState {
    const fn label(&self) -> &'static str {
        match self {
            Self::Error(_) => "error",
            Self::Disconnected => "disconnected",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Error(error) => error,
            Self::Disconnected => "Work Graph runtime is not attached",
        }
    }
}

/// Live Work summary recent-only presentation lifetime (#4688).
pub(super) const RECENT_ONLY_TTL_MS: u64 = 4_000;
/// Settled file/search/write receipt lifetime in the live strip (#4690).
pub(super) const ACTIVITY_RECEIPT_TTL_MS: u64 = 3_000;
pub(super) const TOP_HEIGHT_MIN: u16 = crate::settings::WORK_SURFACE_TOP_HEIGHT_MIN;
pub(super) const TOP_HEIGHT_MAX: u16 = crate::settings::WORK_SURFACE_TOP_HEIGHT_MAX;
pub(super) const SIDE_WIDTH_MIN: u16 = 26;
pub(super) const SIDE_WIDTH_MAX: u16 = 80;

/// Which restored work rows belong to a prior session instance (#4416).
///
/// Decided once per session id and cached: this instance's own later
/// autosaves restamp the persisted record, and re-probing after that would
/// re-badge restored rows as live work.
#[derive(Debug, Clone)]
pub(crate) struct SessionInstanceScope {
    pub(super) session_id: String,
    pub(super) from_prior_instance: bool,
    /// Node ids present in the graph supplied at session restore time: the
    /// restored persisted rows, as opposed to work this instance creates
    /// afterwards.
    pub(super) restored_nodes: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct WorkSurfaceState {
    #[cfg(all(unix, not(target_env = "ohos")))]
    pub(super) terminal: super::terminal::TerminalDock,
    pub placement: WorkSurfacePlacement,
    pub(super) effective_placement: WorkSurfacePlacement,
    /// Panel selection — orthogonal to placement.
    pub panel: RailPanel,
    /// The user picked `panel` (cycle key, tab click, `/workbar <view>`), so
    /// the auto rule leaves it alone and an empty view still paints. Esc
    /// clears it and the dock goes back to showing whichever work view has
    /// content.
    pub explicit_view: bool,
    /// The settled file activity `project()` computed this frame for the
    /// TASKS view. The FILES view and its tab badge read it, so the history
    /// is scanned once per frame, not once per view (#6565).
    pub(super) file_activity: SettledFileActivity,
    /// Cache key that produced `file_activity`. `None` until the first
    /// [`settled_file_activity`] call; kept in lockstep by its epoch checks.
    pub(super) file_activity_stamp: Option<FileActivityStamp>,
    pub top_height: u16,
    pub side_width: u16,
    pub(super) resizing: bool,
    pub(super) divider_hovered: bool,
    pub(super) resize_anchor_column: u16,
    pub(super) resize_anchor_row: u16,
    pub(super) resize_anchor_size: u16,
    /// Focus owner axis — distinct from selection and detail-open.
    pub focused: bool,
    /// Keyboard/mouse selection highlight.
    pub selected: Option<WorkRowId>,
    /// Which row currently owns an open detail (pager / agent card).
    pub opened: Option<WorkRowId>,
    pub scroll_offset: usize,
    pub last_area: Option<Rect>,
    pub visible_rows: usize,
    pub total_rows: usize,
    pub(super) dock_tabs: Vec<DockTabHitbox>,
    pub(super) pressed_tab: Option<DockTabTarget>,
    pub(super) hovered_tab: Option<DockTabTarget>,
    pub dismissed: bool,
    pub dismissed_at_rows: usize,
    /// The auto view measured at dismissal: the dock re-opens when that
    /// view grows or when the auto rule picks a different one (a worker
    /// starting while the to-do list was closed is new work).
    pub(super) dismissed_view: RailPanel,
    pub(super) hovered: Option<WorkRowId>,
    pub(super) hitboxes: Vec<WorkHitbox>,
    pub(super) cached_graph: Option<WorkGraphSnapshot>,
    pub(super) latest_rows: Vec<WorkRow>,
    /// Full ranked catalog retained for inspector/history after live chrome expires.
    pub(super) catalog_rows: Vec<WorkRow>,
    /// Monotonic origin for presentation lifetimes (not wall-clock epoch).
    presentation_origin: Instant,
    /// Optional injected clock (ms since origin) for deterministic tests.
    presentation_now_ms: Option<u64>,
    /// When the projection last became recent-only (ms since origin).
    recent_only_since_ms: Option<u64>,
    /// Fingerprint of the recent-only set so a new completion can re-surface once.
    recent_only_fingerprint: u64,
    /// After TTL or user-turn, keep the live summary collapsed until new actionable work.
    recent_only_suppressed: bool,
    /// When the current activity receipt fingerprint first became live.
    activity_since_ms: Option<u64>,
    activity_fingerprint: u64,
    activity_suppressed: bool,
    /// Bumped on accepted user turns / newly started operations.
    user_turn_epoch: u64,
    last_handled_user_turn_epoch: u64,
    /// Elapsed wall-clock, in ms, captured the first frame each sub-agent was
    /// observed in a terminal state. The manager's `duration_ms` is
    /// `started_at.elapsed()` recomputed per snapshot, so it keeps growing
    /// after an agent finishes; latching the first terminal reading is what
    /// makes a completed row stop ticking.
    pub(super) frozen_agent_elapsed_ms: std::collections::HashMap<String, u64>,
    /// Session-instance ownership of the restored session record (#4416).
    pub(crate) session_instance: Option<SessionInstanceScope>,
    /// Test override for the sessions directory the ownership probe reads;
    /// production resolves the default location lazily.
    pub(crate) session_owner_probe_dir: Option<std::path::PathBuf>,
}

impl Default for WorkSurfaceState {
    fn default() -> Self {
        Self::with_placement(WorkSurfacePlacement::Bottom)
    }
}

impl WorkSurfaceState {
    #[must_use]
    pub(crate) fn is_resizing(&self) -> bool {
        self.resizing
    }

    /// The placement actually rendered this frame (after the narrow-terminal
    /// fallback), for truthful status readouts.
    #[must_use]
    pub fn effective_placement(&self) -> WorkSurfacePlacement {
        self.effective_placement
    }

    #[must_use]
    pub fn with_placement(placement: WorkSurfacePlacement) -> Self {
        Self::with_layout(placement, 3, 30)
    }

    #[must_use]
    pub fn with_layout(placement: WorkSurfacePlacement, top_height: u16, side_width: u16) -> Self {
        Self {
            placement,
            effective_placement: placement,
            panel: RailPanel::default(),
            #[cfg(all(unix, not(target_env = "ohos")))]
            terminal: super::terminal::TerminalDock::default(),
            explicit_view: false,
            file_activity: SettledFileActivity::default(),
            file_activity_stamp: None,
            top_height: top_height.clamp(TOP_HEIGHT_MIN, TOP_HEIGHT_MAX),
            side_width: side_width.clamp(SIDE_WIDTH_MIN, SIDE_WIDTH_MAX),
            resizing: false,
            divider_hovered: false,
            resize_anchor_column: 0,
            resize_anchor_row: 0,
            resize_anchor_size: 0,
            focused: false,
            selected: None,
            opened: None,
            scroll_offset: 0,
            last_area: None,
            visible_rows: 0,
            total_rows: 0,
            dock_tabs: Vec::new(),
            pressed_tab: None,
            hovered_tab: None,
            dismissed: false,
            dismissed_at_rows: 0,
            dismissed_view: RailPanel::default(),
            hovered: None,
            hitboxes: Vec::new(),
            cached_graph: None,
            latest_rows: Vec::new(),
            catalog_rows: Vec::new(),
            presentation_origin: Instant::now(),
            presentation_now_ms: None,
            recent_only_since_ms: None,
            recent_only_fingerprint: 0,
            recent_only_suppressed: false,
            activity_since_ms: None,
            activity_fingerprint: 0,
            activity_suppressed: false,
            user_turn_epoch: 0,
            last_handled_user_turn_epoch: 0,
            frozen_agent_elapsed_ms: std::collections::HashMap::new(),
            session_instance: None,
            session_owner_probe_dir: None,
        }
    }

    /// Inject a monotonic clock for presentation-lifetime tests.
    #[cfg(test)]
    pub(super) fn set_presentation_now_ms(&mut self, now_ms: u64) {
        self.presentation_now_ms = Some(now_ms);
    }

    /// Signal that the user accepted a turn or a new operation started.
    /// Recent-only live chrome collapses immediately (#4688).
    pub fn note_user_turn_or_new_operation(&mut self) {
        self.user_turn_epoch = self.user_turn_epoch.wrapping_add(1);
    }

    /// Record the exact graph restored from persisted session state. This
    /// must happen at the restore boundary: the first later runtime capture
    /// may already contain work created by this process.
    pub(crate) fn record_restored_session(
        &mut self,
        session_id: &str,
        graph: Option<&WorkGraphSnapshot>,
    ) {
        let from_prior_instance = session_record_from_prior_instance(self, session_id);
        let restored_nodes = if from_prior_instance {
            graph
                .into_iter()
                .flat_map(|graph| graph.nodes.iter())
                .map(|node| node.id.as_str().to_string())
                .collect()
        } else {
            HashSet::new()
        };
        self.session_instance = Some(SessionInstanceScope {
            session_id: session_id.to_string(),
            from_prior_instance,
            restored_nodes,
        });
    }

    /// A restored graph row owned by a prior session instance whose terminal
    /// failure or staleness must not render as this session's live work
    /// (#4416). Plan steps stay: the resumed to-do list is the point of
    /// restoring; failed/stale operations and blockers are the leak.
    pub(super) fn is_prior_instance_residue(&self, node: &WorkNode) -> bool {
        let Some(scope) = self.session_instance.as_ref() else {
            return false;
        };
        scope.from_prior_instance
            && scope.restored_nodes.contains(node.id.as_str())
            && node.kind != NodeKind::PlanStep
            && matches!(node.state, NodeState::Failed | NodeState::Stale)
    }

    fn now_ms(&self) -> u64 {
        self.presentation_now_ms.unwrap_or_else(|| {
            u64::try_from(self.presentation_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
        })
    }

    pub(super) fn selected_index(&self, rows: &[WorkRow]) -> Option<usize> {
        self.selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.id == selected))
    }

    /// Keep row identity and the viewport offset valid without moving the
    /// viewport to the remembered keyboard selection. Mouse-wheel scrolling
    /// is allowed to leave that selection off-screen until keyboard
    /// navigation resumes.
    pub(super) fn clamp_viewport(&mut self, rows: &[WorkRow]) {
        let selectable = rows.iter().filter(|row| row.selectable).collect::<Vec<_>>();
        if selectable.is_empty() {
            self.selected = None;
            // An explicitly opened empty panel still owns its tabs and Esc.
            // Preserve that focus; ordinary typing can still release it.
            self.focused &= self.explicit_view;
            self.scroll_offset = 0;
            return;
        }
        let established_selection = selectable
            .iter()
            .any(|row| Some(&row.id) == self.selected.as_ref());
        if !established_selection {
            let preferred = selectable
                .iter()
                // Both halves of the old `Attention` tone: splitting Failure out
                // of it changed what red means, not what deserves focus first.
                .find(|row| matches!(row.tone, WorkTone::Attention | WorkTone::Failure))
                .or_else(|| selectable.iter().find(|row| row.tone == WorkTone::Live))
                .copied()
                .unwrap_or(selectable[0]);
            self.selected = Some(preferred.id.clone());
            // Establishing a new selection should reveal the current or
            // needs-input item without reordering the canonical list. Later
            // redraws keep mouse-wheel ownership and do not chase selection.
            if let Some(selected) = rows.iter().position(|row| row.id == preferred.id) {
                if selected < self.scroll_offset {
                    self.scroll_offset = selected;
                } else if self.visible_rows > 0
                    && selected >= self.scroll_offset.saturating_add(self.visible_rows)
                {
                    self.scroll_offset = selected.saturating_add(1) - self.visible_rows;
                }
            }
        }
        self.scroll_offset = self
            .scroll_offset
            .min(rows.len().saturating_sub(self.visible_rows.max(1)));
    }

    /// Reveal the remembered selection after keyboard navigation. Rendering
    /// alone must use `clamp_viewport`; otherwise every redraw undoes a mouse
    /// wheel offset when the selection is above the viewport.
    pub(super) fn clamp_selection(&mut self, rows: &[WorkRow]) {
        self.clamp_viewport(rows);
        let Some(selected) = self.selected_index(rows) else {
            return;
        };
        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if self.visible_rows > 0
            && selected >= self.scroll_offset.saturating_add(self.visible_rows)
        {
            self.scroll_offset = selected.saturating_add(1).saturating_sub(self.visible_rows);
        }
        self.scroll_offset = self
            .scroll_offset
            .min(rows.len().saturating_sub(self.visible_rows.max(1)));
    }
}

pub(super) fn project(app: &mut App) -> Vec<WorkRow> {
    let active_session = app.current_session_id.is_some();
    freeze_terminal_agent_elapsed(app);
    let agents = agent_rows(app);
    let coordination = coordination_row(app);
    let activity = settled_file_activity(app).clone();
    let capture = app.runtime_services.work.as_ref().map(|work| {
        work.try_capture(app.current_session_id.as_deref())
            .map(|snapshot| snapshot.map(|snapshot| snapshot.graph))
    });

    let (graph, source_state) = match capture {
        Some(Ok(Some(graph))) => {
            app.work_surface.cached_graph = Some(graph.clone());
            (Some(graph), None)
        }
        Some(Ok(None)) => {
            app.work_surface.cached_graph = None;
            (None, None)
        }
        Some(Err(error)) => (
            app.work_surface.cached_graph.clone(),
            active_session.then_some(WorkSourceState::Error(error)),
        ),
        None => (
            app.work_surface.cached_graph.clone(),
            active_session.then_some(WorkSourceState::Disconnected),
        ),
    };

    update_session_instance_scope(app);

    let mut rows = match graph {
        Some(graph) => graph_rows(
            &mut app.work_surface,
            &graph,
            source_state.as_ref(),
            agents,
            coordination,
            activity,
        ),
        None if !agents.is_empty() || coordination.is_some() || !activity.is_empty() => {
            ordered_rows(
                &mut app.work_surface,
                None,
                source_state.as_ref(),
                agents,
                coordination,
                activity,
            )
        }
        None => source_state.map_or_else(Vec::new, |state| {
            vec![section_heading(
                "work",
                &format!("Work · {}", state.label()),
                state.detail(),
            )]
        }),
    };
    rows = with_live_shell_rows(app, rows);
    app.work_surface.latest_rows = rows.clone();
    if let Some(opened) = app.work_surface.opened.as_ref()
        && !rows.iter().any(|row| &row.id == opened)
        && !app
            .work_surface
            .catalog_rows
            .iter()
            .any(|row| &row.id == opened)
    {
        app.work_surface.opened = None;
    }
    rows
}

/// The tasks view: plan-step to-dos. On strips the list is literal to-dos
/// only; side rails keep the rest of the graph projection (operations,
/// blockers, receipts) under them. Sub-agents and shells have their own
/// views now, so nothing here competes with a running worker for rows — the
/// dock opens on the agents view while one runs.
pub(super) fn project_visible(app: &mut App) -> Vec<WorkRow> {
    let rows = project(app);
    let todo_ids = plan_step_row_ids(app);
    let strip = app.work_surface.effective_placement.is_strip();
    let mut out: Vec<WorkRow> = rows
        .into_iter()
        .filter(|row| {
            !row.id.0.starts_with("worker:")
                && !row.id.0.starts_with("shell:")
                && row.id.0 != "section:shells"
                && row.id.0 != "section:agents"
                && (!strip || todo_ids.contains(&row.id.0))
        })
        .collect();
    // On Top the goal is already the strip title; a side column repeats it
    // as its first row so the durable goal home survives in every placement.
    if !strip && let Some(label) = goal_row_label(app) {
        out.insert(0, section_heading("goal", &label, ""));
    }
    app.work_surface.latest_rows = out.clone();
    out
}

fn goal_row_label(app: &App) -> Option<String> {
    let (objective, paused) = crate::tui::footer_ui::active_goal_chip_state(app)?;
    let flat = objective.trim().replace(['\n', '\r'], " ");
    if flat.is_empty() {
        return None;
    }
    Some(if paused {
        format!("Goal (paused): {flat}")
    } else {
        format!("Goal: {flat}")
    })
}

/// The agents view: the roster. Every worker row, live or settled, under the
/// `▾ Subagents N` group door, oldest-first as the runtime reports them.
///
/// ## Order and nesting
///
/// This list is **roster-ordered** — creation order with parked husks sunk
/// last (`build_agent_roster`) — not tree-ordered. The `↳ ` indent and the
/// `(+N)` child count that `order_agent_seeds` stamps are therefore a fact
/// about the *live* projection carried through this merge, never a claim that
/// the row directly above an indented row is its parent.
///
/// Rows built from a retained receipt stay flat, and that is deliberate.
/// `AgentRosterRow` does carry `parent_run_id`, but deriving depth from it
/// here would put a second depth authority on one list and, because the list
/// is not tree-ordered, would draw exactly the dangling indent
/// `order_agent_seeds` refuses to draw. Nesting instead degrades to flat, and
/// it degrades symmetrically: once either half of a pair has outlived its
/// live card, the surviving half loses its `↳ ` or its `(+N)` with it,
/// because `order_agent_seeds` only ever sees cache seeds. A pair is nested
/// or it is flat; one half is never marked without the other. Pinned by
/// `a_retained_child_renders_flat_under_a_retained_parent` and
/// `a_live_child_flattens_once_its_parent_has_expired_to_a_receipt`.
fn agents_view_rows(app: &mut App) -> Vec<WorkRow> {
    let rows = project(app);
    let mut agents: Vec<WorkRow> = rows
        .into_iter()
        .filter(|row| row.id.0.starts_with("worker:"))
        .collect();
    // The compact cache expires settled cards after 45 seconds. The explicit
    // register is session history: retain its receipt rows, preserving fresh
    // cache/progress projections by worker ID when both sources know a worker.
    let mut retained = Vec::new();
    let mut seen = HashSet::new();
    for receipt in app.current_agent_roster() {
        let id = WorkRowId(format!("worker:{}", receipt.worker_id));
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(index) = agents.iter().position(|row| row.id == id) {
            retained.push(agents.remove(index));
            continue;
        }
        let parked = receipt.state == crate::agent_roster::RosterState::Parked;
        let status = if parked {
            current_activity_status_label(AgentCurrentActivityStatus::Parked, app.ui_locale)
        } else {
            std::borrow::Cow::Borrowed(worker_status_label(receipt.status))
        };
        let activity = receipt.activity.clone().unwrap_or_default();
        // The same name every other surface shows for this worker (#6565).
        let name = app
            .agent_label_map
            .get(&receipt.worker_id)
            .cloned()
            .unwrap_or_else(|| receipt.display_name.clone());
        retained.push(WorkRow {
            id,
            mark: receipt.state.glyph(),
            label: name.clone(),
            detail: if activity.is_empty() {
                status.to_string()
            } else {
                format!("{status} · {activity}")
            },
            tone: bucket_tone(if parked {
                WorkBucket::Ready
            } else {
                worker_status_bucket(receipt.status)
            }),
            selectable: true,
            primary_action: Some(SidebarRowAction::OpenAgentTranscript {
                agent_id: receipt.worker_id.clone(),
            }),
            agent: Some(AgentRowFacts {
                role_label: name,
                status: status.to_string(),
                objective: activity,
                elapsed_secs: receipt.millis.map(|millis| millis / 1_000),
                model: (!receipt.model.is_empty() && receipt.model != "unknown")
                    .then(|| receipt.model.clone()),
                tokens: receipt.output_tokens,
                todos_remaining: None,
                // History, not news: a settled receipt has nothing running,
                // and a parked husk asked nobody anything (#5906). Only a
                // worker still running or still answerable holds the dock.
                holds_dock_open: !parked && !receipt.state.is_terminal(),
            }),
        });
    }
    retained.extend(agents);
    let agents = retained;
    let mut out = Vec::with_capacity(agents.len() + 1);
    if !agents.is_empty() {
        out.push(agents_section_heading(&format!(
            "Subagents {}",
            agents.len()
        )));
        out.extend(agents);
    }
    app.work_surface.latest_rows = out.clone();
    out
}

/// Pick the view for this frame when the user has not picked one.
///
/// The dock is one bottom view. Cycling, a tab click, or `/workbar <view>`
/// makes the choice explicit and it sticks until Esc; otherwise the first of
/// [`RailPanel::AUTO_ORDER`] with content wins — agents while a sub-agent
/// runs, then the to-do list, then background work — and the persisted
/// `rail_panel` preference is what shows when none of them has anything.
pub(crate) fn resolve_view(app: &mut App) {
    if app.work_surface.explicit_view {
        return;
    }
    for panel in RailPanel::AUTO_ORDER {
        if !view_has_work(app, panel) {
            continue;
        }
        if app.work_surface.panel != panel {
            app.work_surface.panel = panel;
            app.work_surface.scroll_offset = 0;
            app.work_surface.selected = None;
        }
        return;
    }
}

/// How much work the auto-opening views hold between them: live agents,
/// to-dos, and running background work. The dismissed dock re-opens when
/// this grows, whichever view the new work lands in.
pub(crate) fn auto_work_rows(app: &mut App) -> usize {
    let agents = live_agent_row_count(app);
    let tasks = visible_rows_for(app, RailPanel::Tasks)
        .iter()
        .filter(|row| row.id.0.starts_with("graph:"))
        .count();
    let background = if background_has_live_work(app) {
        visible_rows_for(app, RailPanel::Background).len()
    } else {
        0
    };
    agents + tasks + background
}

fn view_has_work(app: &mut App, panel: RailPanel) -> bool {
    match panel {
        RailPanel::Agents => live_agent_row_count(app) > 0,
        // To-dos are the plan steps. A side rail also folds a summary of
        // running work into this view; that summary is not a reason to
        // open on TODO when the workers themselves live one tab over.
        RailPanel::Tasks => visible_rows_for(app, panel)
            .iter()
            .any(|row| row.id.0.starts_with("graph:")),
        // Scheduled automations that are not running are a fact about the
        // account, not work in this session: they must not open the dock
        // before the first prompt (0.9.12 defect #10). Live shells, durable
        // tasks are work. Scheduled automation configuration belongs in /automation.
        RailPanel::Background => background_has_live_work(app),
        RailPanel::Files
        | RailPanel::Terminal
        | RailPanel::Notepad
        | RailPanel::Context
        | RailPanel::Git
        | RailPanel::Price => false,
    }
}

/// Live worker rows: the ones that make the agents view open on its own.
///
/// Counted over [`agents_view_rows`] — the same merged projection the register
/// renders — not over the 45-second `subagent_cache` alone. A child that is
/// blocked on a person survives in the retained roster after its live card
/// expires, and across a session restore, where `subagent_cache` keeps only
/// this instance's workers; the dock has to open for it either way.
pub(super) fn live_agent_row_count(app: &mut App) -> usize {
    agents_view_rows(app)
        .iter()
        .filter(|row| {
            row.id.0.starts_with("worker:")
                && row
                    .agent
                    .as_ref()
                    .is_some_and(|facts| facts.holds_dock_open)
        })
        .count()
}

/// Whether the background view holds anything actually running: a live
/// shell or a queued or running durable task. Finished work stays listed but
/// never opens the dock or counts as live (#6565).
pub(super) fn background_has_live_work(app: &mut App) -> bool {
    !shell_work_rows(app).is_empty() || !durable_task_rows(app).is_empty()
}

/// The background view: live shells, live durable tasks, then a muted
/// `Finished N` group with the shells and tasks that ended, and how.
fn background_view_rows(app: &mut App) -> Vec<WorkRow> {
    let mut out = Vec::new();
    push_shell_group(&mut out, shell_work_rows(app));
    out.extend(durable_task_rows(app));
    let finished = finished_background_rows(app);
    if !finished.is_empty() {
        out.push(section_heading(
            "finished",
            &tr(app.ui_locale, MessageId::BackgroundFinishedHeading)
                .replace("{count}", &finished.len().to_string()),
            &tr(app.ui_locale, MessageId::BackgroundFinishedHint),
        ));
        out.extend(finished);
    }
    app.work_surface.latest_rows = out.clone();
    out
}

/// A background shell row, live or finished.
fn is_shell_entry(entry: &TaskPanelEntry) -> bool {
    entry.kind == TaskPanelEntryKind::Shell
}

/// Queued or running. A stale running task still needs a look, so it stays.
fn is_live_task_status(status: &str) -> bool {
    matches!(status, "queued" | "running")
}

/// Live durable tasks (queued or running).
fn durable_task_rows(app: &App) -> Vec<WorkRow> {
    app.task_panel
        .iter()
        .filter(|entry| !is_shell_entry(entry) && is_live_task_status(&entry.status))
        .map(|entry| {
            let status = if entry.stale {
                "stale"
            } else {
                entry.status.as_str()
            };
            let bucket = if entry.stale {
                WorkBucket::Attention
            } else if entry.status == "queued" {
                WorkBucket::Ready
            } else {
                WorkBucket::Active
            };
            WorkRow {
                id: WorkRowId(format!("task:{}", entry.id)),
                mark: agent_mark(bucket),
                label: entry.prompt_summary.clone(),
                detail: format!("{status} · {}", entry.id),
                tone: bucket_tone(bucket),
                selectable: true,
                primary_action: task_show_action(entry),
                agent: None,
            }
        })
        .collect()
}

fn task_show_action(entry: &TaskPanelEntry) -> Option<SidebarRowAction> {
    (!entry.id.starts_with("rlm-"))
        .then(|| SidebarRowAction::Command(format!("/task show {}", entry.id)))
}

fn shell_show_action(entry: &TaskPanelEntry) -> Option<SidebarRowAction> {
    Some(SidebarRowAction::Command(format!(
        "/jobs show {}",
        entry.id
    )))
}

/// Finished shells and durable tasks, muted, each saying how it ended:
/// `exit 0 · 12s`, `failed · exit 2`, `killed`, `timed out`. The shell
/// leads with its command; its id is only in the detail and inspector.
fn finished_background_rows(app: &App) -> Vec<WorkRow> {
    app.task_panel
        .iter()
        .filter(|entry| !is_live_task_status(&entry.status))
        .map(|entry| {
            let failed = matches!(entry.status.as_str(), "failed" | "timed_out");
            let tone = if failed {
                WorkTone::Failure
            } else {
                WorkTone::Muted
            };
            let mark = match entry.status.as_str() {
                "completed" => crate::tui::glyphs::DONE,
                _ if failed => crate::tui::glyphs::ATTENTION,
                _ => crate::tui::glyphs::NEUTRAL,
            };
            if is_shell_entry(entry) {
                let command = shell_command(entry);
                let outcome = finished_shell_outcome(app.ui_locale, entry);
                WorkRow {
                    id: WorkRowId(format!("shell:{}", entry.id)),
                    mark,
                    label: command.clone(),
                    detail: format!("{outcome} · {}", entry.id),
                    tone,
                    selectable: true,
                    primary_action: shell_show_action(entry),
                    agent: Some(AgentRowFacts {
                        role_label: "shell".to_string(),
                        status: outcome,
                        objective: command,
                        elapsed_secs: entry.duration_ms.map(|ms| ms / 1_000),
                        model: None,
                        tokens: None,
                        todos_remaining: None,
                        holds_dock_open: false,
                    }),
                }
            } else {
                let took = entry
                    .duration_ms
                    .map(|ms| format!(" · {}", crate::agent_roster::format_duration(ms)))
                    .unwrap_or_default();
                WorkRow {
                    id: WorkRowId(format!("task:{}", entry.id)),
                    mark,
                    label: entry.prompt_summary.clone(),
                    detail: format!(
                        "{}{took} · {}",
                        tr(
                            app.ui_locale,
                            match entry.status.as_str() {
                                "completed" => MessageId::BackgroundOutcomeDone,
                                "failed" => MessageId::BackgroundOutcomeFailed,
                                _ => MessageId::BackgroundOutcomeCancelled,
                            }
                        ),
                        entry.id
                    ),
                    tone,
                    selectable: true,
                    primary_action: task_show_action(entry),
                    agent: None,
                }
            }
        })
        .collect()
}

/// A shell entry's command, without the `shell: ` prefix; its id when the
/// command is empty.
fn shell_command(entry: &TaskPanelEntry) -> String {
    let command = entry
        .prompt_summary
        .strip_prefix("shell: ")
        .unwrap_or(entry.prompt_summary.as_str())
        .trim();
    if command.is_empty() {
        entry.id.clone()
    } else {
        crate::tui::history::summarize_tool_output(command).replace(['\n', '\r'], " ")
    }
}

/// How a finished shell ended, the same words its notice uses.
fn finished_shell_outcome(locale: Locale, entry: &TaskPanelEntry) -> String {
    let exit = entry
        .exit_code
        .map(|code| tr(locale, MessageId::BackgroundExitCode).replace("{code}", &code.to_string()));
    match entry.status.as_str() {
        "completed" => {
            let took = entry
                .duration_ms
                .map(|ms| format!(" · {}", crate::agent_roster::format_duration(ms)))
                .unwrap_or_default();
            format!(
                "{}{took}",
                exit.unwrap_or_else(|| tr(locale, MessageId::BackgroundOutcomeDone).into_owned())
            )
        }
        "failed" => {
            let failed = tr(locale, MessageId::BackgroundOutcomeFailed);
            exit.map_or_else(|| failed.to_string(), |exit| format!("{failed} · {exit}"))
        }
        "killed" => tr(locale, MessageId::BackgroundOutcomeKilled).into_owned(),
        "timed_out" => tr(locale, MessageId::BackgroundOutcomeTimedOut).into_owned(),
        other => other.to_string(),
    }
}

/// Row ids of the plan-step (to-do) nodes in the cached graph.
pub(super) fn plan_step_row_ids(app: &App) -> HashSet<String> {
    app.work_surface
        .cached_graph
        .as_ref()
        .map(|snapshot| {
            snapshot
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::PlanStep)
                .map(|node| format!("graph:{}", node.id.as_str()))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default()
}

/// Rows for the selected rail panel, routed through the same row/hitbox
/// machinery regardless of panel: every work row a user can see is a door
/// (`crates/tui/AGENTS.md`, "rows are objects"), whichever panel it appears
/// in.
///
/// - `Tasks` — the full live projection ([`project_visible`]).
/// - `Agents` — the full sub-agent register under the `▾ Subagents N` header,
///   then live shells, then the durable to-do checklist: opening the register
///   never hides the list.
/// - `Pinned` — the goal, the sub-agent group, then the plan-step to-dos.
/// - `Context` — empty: session facts are a line list, not work rows, and
///   render outside the row machinery.
///
/// **No panel choice may hide a running sub-agent.** `Pinned` used to filter
/// the projection down to plan steps, which meant the owner's own
/// `rail_panel = "pinned"` made a live worker unreachable: with no to-dos and
/// no goal the projection was empty, the strip collapsed to zero rows, and
/// the top bar was header chrome only — the 2026-08-04 "I spawned a sub agent
/// and the top bar showed nothing" report, pinned by
/// `tests/work_bar_subagents_pty.rs`. There is no header chip or phase-strip
/// fallback for sub-agents, so this strip is the *only* persistent surface
/// they have; a panel preference about which durable work to foreground is
/// not consent to lose the running fleet. Sub-agent rows are durable in the
/// same sense the panel's name means (they survive completion — see the row
/// lifetime rule in the module docs), so they belong here on their own terms.
pub(super) fn visible_rows_for_panel(app: &mut App) -> Vec<WorkRow> {
    let panel = app.work_surface.panel;
    visible_rows_for(app, panel)
}

pub(super) fn visible_rows_for(app: &mut App, panel: RailPanel) -> Vec<WorkRow> {
    match panel {
        RailPanel::Agents => agents_view_rows(app),
        RailPanel::Tasks => project_visible(app),
        RailPanel::Background => background_view_rows(app),
        RailPanel::Terminal => Vec::new(),
        RailPanel::Files => super::views::files_rows(app),
        RailPanel::Notepad => super::views::notepad_rows(app),
        RailPanel::Context => super::views::context_rows(app),
        RailPanel::Git => super::views::git_rows(app),
        RailPanel::Price => super::views::price_rows(app),
    }
}

/// Classify the current session against this process's session-instance
/// boot id (#4416), mirroring the `SubAgentManager` prior-session pattern
/// (#405). The probe runs once per session id. Persisted row identity is
/// recorded separately at the actual session restore boundary.
fn update_session_instance_scope(app: &mut App) {
    let Some(session_id) = app.current_session_id.clone() else {
        app.work_surface.session_instance = None;
        return;
    };
    let classified = app
        .work_surface
        .session_instance
        .as_ref()
        .is_some_and(|scope| scope.session_id == session_id);
    if !classified {
        let from_prior_instance =
            session_record_from_prior_instance(&app.work_surface, &session_id);
        app.work_surface.session_instance = Some(SessionInstanceScope {
            session_id,
            from_prior_instance,
            restored_nodes: HashSet::new(),
        });
    }
}

fn session_record_from_prior_instance(surface: &WorkSurfaceState, session_id: &str) -> bool {
    let manager = match surface.session_owner_probe_dir.as_ref() {
        Some(dir) => crate::session_manager::SessionManager::new(dir.clone()),
        None => crate::session_manager::SessionManager::default_location(),
    };
    manager.is_ok_and(|manager| manager.session_from_prior_instance(session_id))
}

fn graph_rows(
    surface: &mut WorkSurfaceState,
    snapshot: &WorkGraphSnapshot,
    source_state: Option<&WorkSourceState>,
    agents: Vec<RankedWorkRow>,
    coordination: Option<RankedWorkRow>,
    activity: SettledFileActivity,
) -> Vec<WorkRow> {
    ordered_rows(
        surface,
        Some(snapshot),
        source_state,
        agents,
        coordination,
        activity,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkBucket {
    Active,
    Attention,
    Ready,
    Recent,
}

impl WorkBucket {
    /// Presentation priority: needs-input outranks running work (#4689).
    const fn rank(self) -> u8 {
        match self {
            Self::Attention => 0,
            Self::Active => 1,
            Self::Ready => 2,
            Self::Recent => 3,
        }
    }

    const fn is_actionable(self) -> bool {
        !matches!(self, Self::Recent)
    }
}

#[derive(Clone)]
struct RankedWorkRow {
    bucket: WorkBucket,
    order: usize,
    is_plan_step: bool,
    row: WorkRow,
}

/// Complete unique targets; only the aggregate summary presentation is capped.
#[derive(Default, Clone, PartialEq, Eq)]
pub(super) struct SettledFileActivity {
    pub(super) summary: FileActivitySummary,
    pub(super) read: Vec<String>,
    list: Vec<String>,
    search: Vec<String>,
    pub(super) write: Vec<String>,
    pub(super) mutations: Vec<FileMutationReceipt>,
    pub(super) inline_diff_mode: InlineDiffMode,
}

impl std::fmt::Debug for SettledFileActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettledFileActivity")
            .field("read", &self.read.len())
            .field("write", &self.write.len())
            .field("mutations", &self.mutations.len())
            .finish_non_exhaustive()
    }
}

impl SettledFileActivity {
    fn is_empty(&self) -> bool {
        self.summary.is_empty()
    }
}

fn ordered_rows(
    surface: &mut WorkSurfaceState,
    snapshot: Option<&WorkGraphSnapshot>,
    source_state: Option<&WorkSourceState>,
    mut ranked: Vec<RankedWorkRow>,
    coordination: Option<RankedWorkRow>,
    activity: SettledFileActivity,
) -> Vec<WorkRow> {
    ranked.extend(coordination);
    if let Some(snapshot) = snapshot {
        ranked.extend(
            snapshot
                .nodes
                .iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::PlanStep | NodeKind::Operation | NodeKind::Blocker
                    )
                })
                .filter(|node| !is_settled_transient_operation(node))
                .filter(|node| !surface.is_prior_instance_residue(node))
                // A plan step dropped from the Plan/To-do list (e.g. the model
                // cleared it with an empty `todo_write`) stays in the graph
                // but must leave the rail (#6546).
                .filter(|node| {
                    node.kind != NodeKind::PlanStep || snapshot.compat.projects(&node.id)
                })
                .enumerate()
                .map(|(order, node)| RankedWorkRow {
                    bucket: node_bucket(node),
                    order: 10_000usize.saturating_add(order),
                    is_plan_step: node.kind == NodeKind::PlanStep,
                    row: graph_node_row(snapshot, node),
                }),
        );
    }

    // Activity is projected separately so we can apply a single aggregated
    // transient receipt instead of one live row per tool kind (#4690).
    let activity_row = aggregate_activity_row(&activity);
    if let Some(row) = activity_row.clone() {
        ranked.push(row);
    }

    ranked.sort_by(|a, b| match (a.is_plan_step, b.is_plan_step) {
        // To-do (plan step) rows keep canonical order: a completed step must
        // not sink below a later pending step and lose its identity. Agent and
        // operation rows still sort by status bucket (#4689).
        (true, true) => a.order.cmp(&b.order),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => a
            .bucket
            .rank()
            .cmp(&b.bucket.rank())
            .then_with(|| a.order.cmp(&b.order)),
    });

    let active = ranked
        .iter()
        .filter(|item| item.bucket == WorkBucket::Active)
        .count();
    let attention = ranked
        .iter()
        .filter(|item| item.bucket == WorkBucket::Attention)
        .count();
    let ready = ranked
        .iter()
        .filter(|item| item.bucket == WorkBucket::Ready)
        .count();
    let recent = ranked
        .iter()
        .filter(|item| item.bucket == WorkBucket::Recent)
        .count();
    let actionable = attention + active + ready;
    let source = source_state
        .map(|state| format!(" · {}", state.label()))
        .unwrap_or_default();
    let detail = match (snapshot, source_state) {
        (Some(snapshot), Some(state)) => {
            format!("graph revision {} · {}", snapshot.revision, state.detail())
        }
        (Some(snapshot), None) => format!("graph revision {}", snapshot.revision),
        (None, Some(state)) => state.detail().to_string(),
        (None, None) => "Current session activity".to_string(),
    };

    let now = surface.now_ms();
    let user_turn_force_hide = surface.user_turn_epoch != surface.last_handled_user_turn_epoch;
    if user_turn_force_hide {
        surface.last_handled_user_turn_epoch = surface.user_turn_epoch;
        if actionable == 0 {
            surface.recent_only_suppressed = true;
        }
        surface.activity_suppressed = true;
    }

    // Recent-only lifecycle (#4688): show a brief completion, then collapse.
    let recent_fp = fingerprint_rows(
        ranked
            .iter()
            .filter(|item| item.bucket == WorkBucket::Recent)
            .map(|item| item.row.id.0.as_str()),
    );
    if actionable > 0 {
        surface.recent_only_since_ms = None;
        surface.recent_only_suppressed = false;
        surface.recent_only_fingerprint = recent_fp;
    } else if recent > 0 {
        if surface.recent_only_fingerprint != recent_fp {
            // A new completion after expiry may surface once.
            surface.recent_only_fingerprint = recent_fp;
            surface.recent_only_since_ms = Some(now);
            surface.recent_only_suppressed = false;
        } else if surface.recent_only_since_ms.is_none() && !surface.recent_only_suppressed {
            surface.recent_only_since_ms = Some(now);
        }
        if let Some(since) = surface.recent_only_since_ms
            && now.saturating_sub(since) >= RECENT_ONLY_TTL_MS
        {
            surface.recent_only_suppressed = true;
        }
    } else {
        surface.recent_only_since_ms = None;
        surface.recent_only_suppressed = false;
        surface.recent_only_fingerprint = 0;
    }

    // Activity receipt lifetime (#4690): one aggregated row, 3s, no raw payloads.
    let activity_fp = activity_row
        .as_ref()
        .map(|row| fingerprint_rows(std::iter::once(row.row.label.as_str())))
        .unwrap_or(0);
    let show_activity = if activity_row.is_none() {
        surface.activity_since_ms = None;
        surface.activity_fingerprint = 0;
        surface.activity_suppressed = false;
        false
    } else {
        if surface.activity_fingerprint != activity_fp {
            surface.activity_fingerprint = activity_fp;
            surface.activity_since_ms = Some(now);
            surface.activity_suppressed = false;
        } else if surface.activity_since_ms.is_none() && !surface.activity_suppressed {
            surface.activity_since_ms = Some(now);
        }
        if let Some(since) = surface.activity_since_ms
            && now.saturating_sub(since) >= ACTIVITY_RECEIPT_TTL_MS
        {
            surface.activity_suppressed = true;
        }
        !surface.activity_suppressed
    };

    let subject = ranked
        .iter()
        .find(|item| item.bucket.is_actionable())
        .map(|item| (item.bucket, sanitize_summary_title(&item.row.label)));
    let heading_label = match (actionable > 0, subject.as_ref()) {
        (true, Some((WorkBucket::Attention, title))) => {
            format!("Work · Needs input: {title} · {attention} blocked{source}")
        }
        (true, Some((WorkBucket::Active, title))) => {
            format!("Work · Running: {title} · {active} active{source}")
        }
        (true, Some((WorkBucket::Ready, title))) => {
            format!("Work · Ready: {title} · {ready} ready{source}")
        }
        (true, _) => format!(
            "Work · {active} active · {attention} needs input · {ready} ready · {recent} recent{source}"
        ),
        (false, _) => format!(
            "Work · {active} active · {attention} needs input · {ready} ready · {recent} recent{source}"
        ),
    };

    // Full catalog for inspector/history even when live chrome collapses.
    let mut catalog = vec![section_heading("work", &heading_label, &detail)];
    catalog.extend(ranked.iter().map(|item| item.row.clone()));
    // Prior-instance terminal residue stays reachable through the explicit
    // catalog, labeled historical, but never as this session's live work
    // (#4416).
    if let Some(snapshot) = snapshot {
        catalog.extend(
            snapshot
                .nodes
                .iter()
                .filter(|node| surface.is_prior_instance_residue(node))
                .map(|node| {
                    let mut row = graph_node_row(snapshot, node);
                    row.detail = format!("prior session · {}", row.detail);
                    row.tone = WorkTone::Muted;
                    row
                }),
        );
    }
    surface.catalog_rows = catalog.clone();

    // Live chrome policy (Tasks/side projections — Top uses `project_visible`):
    // - actionable: heading + (optional) single activity receipt
    // - recent-only: transient receipts collapse after the TTL / next user
    //   turn (#4688); settled to-dos stay as durable rows. Settled sub-agents
    //   on Top collapse into the Subagents header (see `project_visible`) and
    //   remain reachable via the Agents panel / catalog.
    // - empty: no heading
    let is_durable =
        |item: &RankedWorkRow| item.is_plan_step || item.row.id.0.starts_with("worker:");
    let has_durable = ranked.iter().any(is_durable);
    if ranked.is_empty() && source_state.is_none() {
        return Vec::new();
    }
    if actionable == 0 && recent == 0 {
        // Source-only error/disconnected heading is still useful.
        return if source_state.is_some() {
            vec![section_heading("work", &heading_label, &detail)]
        } else {
            Vec::new()
        };
    }
    let suppress_transient_recent = actionable == 0 && surface.recent_only_suppressed;
    if suppress_transient_recent && !has_durable {
        return Vec::new();
    }

    // The live heading must count the rows the live list actually shows.
    // Once transient recent rows are suppressed, quoting the unfiltered
    // `recent` total would claim receipts the reader cannot see — the
    // catalog heading keeps the full count because the catalog keeps the
    // full rows.
    let live_heading = if suppress_transient_recent {
        let live_recent = ranked
            .iter()
            .filter(|item| item.bucket == WorkBucket::Recent && is_durable(item))
            .count();
        format!(
            "Work · {active} active · {attention} needs input · {ready} ready · {live_recent} recent{source}"
        )
    } else {
        heading_label.clone()
    };

    // Full ordered children remain in the projection for side rails, inspector
    // selection, and durable recent visibility. Live Top height is capped in
    // render (#4690). Recent-only *summary* lifetime is handled above (#4688).
    let mut live = vec![section_heading("work", &live_heading, &detail)];
    for item in ranked {
        if item.row.id.0 == "activity:aggregate" && !show_activity {
            continue;
        }
        if suppress_transient_recent && !is_durable(&item) {
            continue;
        }
        live.push(item.row);
    }
    live
}

fn fingerprint_rows<'a>(ids: impl Iterator<Item = &'a str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for id in ids {
        id.hash(&mut hasher);
    }
    hasher.finish()
}

fn sanitize_summary_title(raw: &str) -> String {
    let single_line = raw
        .chars()
        .map(|ch| {
            if ch.is_control() || ch == '\n' || ch == '\r' || ch == '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>();
    let collapsed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return "work item".to_string();
    }
    let mut chars = trimmed.chars();
    let prefix = chars.by_ref().take(72).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn coordination_row(app: &App) -> Option<RankedWorkRow> {
    let projection = app.coordination_detail.as_ref()?;
    let has_context_receipt = projection.context_projections.iter().any(|receipt| {
        !receipt.decision_ids.is_empty()
            || receipt.projected_bytes > 0
            || receipt.deduplicated > 0
            || receipt.omitted > 0
    });
    let has_metrics = !projection.metrics.hottest_paths.is_empty()
        || projection.metrics.package_or_module_growth.is_some()
        || projection.metrics.route_or_cost.is_some();
    if projection.decisions.is_empty()
        && projection.write_claims.is_empty()
        && projection.reconciliations.is_empty()
        && projection.contentions.is_empty()
        && !has_context_receipt
        && !has_metrics
    {
        return None;
    }
    let attention = crate::tui::coordination_detail::needs_attention(projection);
    let bucket = if attention {
        WorkBucket::Attention
    } else {
        WorkBucket::Recent
    };
    let title = app
        .tr(codewhale_localization::MessageId::CoordinationWorkTitle)
        .into_owned();
    Some(RankedWorkRow {
        bucket,
        // Coordination is a session-wide receipt, before individual workers
        // within the same bucket but after live/attention priority sorting.
        order: 100,
        is_plan_step: false,
        row: WorkRow {
            id: WorkRowId("coordination".to_string()),
            mark: if attention {
                crate::tui::glyphs::ATTENTION
            } else {
                crate::tui::glyphs::DONE
            },
            label: title.clone(),
            detail: crate::tui::coordination_detail::summary(app.ui_locale, projection),
            tone: bucket_tone(bucket),
            selectable: true,
            primary_action: Some(SidebarRowAction::InspectWork {
                title,
                body: crate::tui::coordination_detail::format(app.ui_locale, projection),
                stop_action: None,
            }),
            agent: None,
        },
    })
}

fn node_bucket(node: &WorkNode) -> WorkBucket {
    match node.state {
        NodeState::Initializing | NodeState::Active => WorkBucket::Active,
        NodeState::Failed if is_transient_failed_operation(node) => WorkBucket::Recent,
        NodeState::Waiting | NodeState::Blocked | NodeState::Stale | NodeState::Failed => {
            WorkBucket::Attention
        }
        NodeState::Completed if !node.acceptance.is_empty() => WorkBucket::Attention,
        NodeState::Ready => WorkBucket::Ready,
        NodeState::Completed
        | NodeState::Verified
        | NodeState::Superseded
        | NodeState::Cancelled => WorkBucket::Recent,
    }
}

fn is_transient_failed_operation(node: &WorkNode) -> bool {
    node.kind == NodeKind::Operation
        && node
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.durable)
        && node.acceptance.is_empty()
        && node.state == NodeState::Failed
}

/// One worker row before display ordering: the rendered row plus the parent
/// link and fleet identity the strip uses to number, order, and indent
/// nested spawns (#36).
struct AgentRowSeed {
    agent_id: String,
    parent_run_id: Option<String>,
    role: String,
    /// A real nickname or stable label, never the raw agent id (#36).
    name: Option<String>,
    ranked: RankedWorkRow,
}

/// Indent marker for a nested spawn: nothing at the top level (no permanent
/// chrome for the common flat fan-out), `↳` once nesting is actually
/// present, with two extra spaces per additional level (#36).
fn agent_nesting_indent(depth: usize) -> String {
    match depth {
        0 => String::new(),
        level => format!("{}↳ ", "  ".repeat(level.saturating_sub(1))),
    }
}

/// Compose the sub-agent identity column: nesting indent, who the agent is,
/// and `(+N)` when that agent has spawned children of its own.
///
/// `who` is the agent's nickname when it has one and its fleet role when it
/// does not. A nickname is identity that CodeWhale actually has, so it leads;
/// the role is the honest fallback. The raw agent-id hash is never a name and
/// is never rendered (#36).
fn agent_strip_label(indent: &str, who: &str, children: usize) -> String {
    if children == 0 {
        format!("{indent}{who}")
    } else {
        format!("{indent}{who} (+{children})")
    }
}

/// Order worker rows so nested spawns sit directly under their parent, then
/// stamp each label with its display depth and a sequential number. Rows
/// whose parent is not visible (e.g. the parent finished and left the cache)
/// stay at the top level — honest flat rendering beats a dangling indent.
fn order_agent_seeds(seeds: Vec<AgentRowSeed>) -> Vec<RankedWorkRow> {
    let known_ids: HashSet<&str> = seeds.iter().map(|seed| seed.agent_id.as_str()).collect();
    let mut children: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    let mut roots = Vec::new();
    for (idx, seed) in seeds.iter().enumerate() {
        if let Some(parent) = seed.parent_run_id.as_deref()
            && known_ids.contains(parent)
        {
            children.entry(parent).or_default().push(idx);
            continue;
        }
        roots.push(idx);
    }

    fn push_tree(
        idx: usize,
        depth: usize,
        seeds: &[AgentRowSeed],
        children: &std::collections::HashMap<&str, Vec<usize>>,
        seen: &mut HashSet<usize>,
        order: &mut Vec<(usize, usize)>,
    ) {
        if !seen.insert(idx) {
            return;
        }
        order.push((idx, depth));
        if let Some(child_indices) = children.get(seeds[idx].agent_id.as_str()) {
            for child_idx in child_indices {
                push_tree(*child_idx, depth + 1, seeds, children, seen, order);
            }
        }
    }

    let mut order = Vec::with_capacity(seeds.len());
    let mut seen = HashSet::new();
    for idx in roots {
        push_tree(idx, 0, &seeds, &children, &mut seen, &mut order);
    }
    // Cycle/orphan backstop: emit anything the walk missed at the top level.
    for idx in 0..seeds.len() {
        push_tree(idx, 0, &seeds, &children, &mut seen, &mut order);
    }

    // `(+N)` counts children that are actually on this surface: the same map
    // the tree walk used, so the badge can never promise a child the list does
    // not show. Snapshot it before `seeds` is consumed — `children` borrows it.
    let child_counts: Vec<usize> = seeds
        .iter()
        .map(|seed| {
            children
                .get(seed.agent_id.as_str())
                .map_or(0, |indices| indices.len())
        })
        .collect();

    let mut slots: Vec<Option<AgentRowSeed>> = seeds.into_iter().map(Some).collect();
    order
        .into_iter()
        .enumerate()
        .map(|(position, (idx, depth))| {
            let seed = slots[idx].take().expect("each row emitted exactly once");
            let mut ranked = seed.ranked;
            let indent = agent_nesting_indent(depth.min(3));
            let role_label = agent_strip_label(&indent, &seed.role, child_counts[idx]);
            ranked.row.label = match seed.name.as_deref() {
                Some(name) => agent_strip_label(&indent, name, child_counts[idx]),
                None => role_label.clone(),
            };
            if let Some(facts) = ranked.row.agent.as_mut() {
                facts.role_label = role_label;
            }
            // `ordered_rows` re-sorts within status buckets by `order`; stamp
            // the tree position so a child sorts directly under its parent
            // whenever they share a bucket.
            ranked.order = position;
            ranked
        })
        .collect()
}

fn agent_rows(app: &App) -> Vec<RankedWorkRow> {
    let cached_ids = app
        .subagent_cache
        .iter()
        .filter(|agent| !agent.from_prior_session)
        .map(|agent| agent.agent_id.as_str())
        .collect::<HashSet<_>>();
    let mut seeds = app
        .subagent_cache
        .iter()
        .filter(|agent| !agent.from_prior_session)
        .enumerate()
        .map(|(order, agent)| {
            let meta = app.agent_progress_meta.get(&agent.agent_id);
            let current_activity = meta.and_then(|meta| meta.current_activity.as_ref());
            let status = current_activity
                .map(|activity| current_activity_status_label(activity.status, app.ui_locale))
                .or_else(|| {
                    agent
                        .worker_status
                        .map(|status| std::borrow::Cow::Borrowed(worker_status_label(status)))
                })
                .unwrap_or_else(|| {
                    std::borrow::Cow::Borrowed(subagent_status_label(&agent.status))
                });
            let bucket = current_activity
                .map(|activity| current_activity_status_bucket(activity.status))
                .or_else(|| agent.worker_status.map(worker_status_bucket))
                .unwrap_or_else(|| subagent_status_bucket(&agent.status));
            // Read failure from the same source the bucket came from, so the
            // tone can never disagree with the row it is painting.
            let failed = current_activity.map_or_else(
                || {
                    agent.worker_status.map_or_else(
                        || matches!(agent.status, SubAgentStatus::Failed(_)),
                        |status| matches!(status, AgentWorkerStatus::Failed),
                    )
                },
                |activity| matches!(activity.status, AgentCurrentActivityStatus::Failed),
            );
            let resolved_profile = agent
                .child_route
                .as_ref()
                .and_then(|route| route.resolved_profile_id.as_deref())
                .map(str::trim)
                .filter(|profile| !profile.is_empty());
            let role = resolved_profile
                .or(agent
                    .assignment
                    .role
                    .as_deref()
                    .filter(|role| !role.trim().is_empty()))
                .unwrap_or_else(|| agent.agent_type.as_str())
                .to_string();
            // The dispatch name leads (#5287); a nickname or stable label
            // names the agents dispatched without one. Never the bare agent
            // id (#36) — absent rather than fabricated, so the identity
            // column falls back to the role.
            let name = app
                .agent_given_name(&agent.agent_id)
                .or_else(|| resolved_profile.map(str::to_string))
                .or_else(|| {
                    agent
                        .nickname
                        .clone()
                        .filter(|name| !name.trim().is_empty() && name != &agent.agent_id)
                })
                .or_else(|| app.agent_label_map.get(&agent.agent_id).cloned());
            let terminal = agent_is_terminal(agent, meta);
            // A finished agent's row says what it produced, not what it was
            // asked (#6565): the headline of its result, or why it stopped.
            // Enter opens the full text.
            let objective = terminal
                .then(|| crate::tui::agent_focus::settled_headline(app, &agent.agent_id))
                .flatten()
                .unwrap_or_else(|| summarize_assignment(&agent.assignment.objective));
            let mut facts = vec![status.to_string(), objective.clone()];
            // Quiet completion (#36): in-flight metadata (current tool, step
            // counters) is working state, not a receipt, and must not linger
            // after the run ends. What it changed is the receipt, so a
            // finished row keeps its file count.
            if terminal {
                if let Some(files) = meta
                    .map(|meta| meta.files_touched)
                    .filter(|count| *count > 0)
                {
                    facts.push(
                        tr(app.ui_locale, MessageId::BackgroundFilesChanged)
                            .replace("{count}", &files.to_string()),
                    );
                }
            } else {
                facts.extend(live_activity_facts(app, &agent.agent_id));
                facts.extend(quiet_fact(app, agent, meta));
            }
            AgentRowSeed {
                agent_id: agent.agent_id.clone(),
                parent_run_id: agent.parent_run_id.clone(),
                role,
                name,
                ranked: RankedWorkRow {
                    bucket,
                    order,
                    is_plan_step: false,
                    row: WorkRow {
                        id: WorkRowId(format!("worker:{}", agent.agent_id)),
                        mark: agent_mark(bucket),
                        // Stamped by `order_agent_seeds` once the display
                        // depth (and therefore the indent) is known.
                        label: String::new(),
                        detail: facts.join(" · "),
                        tone: agent_tone(bucket, failed),
                        selectable: true,
                        // One agent, one destination (v0.9.7): activation
                        // opens the agent's transcript directly; Agent
                        // Details is the secondary action from there.
                        primary_action: Some(SidebarRowAction::OpenAgentTranscript {
                            agent_id: agent.agent_id.clone(),
                        }),
                        agent: Some(AgentRowFacts {
                            // Stamped by `order_agent_seeds`, which is where
                            // the indent and child count become known.
                            role_label: String::new(),
                            status: status.to_string(),
                            objective,
                            elapsed_secs: Some(agent_elapsed_ms(app, agent) / 1_000),
                            model: meta.and_then(|meta| meta.resolved_model.clone()),
                            tokens: meta.and_then(|meta| meta.received_tokens),
                            todos_remaining: meta.and_then(|meta| meta.todos_remaining),
                            holds_dock_open: bucket.is_actionable(),
                        }),
                    },
                },
            }
        })
        .collect::<Vec<_>>();

    let mut progress_only = app
        .agent_progress
        .iter()
        .filter(|(id, _)| !cached_ids.contains(id.as_str()))
        .collect::<Vec<_>>();
    progress_only.sort_by_key(|(id, _)| (*id).clone());
    seeds.extend(
        progress_only
            .into_iter()
            .enumerate()
            .map(|(order, (id, _progress))| {
                let meta = app.agent_progress_meta.get(id);
                let current_activity = meta.and_then(|meta| meta.current_activity.as_ref());
                let status = current_activity
                    .map(|activity| current_activity_status_label(activity.status, app.ui_locale))
                    .unwrap_or(std::borrow::Cow::Borrowed("running"));
                let bucket = current_activity
                    .map(|activity| current_activity_status_bucket(activity.status))
                    .unwrap_or(WorkBucket::Active);
                let failed = current_activity
                    .is_some_and(|a| matches!(a.status, AgentCurrentActivityStatus::Failed));
                let name = app.agent_label_map.get(id).cloned();
                let mut facts = vec![status.to_string()];
                facts.extend(live_activity_facts(app, id));
                AgentRowSeed {
                    agent_id: id.clone(),
                    parent_run_id: meta.and_then(|meta| meta.parent_run_id.clone()),
                    // Role is unknown until the manager snapshot arrives;
                    // "agent" is the honest fallback, not a fabrication.
                    role: "agent".to_string(),
                    name,
                    ranked: RankedWorkRow {
                        bucket,
                        order: 5_000usize.saturating_add(order),
                        is_plan_step: false,
                        row: WorkRow {
                            id: WorkRowId(format!("worker:{id}")),
                            mark: agent_mark(bucket),
                            label: String::new(),
                            detail: facts.join(" · "),
                            tone: agent_tone(bucket, failed),
                            selectable: true,
                            // Same destination as the cached-seed rows above.
                            primary_action: Some(SidebarRowAction::OpenAgentTranscript {
                                agent_id: id.clone(),
                            }),
                            agent: Some(AgentRowFacts {
                                role_label: String::new(),
                                status: status.to_string(),
                                // No manager snapshot yet, so there is no
                                // assignment to quote: the live activity line
                                // is the honest answer to "what is it doing".
                                // The status word itself is the status
                                // column's job, so it is not repeated here.
                                objective: facts[1..].join(" · "),
                                // Neither a duration nor a usage envelope has
                                // been seen for this id. Both render as
                                // nothing rather than as `0s` / `0 tokens`.
                                elapsed_secs: None,
                                model: meta.and_then(|meta| meta.resolved_model.clone()),
                                tokens: meta.and_then(|meta| meta.received_tokens),
                                todos_remaining: meta.and_then(|meta| meta.todos_remaining),
                                holds_dock_open: bucket.is_actionable(),
                            }),
                        },
                    },
                }
            }),
    );
    order_agent_seeds(seeds)
}

fn summarize_assignment(value: &str) -> String {
    // Flatten newlines the way the goal-title path does: a multi-line
    // objective must not break the one-line work-bar row (2026-08-04 review).
    let summary = crate::tui::history::summarize_tool_output(value);
    if summary.contains(['\n', '\r']) {
        summary.replace(['\n', '\r'], " ")
    } else {
        summary
    }
}

/// A running agent's live facts for its dock row, each said once (#6565).
/// The activity detail usually already names the step and the tool ("step 6:
/// finished tool 'read_file'"); repeating them as `using read_file · step 6`
/// made every row say the same thing twice.
fn live_activity_facts(app: &App, agent_id: &str) -> Vec<String> {
    let meta = app.agent_progress_meta.get(agent_id);
    let activity = meta.and_then(|meta| meta.current_activity.as_ref());
    let detail = activity.and_then(|activity| activity.detail.as_deref());
    let mut facts = Vec::new();
    if let Some(detail) = detail {
        facts.push(detail.to_string());
    }
    let said = |fact: &str| detail.is_some_and(|detail| names_whole(detail, fact));
    if let Some(tool) = activity.and_then(|activity| activity.current_tool.as_deref())
        && !said(tool)
    {
        facts.push(tr(app.ui_locale, MessageId::BackgroundUsingTool).replace("{tool}", tool));
    }
    if let Some(step) = activity.and_then(|activity| activity.step)
        && !said(&format!("step {step}"))
    {
        facts.push(
            tr(app.ui_locale, MessageId::BackgroundStep).replace("{step}", &step.to_string()),
        );
    }
    if let Some(files) = meta
        .map(|meta| meta.files_touched)
        .filter(|count| *count > 0)
    {
        facts.push(
            tr(app.ui_locale, MessageId::BackgroundFilesChanged)
                .replace("{count}", &files.to_string()),
        );
    }
    facts
}

/// A running agent quiet this long says so on its row.
const QUIET_AFTER_MS: u64 = 60_000;

/// `quiet 2m · auto-stop at 5m` for a running agent that has shown the manager
/// no progress for a minute and has no tool in flight (#6565).
///
/// This reads the engine's own clock (`idle_ms`, the one its heartbeat reads)
/// and the bound that heartbeat enforces, capped by the last envelope the TUI
/// saw from the child, because `AgentList` snapshots are not refreshed by
/// ordinary progress. A tool in flight is never quiet: a long tool is expected, and the
/// heartbeat bound sits above the tool timeout. When the engine does stop the
/// agent, the row's result says so ("Auto-cancelled after 300s without
/// sub-agent progress"), and Stop on the row ends it sooner.
fn quiet_fact(
    app: &App,
    agent: &SubAgentResult,
    meta: Option<&AgentProgressMeta>,
) -> Option<String> {
    if agent.worker_status == Some(AgentWorkerStatus::RunningTool)
        || meta.and_then(|meta| meta.current_tool.as_ref()).is_some()
    {
        return None;
    }
    let since_snapshot = app.subagent_cache_received_at.map_or(0, |at| {
        u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
    });
    let mut idle = agent.idle_ms?.saturating_add(since_snapshot);
    // The snapshot is not refreshed by ordinary progress, so an agent that
    // kept working after it would otherwise read as quiet. Any envelope the
    // TUI saw from the child since then caps the estimate.
    if let Some(at) = meta.and_then(|meta| meta.last_progress_at) {
        idle = idle.min(u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    if idle < QUIET_AFTER_MS {
        return None;
    }
    let bound = agent.heartbeat_timeout_ms?;
    let coarse = |millis: u64| {
        let seconds = millis / 1_000;
        if seconds < 60 {
            format!("{seconds}s")
        } else {
            format!("{}m", seconds / 60)
        }
    };
    Some(
        tr(app.ui_locale, MessageId::BackgroundQuiet)
            .replace("{idle}", &coarse(idle.min(bound)))
            .replace("{bound}", &coarse(bound)),
    )
}

/// Whether `text` names `fact` as a whole token: "step 1" is not said by
/// "step 10", nor a tool `read` by "read_file".
fn names_whole(text: &str, fact: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    !fact.is_empty()
        && text.match_indices(fact).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + fact.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
}

/// Has this agent stopped working? Typed live activity wins over the worker
/// status, which in turn wins over the coarse manager status — the same
/// precedence the row's status label and bucket already use.
fn agent_is_terminal(agent: &SubAgentResult, meta: Option<&AgentProgressMeta>) -> bool {
    meta.and_then(|meta| meta.current_activity.as_ref())
        .map(|activity| {
            matches!(
                activity.status,
                AgentCurrentActivityStatus::Done
                    | AgentCurrentActivityStatus::Canceled
                    | AgentCurrentActivityStatus::Failed
                    | AgentCurrentActivityStatus::Interrupted
            )
        })
        .or_else(|| {
            agent.worker_status.map(|worker_status| {
                matches!(
                    worker_status,
                    AgentWorkerStatus::Completed
                        | AgentWorkerStatus::Cancelled
                        | AgentWorkerStatus::Failed
                        | AgentWorkerStatus::Interrupted
                )
            })
        })
        .unwrap_or(matches!(
            agent.status,
            SubAgentStatus::Completed
                | SubAgentStatus::Cancelled
                | SubAgentStatus::Failed(_)
                | SubAgentStatus::Interrupted(_)
                | SubAgentStatus::BudgetExhausted
        ))
}

/// Latch each finished agent's elapsed time the first frame it is observed
/// terminal, and forget agents that have left the cache.
///
/// The manager recomputes `SubAgentResult::duration_ms` as
/// `started_at.elapsed()` on every snapshot, so a completed agent's duration
/// keeps growing for as long as it stays listed. Without this pass a finished
/// row would tick forever, which is exactly the thing a receipt must not do.
fn freeze_terminal_agent_elapsed(app: &mut App) {
    let live: HashSet<&str> = app
        .subagent_cache
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    app.work_surface
        .frozen_agent_elapsed_ms
        .retain(|id, _| live.contains(id.as_str()));

    for agent in &app.subagent_cache {
        if !agent_is_terminal(agent, app.agent_progress_meta.get(&agent.agent_id)) {
            continue;
        }
        app.work_surface
            .frozen_agent_elapsed_ms
            .entry(agent.agent_id.clone())
            .or_insert(agent.duration_ms);
    }
}

/// Frozen elapsed for a finished agent, live elapsed for a running one.
fn agent_elapsed_ms(app: &App, agent: &crate::tools::subagent::SubAgentResult) -> u64 {
    // Live ticking (4b): derive from start timestamp at render when running,
    // otherwise use frozen snapshot. The redraw already happens; stale cached
    // duration is the bug.
    if matches!(
        agent.status,
        crate::tools::subagent::SubAgentStatus::Running
    ) && let Some(started_at) = agent.started_at
    {
        return u64::try_from(started_at.elapsed().as_millis()).unwrap_or(agent.duration_ms);
    }
    app.work_surface
        .frozen_agent_elapsed_ms
        .get(&agent.agent_id)
        .copied()
        .unwrap_or(agent.duration_ms)
}

fn current_activity_status_bucket(status: AgentCurrentActivityStatus) -> WorkBucket {
    match status {
        AgentCurrentActivityStatus::Waiting
        | AgentCurrentActivityStatus::Interrupted
        | AgentCurrentActivityStatus::Failed => WorkBucket::Attention,
        // Not Attention (#5906): a parked husk asked nobody anything, so it
        // must not sort above live work nor be counted in the `needs input`
        // chip. Ready ranks below Attention and Active and stays actionable,
        // which is what it is — resumable, or dismissable.
        AgentCurrentActivityStatus::Parked | AgentCurrentActivityStatus::Queued => {
            WorkBucket::Ready
        }
        AgentCurrentActivityStatus::Done | AgentCurrentActivityStatus::Canceled => {
            WorkBucket::Recent
        }
        AgentCurrentActivityStatus::Starting
        | AgentCurrentActivityStatus::Running
        | AgentCurrentActivityStatus::ModelWait
        | AgentCurrentActivityStatus::RunningTool => WorkBucket::Active,
    }
}

fn current_activity_status_label(
    status: AgentCurrentActivityStatus,
    locale: codewhale_localization::Locale,
) -> std::borrow::Cow<'static, str> {
    // `parked` is the one word here that has to be translated: it is new
    // vocabulary a reader has never seen on this row, and the whole point is
    // that it does not read as "waiting for input" (#5906).
    if status == AgentCurrentActivityStatus::Parked {
        return codewhale_localization::tr(
            locale,
            codewhale_localization::MessageId::AgentStatusParked,
        );
    }
    std::borrow::Cow::Borrowed(match status {
        AgentCurrentActivityStatus::Queued => "queued",
        AgentCurrentActivityStatus::Starting => "starting",
        AgentCurrentActivityStatus::Running => "running",
        AgentCurrentActivityStatus::ModelWait => "waiting for model",
        AgentCurrentActivityStatus::RunningTool => "running tool",
        AgentCurrentActivityStatus::Waiting => "waiting for input",
        AgentCurrentActivityStatus::Done => "completed",
        AgentCurrentActivityStatus::Failed => "failed",
        AgentCurrentActivityStatus::Canceled => "cancelled",
        AgentCurrentActivityStatus::Interrupted => "interrupted",
        AgentCurrentActivityStatus::Parked => unreachable!("handled above"),
    })
}

fn worker_status_bucket(status: AgentWorkerStatus) -> WorkBucket {
    match status {
        AgentWorkerStatus::WaitingForUser
        | AgentWorkerStatus::Interrupted
        | AgentWorkerStatus::Failed => WorkBucket::Attention,
        AgentWorkerStatus::Queued => WorkBucket::Ready,
        AgentWorkerStatus::Completed | AgentWorkerStatus::Cancelled => WorkBucket::Recent,
        AgentWorkerStatus::Starting
        | AgentWorkerStatus::Running
        | AgentWorkerStatus::ModelWait
        | AgentWorkerStatus::RunningTool => WorkBucket::Active,
    }
}

fn worker_status_label(status: AgentWorkerStatus) -> &'static str {
    match status {
        AgentWorkerStatus::Queued => "queued",
        AgentWorkerStatus::Starting => "starting",
        AgentWorkerStatus::Running => "running",
        AgentWorkerStatus::WaitingForUser => "waiting for input",
        AgentWorkerStatus::ModelWait => "waiting for model",
        AgentWorkerStatus::RunningTool => "running tool",
        AgentWorkerStatus::Completed => "completed",
        AgentWorkerStatus::Failed => "failed",
        AgentWorkerStatus::Cancelled => "cancelled",
        AgentWorkerStatus::Interrupted => "interrupted",
    }
}

fn subagent_status_bucket(status: &SubAgentStatus) -> WorkBucket {
    match status {
        SubAgentStatus::Running => WorkBucket::Active,
        SubAgentStatus::Interrupted(_)
        | SubAgentStatus::Failed(_)
        | SubAgentStatus::BudgetExhausted => WorkBucket::Attention,
        SubAgentStatus::Completed | SubAgentStatus::Cancelled => WorkBucket::Recent,
    }
}

fn subagent_status_label(status: &SubAgentStatus) -> &'static str {
    match status {
        SubAgentStatus::Running => "running",
        SubAgentStatus::Completed => "completed",
        SubAgentStatus::Interrupted(_) => "interrupted",
        SubAgentStatus::Failed(_) => "failed",
        SubAgentStatus::Cancelled => "cancelled",
        SubAgentStatus::BudgetExhausted => "budget exhausted",
    }
}

/// `WorkBucket::Attention` deliberately groups a wait with a failure so they
/// sort together — both need you. Tone must not follow it that far: only an
/// actual failure spends Failure red.
const fn agent_tone(bucket: WorkBucket, failed: bool) -> WorkTone {
    match bucket {
        WorkBucket::Attention if failed => WorkTone::Failure,
        other => bucket_tone(other),
    }
}

const fn bucket_tone(bucket: WorkBucket) -> WorkTone {
    match bucket {
        WorkBucket::Active => WorkTone::Live,
        WorkBucket::Attention => WorkTone::Attention,
        WorkBucket::Ready => WorkTone::Muted,
        WorkBucket::Recent => WorkTone::Success,
    }
}

const fn agent_mark(bucket: WorkBucket) -> &'static str {
    match bucket {
        WorkBucket::Active => crate::tui::glyphs::SELECTION,
        WorkBucket::Attention => crate::tui::glyphs::ATTENTION,
        WorkBucket::Ready => crate::tui::glyphs::READY,
        WorkBucket::Recent => crate::tui::glyphs::DONE,
    }
}

/// Cache key for the settled file-activity aggregates: every input
/// [`compute_settled_file_activity`] reads. History cell and active-entry
/// mutations bump `history_version`; detail-map mutations bump
/// `details_epoch` ([`App::bump_details_epoch`]). A stale FILES view means
/// one of these changed without its bump.
#[derive(PartialEq, Eq, Clone, Debug)]
pub(super) struct FileActivityStamp {
    history_version: u64,
    details_epoch: u64,
    inline_diff_mode: InlineDiffMode,
    workspace: std::path::PathBuf,
}

/// Settled file-activity aggregates for the FILES view and tab badges,
/// computed once per input change instead of once per frame (#6565).
pub(super) fn settled_file_activity(app: &mut App) -> &SettledFileActivity {
    let stamp = FileActivityStamp {
        history_version: app.history_version,
        details_epoch: app.details_epoch,
        inline_diff_mode: app.inline_diff_mode,
        workspace: app.workspace.clone(),
    };
    if app.work_surface.file_activity_stamp.as_ref() != Some(&stamp) {
        app.work_surface.file_activity = compute_settled_file_activity(app);
        app.work_surface.file_activity_stamp = Some(stamp);
    }
    &app.work_surface.file_activity
}

/// Uncached full scan of tool detail records; the correctness reference the
/// cache above must always reproduce.
pub(super) fn compute_settled_file_activity(app: &App) -> SettledFileActivity {
    let mut activity = SettledFileActivity {
        inline_diff_mode: app.inline_diff_mode,
        ..SettledFileActivity::default()
    };
    // Visit only cells with details, in transcript order. Committed details win;
    // stable sorting preserves the resolver's first active match at each index.
    let mut records: Vec<_> = app
        .tool_details_by_cell
        .iter()
        .map(|(&index, detail)| (index, detail))
        .chain(app.active_tool_details.values().filter_map(|detail| {
            let &index = app.tool_cells.get(&detail.tool_id)?;
            (!app.tool_details_by_cell.contains_key(&index)).then_some((index, detail))
        }))
        .collect();
    records.sort_by_key(|(index, _)| *index);
    records.dedup_by_key(|(index, _)| *index);
    let mut seen = HashSet::with_capacity(records.len());
    for (index, detail) in records {
        let Some(HistoryCell::Tool(cell)) = app.cell_at_virtual_index(index) else {
            continue;
        };
        if !cell.is_success() {
            continue;
        }
        let activity_tool_name = canonical_action_alias(&detail.tool_name, &detail.input);
        let kind = if matches!(cell, ToolCell::PatchSummary(_)) {
            Some(FileActivityKind::Write)
        } else {
            FileActivitySummary::from_tool_name(activity_tool_name)
        };
        let Some(kind) = kind else {
            continue;
        };
        if !seen.insert(detail.tool_id.as_str()) {
            continue;
        }
        activity.summary.record(kind);
        if kind == FileActivityKind::Write
            && let ToolCell::PatchSummary(mutation) = cell
            && let Some(receipt) = mutation.receipt.as_ref()
        {
            let additional_files =
                u32::try_from(receipt.files.len().saturating_sub(1)).unwrap_or(u32::MAX);
            activity.summary.files_written = activity
                .summary
                .files_written
                .saturating_add(additional_files);
            activity.mutations.push(receipt.clone());
        }
        let target = activity_target(&app.workspace, activity_tool_name, &detail.input, kind);
        let details = match kind {
            FileActivityKind::Read => &mut activity.read,
            FileActivityKind::List => &mut activity.list,
            FileActivityKind::Search => &mut activity.search,
            FileActivityKind::Write => &mut activity.write,
        };
        if let Some(target) = target
            && !details.iter().any(|existing| existing.as_str() == &*target)
        {
            details.push(target.into_owned());
        }
    }
    activity
}

fn aggregate_activity_row(activity: &SettledFileActivity) -> Option<RankedWorkRow> {
    if activity.is_empty() {
        return None;
    }
    let summaries = activity.summary.compact_display();
    if summaries.is_empty() {
        return None;
    }
    // Single aggregated live receipt; never inline raw patterns/commands (#4690).
    let label = if summaries.len() == 1 {
        summaries[0].clone()
    } else {
        summaries.join(" · ")
    };
    let mutation_detail = activity.mutations.last().map(|receipt| {
        if activity.inline_diff_mode == InlineDiffMode::Off {
            receipt.outcome_label()
        } else {
            receipt.semantic_summary()
        }
    });
    let mutation_body = settled_mutation_body(&activity.mutations, activity.inline_diff_mode);
    let mut body_parts = Vec::new();
    if !mutation_body.is_empty() {
        body_parts.push(mutation_body);
    }
    for (kind, details) in [
        ("Read", &activity.read),
        ("List", &activity.list),
        ("Search", &activity.search),
        ("Write", &activity.write),
    ] {
        if details.is_empty() {
            continue;
        }
        body_parts.push(format!(
            "{kind}:\n{}",
            details[..details.len().min(12)].join("\n")
        ));
    }
    if body_parts.is_empty() {
        body_parts.push("No safe target detail retained".to_string());
    }
    let detail = mutation_detail
        .or_else(|| {
            activity
                .write
                .first()
                .cloned()
                .or_else(|| activity.read.first().cloned())
                .or_else(|| activity.search.first().map(|_| "patterns".to_string()))
                .or_else(|| activity.list.first().cloned())
        })
        .unwrap_or_else(|| "settled".to_string());
    let detail = sanitize_summary_title(&detail);
    Some(RankedWorkRow {
        bucket: WorkBucket::Recent,
        order: 20_000,
        is_plan_step: false,
        row: WorkRow {
            id: WorkRowId("activity:aggregate".to_string()),
            mark: crate::tui::glyphs::DONE,
            label,
            detail,
            tone: WorkTone::Success,
            selectable: true,
            primary_action: Some(SidebarRowAction::InspectWork {
                title: "Work · file activity".to_string(),
                body: body_parts.join("\n\n"),
                stop_action: None,
            }),
            agent: None,
        },
    })
}

#[cfg(test)]
fn activity_rows(activity: SettledFileActivity) -> Vec<RankedWorkRow> {
    aggregate_activity_row(&activity).into_iter().collect()
}

pub(super) fn settled_mutation_body(
    receipts: &[FileMutationReceipt],
    mode: InlineDiffMode,
) -> String {
    let Some(receipt) = receipts.last() else {
        return String::new();
    };
    let details = crate::tui::key_shortcuts::tool_details_shortcut_action_hint(
        "exact change evidence on the matching File receipt",
    );
    let hint = format!("Select the matching File receipt; {details}.");
    match mode {
        InlineDiffMode::Off => format!("{}\n\n{hint}", receipt.outcome_label()),
        InlineDiffMode::Summary => format!("{}\n\n{hint}", receipt.semantic_summary()),
        InlineDiffMode::Full => {
            let diff = receipt
                .display_diff
                .lines()
                .take(40)
                .collect::<Vec<_>>()
                .join("\n");
            if diff.trim().is_empty() {
                format!("{}\n\n{hint}", receipt.semantic_summary())
            } else {
                format!("{}\n\n{diff}\n\n{hint}", receipt.semantic_summary())
            }
        }
    }
}

fn activity_target<'a>(
    workspace: &'a Path,
    tool_name: &str,
    input: &'a serde_json::Value,
    kind: FileActivityKind,
) -> Option<std::borrow::Cow<'a, str>> {
    if tool_name == "apply_patch"
        && let Ok(preflight) = crate::tools::apply_patch::preflight_apply_patch(input)
    {
        let targets = preflight
            .touched_files
            .iter()
            .filter_map(|path| privacy_safe_path(workspace, path))
            .take(4)
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            return Some(std::borrow::Cow::Owned(targets.join(", ")));
        }
    }
    let keys: &[&str] = match kind {
        FileActivityKind::Search => &["pattern", "query", "path"],
        _ => &["path", "file_path"],
    };
    keys.iter().find_map(|key| {
        let value = input.get(*key)?.as_str()?.trim();
        if value.is_empty() {
            return None;
        }
        if kind == FileActivityKind::Search && *key != "path" {
            return Some(std::borrow::Cow::Owned(safe_pattern(value)));
        }
        privacy_safe_path(workspace, value)
    })
}

fn privacy_safe_path<'a>(workspace: &'a Path, raw: &'a str) -> Option<std::borrow::Cow<'a, str>> {
    // Resolve against the raw spelling (drive/verbatim prefixes) exactly as
    // before; normalization only rewrites separators.
    let absolute = Path::new(raw).is_absolute() || raw.starts_with('/');
    if raw.contains('\\') {
        // Rare: separator normalization allocates, so the target owns bytes.
        let normalized = raw.replace('\\', "/");
        let relative = relative_workspace_path(workspace, &normalized, absolute)?;
        (!relative.is_empty()).then(|| std::borrow::Cow::Owned(relative.to_string()))
    } else {
        let relative = relative_workspace_path(workspace, raw, absolute)?;
        (!relative.is_empty()).then(|| std::borrow::Cow::Borrowed(relative))
    }
}

/// Workspace-relative `/`-separated slice of `normalized`, or `None` when the
/// path is absolute outside the workspace or carries root/drive/`..` shapes.
/// `relative` is backslash-free UTF-8, so the component shapes
/// `Path::components` would surface are exactly: a leading root, a Windows
/// drive prefix, and `..` traversal segments. Scanning bytes rejects the same
/// inputs without iterating components or allocating.
fn relative_workspace_path<'a>(
    workspace: &'a Path,
    normalized: &'a str,
    absolute: bool,
) -> Option<&'a str> {
    let relative = if absolute {
        let lossy = workspace.to_string_lossy();
        let normalized_workspace = if lossy.contains('\\') {
            std::borrow::Cow::Owned(lossy.replace('\\', "/"))
        } else {
            lossy
        };
        let workspace_prefix = normalized_workspace.trim_end_matches('/');
        if normalized == workspace_prefix {
            ""
        } else {
            normalized
                .strip_prefix(workspace_prefix)
                .and_then(|after| after.strip_prefix('/'))?
        }
    } else {
        normalized
    };
    if relative.starts_with('/') {
        return None;
    }
    if cfg!(windows)
        && relative.len() >= 2
        && relative.as_bytes()[1] == b':'
        && relative.as_bytes()[0].is_ascii_alphabetic()
    {
        return None;
    }
    if relative.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(relative)
}

fn safe_pattern(raw: &str) -> String {
    let single_line = raw.replace(['\n', '\r', '\t'], " ");
    let mut chars = single_line.chars();
    let prefix = chars.by_ref().take(80).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn is_settled_transient_operation(node: &WorkNode) -> bool {
    node.kind == NodeKind::Operation
        && node
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.durable)
        && match node.state {
            NodeState::Completed => node.acceptance.is_empty(),
            NodeState::Verified | NodeState::Superseded | NodeState::Cancelled => true,
            _ => false,
        }
}

fn section_heading(id: &str, label: &str, detail: &str) -> WorkRow {
    WorkRow {
        id: WorkRowId(format!("section:{id}")),
        mark: "▾",
        label: label.to_string(),
        detail: detail.to_string(),
        tone: WorkTone::Heading,
        selectable: false,
        primary_action: None,
        agent: None,
    }
}

/// The sub-agent heading is a real group door: selecting it reveals the full
/// Agents panel, including settled workers whose exact transcripts remain
/// available after the compact strip archives them.
fn agents_section_heading(label: &str) -> WorkRow {
    WorkRow {
        id: WorkRowId("section:agents".to_string()),
        mark: "▾",
        label: label.to_string(),
        detail: "Open the full subagent register".to_string(),
        tone: WorkTone::Heading,
        selectable: true,
        primary_action: Some(SidebarRowAction::ShowSubagentsPanel),
        agent: None,
    }
}

fn with_live_shell_rows(app: &App, mut rows: Vec<WorkRow>) -> Vec<WorkRow> {
    rows.retain(|row| !row.id.0.starts_with("shell:") && row.id.0 != "section:shells");
    let shells = shell_work_rows(app);
    if shells.is_empty() {
        return rows;
    }
    let mut out = Vec::with_capacity(rows.len() + shells.len() + 1);
    push_shell_group(&mut out, shells);
    out.extend(rows);
    out
}

fn push_shell_group(out: &mut Vec<WorkRow>, shells: Vec<WorkRow>) {
    if shells.is_empty() {
        return;
    }
    out.push(shells_section_heading(&shells));
    out.extend(shells);
}

fn shell_work_rows(app: &App) -> Vec<WorkRow> {
    app.task_panel
        .iter()
        .filter(|entry| is_live_shell_entry(entry))
        .map(|entry| {
            let command = shell_command(entry);
            let command = command.as_str();
            let status = if entry.stale {
                "stale"
            } else {
                entry.status.as_str()
            };
            let elapsed_secs = entry.duration_ms.map(|ms| ms / 1_000);
            let objective = command.to_string();
            WorkRow {
                id: WorkRowId(format!("shell:{}", entry.id)),
                mark: agent_mark(WorkBucket::Active),
                // The command is what a person recognises; the id stays in
                // the detail and the inspector (#6565).
                label: objective.clone(),
                detail: format!("{status} · {}", entry.id),
                tone: WorkTone::Live,
                selectable: true,
                primary_action: shell_show_action(entry),
                agent: Some(AgentRowFacts {
                    role_label: "shell".to_string(),
                    status: status.to_string(),
                    objective,
                    elapsed_secs,
                    model: None,
                    tokens: None,
                    todos_remaining: None,
                    // Only live shells reach this list; the agents counter
                    // never sees a `shell:` row either way.
                    holds_dock_open: true,
                }),
            }
        })
        .collect()
}

fn shells_section_heading(shells: &[WorkRow]) -> WorkRow {
    let ids: Vec<&str> = shells
        .iter()
        .filter_map(|row| row.id.0.strip_prefix("shell:"))
        .collect();
    let listing = if ids.is_empty() {
        String::new()
    } else {
        format!(
            "{}\n\n",
            ids.iter()
                .map(|id| format!("- {id}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    WorkRow {
        id: WorkRowId("section:shells".to_string()),
        mark: "▾",
        label: format!("Shells {}", shells.len()),
        detail: "Watch live output · cancel by the id on the row".to_string(),
        tone: WorkTone::Heading,
        selectable: true,
        primary_action: Some(SidebarRowAction::InspectWork {
            title: format!("Shells {}", shells.len()),
            body: format!(
                "Running shells ({})\n\n{listing}Open a row to watch output. Cancel with /jobs cancel <id> or /jobs cancel all.",
                shells.len()
            ),
            stop_action: Some(Box::new(SidebarRowAction::Command(
                "/jobs cancel all".to_string(),
            ))),
        }),
        agent: None,
    }
}

fn graph_node_row(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> WorkRow {
    let (mark, tone) = match node.state {
        NodeState::Ready => (crate::tui::glyphs::READY, WorkTone::Muted),
        NodeState::Initializing => (crate::tui::glyphs::SELECTION, WorkTone::Live),
        NodeState::Active => (crate::tui::glyphs::SELECTION, WorkTone::Live),
        NodeState::Waiting => (crate::tui::glyphs::ATTENTION, WorkTone::Attention),
        NodeState::Blocked => (
            status_mark(StatusKind::Attention).glyph,
            WorkTone::Attention,
        ),
        NodeState::Completed if node.acceptance.is_empty() => {
            (status_mark(StatusKind::Done).glyph, WorkTone::Success)
        }
        NodeState::Completed => (
            status_mark(StatusKind::Attention).glyph,
            WorkTone::Attention,
        ),
        NodeState::Verified => (status_mark(StatusKind::Done).glyph, WorkTone::Success),
        NodeState::Stale => ("?", WorkTone::Attention),
        NodeState::Superseded | NodeState::Cancelled => ("−", WorkTone::Muted),
        NodeState::Failed => (crate::tui::glyphs::FAILED, WorkTone::Failure),
    };
    let state = state_label(node);
    let kind = kind_label(node.kind);
    // A to-do row always carries its status word in the detail column, using
    // the same vocabulary as `/task digest` (pending / in progress /
    // completed / cancelled). Only the redundant `· plan step` KIND suffix is
    // dropped — the strip's checkbox marks already say the row is a plan
    // step, but they do not say its state in words, and dropping the state
    // itself was the 0.9.4 regression (a pending to-do rendered no label at
    // all). Non-step nodes keep the state · kind pair.
    let detail = if node.kind == NodeKind::PlanStep {
        todo_state_label(node).to_string()
    } else {
        format!("{state} · {kind}")
    };
    let stop_action = node
        .state
        .is_live()
        .then(|| stop_action(node.binding.as_ref()))
        .flatten();
    WorkRow {
        id: WorkRowId(format!("graph:{}", node.id.as_str())),
        mark,
        label: node.title.clone(),
        detail,
        tone,
        selectable: true,
        primary_action: Some(SidebarRowAction::InspectWork {
            title: format!("Work · {}", node.title),
            body: inspector_text(snapshot, node),
            stop_action: stop_action.map(Box::new),
        }),
        agent: None,
    }
}

/// Status word for a plan-step (to-do) row, aligned with the four-state
/// To-do vocabulary the `/task digest` text surface uses. Graph-only states
/// keep their graph names.
fn todo_state_label(node: &WorkNode) -> &'static str {
    match node.state {
        NodeState::Ready => "pending",
        NodeState::Initializing | NodeState::Active => "in progress",
        _ => state_label(node),
    }
}

fn state_label(node: &WorkNode) -> &'static str {
    match node.state {
        NodeState::Ready => "ready",
        NodeState::Initializing => "initializing",
        NodeState::Active => "running",
        NodeState::Waiting => "waiting",
        NodeState::Blocked => "blocked",
        NodeState::Completed if node.acceptance.is_empty() => "completed",
        NodeState::Completed => "completed · evidence pending",
        NodeState::Verified => "verified",
        NodeState::Stale => "stale",
        NodeState::Superseded => "superseded",
        NodeState::Cancelled => "cancelled",
        NodeState::Failed => "failed",
    }
}

const fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Objective => "objective",
        NodeKind::PlanStep => "plan step",
        NodeKind::Operation => "operation",
        NodeKind::Evidence => "evidence",
        NodeKind::Blocker => "blocker",
        NodeKind::Approval => "approval",
        NodeKind::RuntimeRef => "runtime",
        NodeKind::LaneRef => "lane",
    }
}

fn stop_action(binding: Option<&OperationBinding>) -> Option<SidebarRowAction> {
    let binding = binding?;
    if let Some(id) = binding.external.strip_prefix("task:") {
        Some(SidebarRowAction::Command(format!("/task cancel {id}")))
    } else if let Some(id) = binding.external.strip_prefix("shell:") {
        Some(SidebarRowAction::Command(format!("/jobs cancel {id}")))
    } else if let Some(id) = binding.external.strip_prefix("worker:") {
        Some(SidebarRowAction::CancelAgent {
            agent_id: id.to_string(),
        })
    } else {
        binding
            .external
            .strip_prefix("workflow:")
            .map(|id| SidebarRowAction::Command(format!("/workflow cancel {id}")))
    }
}

fn inspector_text(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> String {
    let mut out = String::new();
    section_text(
        &mut out,
        "Objective",
        objective_for(snapshot, node)
            .as_deref()
            .unwrap_or("Not connected"),
    );
    section_list(
        &mut out,
        "Prerequisites",
        related_nodes(snapshot, node, EdgeKind::DependsOn, true),
    );
    section_text(
        &mut out,
        "Current",
        &format!("{} · {}", state_label(node), kind_label(node.kind)),
    );
    section_list(
        &mut out,
        "Downstream impact",
        related_nodes(snapshot, node, EdgeKind::DependsOn, false),
    );
    section_text(&mut out, "Binding + lifecycle owner", &binding_text(node));
    section_text(
        &mut out,
        "Evidence vs acceptance",
        &evidence_text(snapshot, node),
    );
    section_text(
        &mut out,
        "Blockers / approvals",
        &blockers_approvals_text(snapshot, node),
    );
    section_text(&mut out, "Why next", &why_next(snapshot, node));
    section_text(
        &mut out,
        "Provenance + last reconcile",
        &provenance_text(node),
    );
    if node.state == NodeState::Stale {
        section_text(
            &mut out,
            "Last bounded output",
            last_output_ref(snapshot, node)
                .as_deref()
                .unwrap_or("No output receipt"),
        );
    }
    out.trim_end().to_string()
}

fn objective_for(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> Option<String> {
    if node.kind == NodeKind::Objective {
        return Some(node.title.clone());
    }
    let mut current = node.id.clone();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(parent) = snapshot.edges.iter().find_map(|edge| {
            (edge.kind == EdgeKind::Contains && edge.to == current).then(|| edge.from.clone())
        }) else {
            break;
        };
        let Some(parent_node) = snapshot.node(&parent) else {
            break;
        };
        if parent_node.kind == NodeKind::Objective {
            return Some(parent_node.title.clone());
        }
        current = parent;
    }
    snapshot.compat.plan.objective.clone()
}

fn related_nodes(
    snapshot: &WorkGraphSnapshot,
    node: &WorkNode,
    kind: EdgeKind,
    outgoing: bool,
) -> Vec<String> {
    snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == kind)
        .filter_map(|edge| {
            let related = if outgoing && edge.from == node.id {
                Some(&edge.to)
            } else if !outgoing && edge.to == node.id {
                Some(&edge.from)
            } else {
                None
            }?;
            snapshot
                .node(related)
                .map(|related| format!("{} · {}", related.title, state_label(related)))
        })
        .collect()
}

fn binding_text(node: &WorkNode) -> String {
    let Some(binding) = node.binding.as_ref() else {
        return "Not bound".to_string();
    };
    let mut text = format!(
        "Owner: {}\nDurable: {}",
        binding.external,
        if binding.durable { "yes" } else { "no" }
    );
    if let Some(observation) = binding.last_observation.as_ref() {
        let owner_state = match observation.owner_state {
            OwnerState::Initializing => "initializing",
            OwnerState::Running => "running",
            OwnerState::Waiting => "waiting",
            OwnerState::Completed => "completed",
            OwnerState::Degraded => "degraded (finished with dropped slots)",
            OwnerState::Failed => "failed",
            OwnerState::Cancelled => "cancelled",
        };
        let _ = write!(
            text,
            "\nLast owner state: {owner_state}\nLast reconcile: {} ms UTC · sequence {}",
            observation.observed_at, observation.seq
        );
    } else {
        text.push_str("\nLast reconcile: never");
    }
    text
}

fn evidence_text(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> String {
    let acceptance = if node.acceptance.is_empty() {
        vec!["- No evidence requirement".to_string()]
    } else {
        node.acceptance
            .iter()
            .map(|requirement| format!("- {}", acceptance_label(requirement)))
            .collect()
    };
    let evidence = evidence_for(snapshot, node);
    let evidence = if evidence.is_empty() {
        vec!["- None attached".to_string()]
    } else {
        evidence
            .into_iter()
            .map(|evidence| {
                let reference = evidence
                    .evidence
                    .as_ref()
                    .map(|item| item.reference())
                    .unwrap_or("invalid evidence node");
                format!("- {reference} · {}", state_label(evidence))
            })
            .collect()
    };
    format!(
        "Acceptance:\n{}\nEvidence:\n{}",
        acceptance.join("\n"),
        evidence.join("\n")
    )
}

fn acceptance_label(requirement: &AcceptanceRequirement) -> String {
    match requirement {
        AcceptanceRequirement::EvidenceOfKind { kind } => {
            let kind = match kind {
                EvidenceKindTag::ToolRun => "tool run",
                EvidenceKindTag::Artifact => "artifact",
                EvidenceKindTag::TestSummary => "test summary",
                EvidenceKindTag::Receipt => "receipt",
                EvidenceKindTag::Approval => "approval",
                EvidenceKindTag::Route => "route",
                EvidenceKindTag::WebCitation => "web citation",
            };
            format!("evidence of kind {kind}")
        }
    }
}

fn evidence_for<'a>(snapshot: &'a WorkGraphSnapshot, node: &WorkNode) -> Vec<&'a WorkNode> {
    snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Verifies && edge.to == node.id)
        .filter_map(|edge| snapshot.node(&edge.from))
        .collect()
}

fn blockers_approvals_text(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> String {
    let mut lines = Vec::new();
    lines.extend(
        related_nodes(snapshot, node, EdgeKind::Blocks, false)
            .into_iter()
            .map(|item| format!("- Blocked by {item}")),
    );
    lines.extend(
        related_nodes(snapshot, node, EdgeKind::RequiresApproval, true)
            .into_iter()
            .map(|item| format!("- Approval {item}")),
    );
    if node.kind == NodeKind::PlanStep {
        lines.extend(
            snapshot
                .nodes
                .iter()
                .filter(|candidate| candidate.kind == NodeKind::Approval)
                .map(|approval| format!("- {} · {}", approval.title, state_label(approval))),
        );
    }
    if lines.is_empty() {
        "None".to_string()
    } else {
        lines.join("\n")
    }
}

fn why_next(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> String {
    match node.state {
        NodeState::Ready => {
            let pending = related_nodes(snapshot, node, EdgeKind::DependsOn, true);
            if pending.is_empty() {
                "Ready with no recorded prerequisite".to_string()
            } else {
                format!("Ready after: {}", pending.join(", "))
            }
        }
        NodeState::Initializing => "Spawn intent is registered; awaiting owner handle".to_string(),
        NodeState::Active => "Lifecycle owner reports active work".to_string(),
        NodeState::Waiting => "Waiting on an owner or approval".to_string(),
        NodeState::Blocked => "Blocked; resolve the causes above".to_string(),
        NodeState::Completed if !node.acceptance.is_empty() => {
            "Execution ended, but acceptance evidence is still missing".to_string()
        }
        NodeState::Stale => "Owner cannot confirm liveness after reconciliation".to_string(),
        NodeState::Verified => "Acceptance evidence is satisfied".to_string(),
        NodeState::Completed => "Completed with no evidence requirement".to_string(),
        NodeState::Superseded => "A replacement node owns this work".to_string(),
        NodeState::Cancelled => "Cancelled by lifecycle owner".to_string(),
        NodeState::Failed => "Failed; inspect owner output before retrying".to_string(),
    }
}

fn provenance_text(node: &WorkNode) -> String {
    let provenance = match &node.provenance {
        Provenance::Import { ordinal, .. } => ordinal
            .map(|ordinal| format!("legacy import · ordinal {ordinal}"))
            .unwrap_or_else(|| "legacy import".to_string()),
        Provenance::ToolUpdate { tool, call_id } => {
            format!("tool {tool} · call {call_id}")
        }
        Provenance::RuntimeReconcile {
            source,
            observed_at,
        } => format!("runtime {source} · {observed_at} ms UTC"),
        Provenance::UserEdit { proposal_id } => format!("user-approved diff {proposal_id}"),
    };
    let reconcile = node
        .binding
        .as_ref()
        .and_then(|binding| binding.last_observation.as_ref())
        .map(|observation| format!("{} ms UTC", observation.observed_at))
        .unwrap_or_else(|| "never".to_string());
    format!("Source: {provenance}\nLast reconcile: {reconcile}")
}

fn last_output_ref(snapshot: &WorkGraphSnapshot, node: &WorkNode) -> Option<String> {
    node.binding
        .as_ref()
        .and_then(|binding| binding.last_observation.as_ref())
        .and_then(|observation| observation.output.as_ref())
        .map(format_evidence_ref)
        .or_else(|| {
            evidence_for(snapshot, node)
                .into_iter()
                .max_by_key(|evidence| evidence.updated_at)
                .and_then(|evidence| evidence.evidence.as_ref())
                .map(format_evidence_ref)
        })
}

fn format_evidence_ref(evidence: &crate::work_graph::EvidenceRef) -> String {
    let kind = match evidence.kind() {
        EvidenceKind::ToolRun => "tool run".to_string(),
        EvidenceKind::Artifact { .. } => "artifact".to_string(),
        EvidenceKind::TestSummary => "test summary".to_string(),
        EvidenceKind::Receipt { .. } => "receipt".to_string(),
        EvidenceKind::Approval => "approval".to_string(),
        EvidenceKind::Route => "route".to_string(),
        EvidenceKind::WebCitation {
            url, retrieved_at, ..
        } => format!("web citation · {url} · retrieved {retrieved_at}"),
    };
    let bytes = evidence
        .raw_bytes()
        .map(|bytes| format!(" · {bytes} raw bytes"))
        .unwrap_or_default();
    let truncation = if evidence.truncated() {
        " · truncated"
    } else {
        ""
    };
    format!("{} · {kind}{bytes}{truncation}", evidence.reference())
}

fn section_text(out: &mut String, title: &str, body: &str) {
    let _ = writeln!(out, "{title}\n{body}\n");
}

fn section_list(out: &mut String, title: &str, items: Vec<String>) {
    if items.is_empty() {
        section_text(out, title, "None");
    } else {
        section_text(
            out,
            title,
            &items
                .into_iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tools::spec::ToolResult;

    /// #6565: a fact is dropped from a dock row only when the detail names
    /// that exact fact, never a longer one that contains it.
    #[test]
    fn a_fact_is_said_only_by_its_whole_name() {
        assert!(names_whole("step 6: finished tool 'read_file'", "step 6"));
        assert!(names_whole(
            "step 6: finished tool 'read_file'",
            "read_file"
        ));
        assert!(!names_whole("step 10: finished tool 'read_file'", "step 1"));
        assert!(!names_whole("finished tool 'read_file'", "read"));
        assert!(names_whole("read then step 1", "step 1"));
        assert!(!names_whole("anything", ""));
    }
    use crate::tui::app::TuiOptions;
    use crate::tui::history::ToolStatus;
    use crate::tui::tool_routing::{handle_tool_call_complete, handle_tool_call_started};
    use crate::work_graph::{CompatTodoBinding, OperationBinding, WorkNodeId};

    fn test_app() -> App {
        App::new(
            TuiOptions {
                model: "deepseek-v4-flash".to_string(),
                start_in_agent_mode: true,
                ..crate::tui::test_support::test_tui_options(std::path::PathBuf::from(
                    "/workspace/project",
                ))
            },
            &Config::default(),
        )
    }

    fn surface() -> WorkSurfaceState {
        WorkSurfaceState::default()
    }

    fn operation(state: NodeState, suffix: &str) -> WorkNode {
        WorkNode {
            id: WorkNodeId::derive("work-surface-test", suffix),
            kind: NodeKind::Operation,
            title: format!("operation {suffix}"),
            state,
            acceptance: Vec::new(),
            binding: Some(OperationBinding {
                external: format!("shell:{suffix}"),
                durable: false,
                last_observation: None,
            }),
            evidence: None,
            provenance: Provenance::ToolUpdate {
                tool: "test".to_string(),
                call_id: suffix.to_string(),
            },
            created_at: 1,
            updated_at: 1,
        }
    }

    fn running_agent(agent_id: &str) -> SubAgentResult {
        SubAgentResult {
            usage: None,
            name: agent_id.to_string(),
            agent_id: agent_id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: crate::tools::subagent::FleetRole::Worker,
            assignment: crate::tools::subagent::SubAgentAssignment {
                native_preset: None,
                objective: "sweep the lane".to_string(),
                role: Some("builder".to_string()),
            },
            model: "test-model".to_string(),
            nickname: Some(crate::tools::subagent::whale_name_for_id_in_locale(
                agent_id, "en",
            )),
            status: SubAgentStatus::Running,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 0,
            child_route: None,
            result: None,
            steps_taken: 1,
            checkpoint: None,
            needs_input: None,
            duration_ms: 100,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    fn retained_agent_receipt(
        id: &str,
        status: AgentWorkerStatus,
        state: crate::agent_roster::RosterState,
    ) -> crate::agent_roster::AgentRosterRow {
        crate::agent_roster::AgentRosterRow {
            worker_id: id.to_string(),
            display_name: format!("retained {id}"),
            model: "test-model".to_string(),
            state,
            status,
            activity: None,
            outcome: None,
            millis: Some(3_000),
            input_tokens: None,
            output_tokens: None,
            cost_microusd: None,
            steps_taken: 3,
            parent_run_id: None,
            run_id: id.to_string(),
        }
    }

    /// The register merges the retained roster with the live cache; the
    /// counter that opens the dock has to read the same merged view, or a
    /// child blocked on a person goes unseen the moment its 45-second card
    /// expires (or a session restore filters it out of `subagent_cache`).
    #[test]
    fn a_roster_only_waiting_worker_opens_the_dock() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        assert!(app.subagent_cache.is_empty());
        app.agent_roster = vec![retained_agent_receipt(
            "blocked",
            AgentWorkerStatus::WaitingForUser,
            RosterState::Waiting,
        )];

        assert_eq!(live_agent_row_count(&mut app), 1);
        assert!(view_has_work(&mut app, RailPanel::Agents));
    }

    /// The other half of reading the roster: it is retained for an hour, so a
    /// settled receipt must not pin the dock open for the rest of the session.
    /// A parked husk counts as settled too — nothing will answer it (#5906).
    #[test]
    fn retained_settled_receipts_do_not_hold_the_dock_open() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        app.agent_roster = vec![
            retained_agent_receipt("done", AgentWorkerStatus::Completed, RosterState::Done),
            retained_agent_receipt("failed", AgentWorkerStatus::Failed, RosterState::Failed),
            retained_agent_receipt(
                "cancelled",
                AgentWorkerStatus::Cancelled,
                RosterState::Cancelled,
            ),
            retained_agent_receipt(
                "parked",
                AgentWorkerStatus::WaitingForUser,
                RosterState::Parked,
            ),
        ];

        // Every receipt is still readable in the register...
        assert_eq!(
            agents_view_rows(&mut app)
                .iter()
                .filter(|row| row.agent.is_some())
                .count(),
            4
        );
        // ...and none of them re-opens the dock.
        assert_eq!(live_agent_row_count(&mut app), 0);
        assert!(!view_has_work(&mut app, RailPanel::Agents));
    }

    /// #36 nesting is a fact about the *live* projection: `order_agent_seeds`
    /// indents a child only when its parent is on the same surface, and
    /// `(+N)` counts only children the list actually shows.
    #[test]
    fn nesting_survives_while_both_rows_are_live() {
        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        let mut child = running_agent("child");
        child.parent_run_id = Some("parent".to_string());
        app.subagent_cache = vec![running_agent("parent"), child];

        let rows = agents_view_rows(&mut app);
        let label = |id: &str| {
            rows.iter()
                .find(|row| row.id.0 == format!("worker:{id}"))
                .unwrap_or_else(|| panic!("{id} row: {rows:?}"))
                .label
                .clone()
        };
        assert!(label("child").starts_with("↳ "), "{:?}", label("child"));
        assert!(label("parent").contains("(+1)"), "{:?}", label("parent"));
    }

    /// Deliberate, and asserted so it is not "fixed" into a lie: a receipt
    /// that has outlived its live card renders FLAT, with no `↳ ` and no
    /// `(+N)`, even when the roster still knows its `parent_run_id`.
    ///
    /// The register is ordered by the roster (creation order, parked last),
    /// not by the tree, so an indent here would be an adjacency claim the
    /// list cannot keep — exactly the dangling indent `order_agent_seeds`
    /// refuses to draw. Deriving depth here instead would also put a second
    /// depth authority on the same list. Nesting degrades to flat, and it
    /// degrades symmetrically: see the sibling test below.
    #[test]
    fn a_retained_child_renders_flat_under_a_retained_parent() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        let mut child =
            retained_agent_receipt("child", AgentWorkerStatus::Completed, RosterState::Done);
        child.parent_run_id = Some("parent".to_string());
        app.agent_roster = vec![
            retained_agent_receipt("parent", AgentWorkerStatus::Completed, RosterState::Done),
            child,
        ];

        let rows = agents_view_rows(&mut app);
        for id in ["parent", "child"] {
            let label = rows
                .iter()
                .find(|row| row.id.0 == format!("worker:{id}"))
                .unwrap_or_else(|| panic!("{id} row: {rows:?}"))
                .label
                .clone();
            assert!(!label.contains('↳'), "{id}: {label:?}");
            assert!(!label.contains("(+"), "{id}: {label:?}");
        }
    }

    /// The other direction, which is what makes the flattening honest rather
    /// than half-applied: when the PARENT has expired to a receipt and only
    /// the child is still live, the live child flattens too — its parent is
    /// not among the cache seeds, so `order_agent_seeds` leaves it at the top
    /// level. Nesting is present for a pair or absent for a pair; it is never
    /// stamped on one half of one.
    #[test]
    fn a_live_child_flattens_once_its_parent_has_expired_to_a_receipt() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        let mut child = running_agent("child");
        child.parent_run_id = Some("parent".to_string());
        app.subagent_cache = vec![child];
        let mut retained_child =
            retained_agent_receipt("child", AgentWorkerStatus::Running, RosterState::Running);
        retained_child.parent_run_id = Some("parent".to_string());
        app.agent_roster = vec![
            retained_agent_receipt("parent", AgentWorkerStatus::Completed, RosterState::Done),
            retained_child,
        ];

        let rows = agents_view_rows(&mut app);
        let child_label = rows
            .iter()
            .find(|row| row.id.0 == "worker:child")
            .unwrap_or_else(|| panic!("child row: {rows:?}"))
            .label
            .clone();
        assert!(!child_label.contains('↳'), "{child_label:?}");
        let parent_label = rows
            .iter()
            .find(|row| row.id.0 == "worker:parent")
            .unwrap_or_else(|| panic!("parent row: {rows:?}"))
            .label
            .clone();
        assert!(!parent_label.contains("(+"), "{parent_label:?}");
    }

    #[test]
    fn agents_register_retains_expired_receipts_alongside_fresh_live_rows() {
        use crate::agent_roster::RosterState;
        use crate::tui::subagent_routing::reconcile_subagent_activity_state_at;
        use std::time::Duration;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        let mut done = running_agent("done");
        done.status = SubAgentStatus::Completed;
        app.subagent_cache = vec![done, running_agent("live")];
        let receipt =
            retained_agent_receipt("done", AgentWorkerStatus::Completed, RosterState::Done);
        app.agent_roster = vec![
            receipt.clone(),
            receipt,
            // A newer live cache row must win over an older retained receipt.
            retained_agent_receipt("live", AgentWorkerStatus::Completed, RosterState::Done),
        ];
        app.agent_progress
            .insert("done".to_string(), "old progress".to_string());
        let observed = Instant::now();
        reconcile_subagent_activity_state_at(&mut app, observed);
        reconcile_subagent_activity_state_at(&mut app, observed + Duration::from_secs(46));
        assert!(
            !app.subagent_cache
                .iter()
                .any(|agent| agent.agent_id == "done")
        );
        assert!(!app.agent_progress.contains_key("done"));

        let rows = agents_view_rows(&mut app);
        let workers: Vec<_> = rows.iter().filter(|row| row.agent.is_some()).collect();
        assert_eq!(workers.len(), 2, "one row per retained/live worker ID");
        let done = workers
            .iter()
            .find(|row| row.id.0 == "worker:done")
            .unwrap();
        assert_eq!(done.agent.as_ref().unwrap().status, "completed");
        assert_eq!(done.agent.as_ref().unwrap().elapsed_secs, Some(3));
        assert_eq!(done.agent.as_ref().unwrap().tokens, None);
        let live = workers
            .iter()
            .find(|row| row.id.0 == "worker:live")
            .unwrap();
        assert_eq!(live.agent.as_ref().unwrap().status, "running");

        app.subagent_cache[0].status = SubAgentStatus::Completed;
        reconcile_subagent_activity_state_at(&mut app, observed + Duration::from_secs(47));
        reconcile_subagent_activity_state_at(&mut app, observed + Duration::from_secs(93));
        assert!(app.subagent_cache.is_empty());
        assert!(app.agent_progress.is_empty());
        let rows = agents_view_rows(&mut app);
        assert_eq!(rows.iter().filter(|row| row.agent.is_some()).count(), 2);
    }

    #[test]
    fn retained_agents_register_is_bound_to_the_exact_parent_session() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.agent_roster_session_id = Some("original-session".to_string());
        app.agent_roster = vec![retained_agent_receipt(
            "private-worker",
            AgentWorkerStatus::Completed,
            RosterState::Done,
        )];
        app.agent_roster[0].cost_microusd = Some(1_000);
        for owner in [None, Some(""), Some("other-session")] {
            app.current_session_id = owner.map(str::to_string);
            assert!(app.current_agent_roster().is_empty());
            assert!(
                !agents_view_rows(&mut app)
                    .iter()
                    .any(|row| row.agent.is_some())
            );
            assert!(
                !super::super::views::price_rows(&mut app)
                    .iter()
                    .any(|row| row.id.0.starts_with("price:agent:"))
            );
        }
        app.current_session_id = Some("original-session".to_string());
        assert_eq!(app.current_agent_roster().len(), 1);
        assert_eq!(
            agents_view_rows(&mut app)
                .iter()
                .filter(|row| row.agent.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn retained_agents_register_distinguishes_parked_from_waiting_for_input() {
        use crate::agent_roster::RosterState;

        let mut app = test_app();
        app.current_session_id = Some("roster-owner".to_string());
        app.agent_roster_session_id = app.current_session_id.clone();
        app.agent_roster = vec![
            retained_agent_receipt(
                "parked",
                AgentWorkerStatus::WaitingForUser,
                RosterState::Parked,
            ),
            retained_agent_receipt(
                "asked",
                AgentWorkerStatus::WaitingForUser,
                RosterState::Waiting,
            ),
        ];
        let rows = agents_view_rows(&mut app);
        let parked = rows.iter().find(|row| row.id.0 == "worker:parked").unwrap();
        let asked = rows.iter().find(|row| row.id.0 == "worker:asked").unwrap();
        assert_eq!(parked.mark, RosterState::Parked.glyph());
        assert_eq!(parked.tone, WorkTone::Muted);
        assert_ne!(parked.agent.as_ref().unwrap().status, "waiting for input");
        assert_eq!(asked.agent.as_ref().unwrap().status, "waiting for input");
        assert_eq!(asked.tone, WorkTone::Attention);
        assert!(
            matches!(&parked.primary_action, Some(SidebarRowAction::OpenAgentTranscript { agent_id }) if agent_id == "parked")
        );
    }

    /// #5287: the identity column leads with the name the lane was dispatched
    /// under; the whale only names an agent that was dispatched without one.
    #[test]
    fn agent_identity_column_leads_with_the_dispatch_name() {
        let mut app = test_app();
        let mut named = running_agent("agent_named_lane");
        named.name = "branch-triage".to_string();
        app.subagent_cache.push(named);
        app.subagent_cache.push(running_agent("agent_plain_lane"));

        let rows = agent_rows(&app);
        let label = |id: &str| {
            rows.iter()
                .find(|ranked| ranked.row.id.0 == format!("worker:{id}"))
                .unwrap_or_else(|| panic!("row for {id}"))
                .row
                .label
                .clone()
        };
        assert_eq!(label("agent_named_lane"), "branch-triage");
        assert_eq!(
            label("agent_plain_lane"),
            crate::tools::subagent::whale_name_for_id_in_locale("agent_plain_lane", "en")
        );
    }

    #[test]
    fn agent_identity_column_prefers_resolved_profile_over_generated_whale() {
        let mut app = test_app();
        let mut agent = running_agent("agent_flash_lane");
        agent.child_route = Some(crate::tools::subagent::ChildRouteReceipt {
            requested_type: "custom".to_string(),
            requested_profile: Some("DeepSeek V4 Flash".to_string()),
            resolved_profile_id: Some("flash-scout".to_string()),
            profile_origin: Some("fleet:release".to_string()),
            canonical_role: "scout".to_string(),
            provider_id: "deepseek".to_string(),
            model_id: "deepseek-v4-flash-vision-exp".to_string(),
            route_source: "fleet".to_string(),
            fallback_note: None,
            requested_reasoning: "inherit".to_string(),
            effective_reasoning: None,
            runtime_version: "test".to_string(),
            runtime_build_sha: "unknown".to_string(),
        });
        app.subagent_cache.push(agent);

        let row = agent_rows(&app)
            .into_iter()
            .find(|ranked| ranked.row.id.0 == "worker:agent_flash_lane")
            .expect("resolved Fleet row")
            .row;
        assert_eq!(row.label, "flash-scout");
        assert_eq!(
            row.agent.as_ref().map(|facts| facts.role_label.as_str()),
            Some("flash-scout")
        );
    }

    /// After the recent-only TTL suppresses transient receipts, the live
    /// heading must count only the recent rows the live list still shows —
    /// quoting the unfiltered total would claim receipts the reader cannot
    /// see (2026-08-04 adversarial review of the durable-row exemption).
    #[test]
    fn suppressed_transients_leave_the_live_heading_count_honest() {
        let mut plan_step = operation(NodeState::Completed, "shipped-step");
        plan_step.kind = NodeKind::PlanStep;
        plan_step.binding = None;
        let mut transient = operation(NodeState::Completed, "settled-op");
        transient.binding.as_mut().expect("binding").durable = true;
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.compat.todos.push(CompatTodoBinding {
            legacy_id: 1,
            node: plan_step.id.clone(),
            plan_index: None,
        });
        snapshot.nodes = vec![plan_step, transient];

        let mut surface = surface();
        surface.set_presentation_now_ms(0);
        let _ = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        surface.set_presentation_now_ms(RECENT_ONLY_TTL_MS + 1);
        let rows = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        let heading = &rows[0];
        assert!(
            heading.label.contains("1 recent"),
            "live heading counts only the surviving durable row: {}",
            heading.label
        );
        assert!(
            rows.iter().any(|row| row.label.contains("shipped-step")),
            "durable to-do row survives: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.label.contains("settled-op")),
            "transient receipt is suppressed: {rows:?}"
        );
        // The catalog keeps the full rows, so it keeps the full count.
        assert!(
            surface
                .catalog_rows
                .first()
                .is_some_and(|row| row.label.contains("2 recent")),
            "catalog heading keeps the unfiltered count: {:?}",
            surface.catalog_rows.first()
        );
    }

    #[test]
    fn heading_counts_initializing_and_active_operations_as_running() {
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![
            operation(NodeState::Initializing, "initializing"),
            operation(NodeState::Active, "active"),
            operation(NodeState::Ready, "ready"),
        ];

        let rows = graph_rows(
            &mut surface(),
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );

        assert_eq!(
            rows.first().map(|row| row.label.as_str()),
            Some("Work · Running: operation initializing · 2 active")
        );
    }

    #[test]
    fn live_projection_hides_clean_transient_receipts_without_duplicate_todo_group() {
        let todo_id = WorkNodeId::derive("work-surface-test", "todo:1");
        let todo = WorkNode {
            id: todo_id.clone(),
            kind: NodeKind::PlanStep,
            title: "Keep the durable checklist visible".to_string(),
            state: NodeState::Ready,
            acceptance: Vec::new(),
            binding: None,
            evidence: None,
            provenance: Provenance::ToolUpdate {
                tool: "work_update".to_string(),
                call_id: "todo-1".to_string(),
            },
            created_at: 1,
            updated_at: 1,
        };
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![
            operation(NodeState::Completed, "settled"),
            operation(NodeState::Active, "running"),
            todo,
        ];
        snapshot.compat.todos.push(CompatTodoBinding {
            legacy_id: 1,
            node: todo_id,
            plan_index: None,
        });

        let rows = graph_rows(
            &mut surface(),
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();

        assert!(labels.contains(&"operation running"), "{labels:?}");
        assert!(!labels.contains(&"operation settled"), "{labels:?}");
        assert_eq!(
            labels
                .iter()
                .filter(|label| **label == "Keep the durable checklist visible")
                .count(),
            1,
            "one plan node must produce one Work row: {labels:?}"
        );
        assert!(
            !labels.iter().any(|label| label.starts_with("To-do")),
            "the ordered Work projection must not add a duplicate To-do heading: {labels:?}"
        );
        assert!(
            labels.contains(&"Keep the durable checklist visible"),
            "{labels:?}"
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.title == "operation settled"),
            "projection filtering must retain the historical graph receipt"
        );
    }

    /// #6546: clearing the To-do list (an empty `todo_write`) drops the
    /// compat bindings but keeps the old plan-step nodes in the graph. The
    /// rail must follow the projection, not every plan step ever written.
    #[tokio::test]
    async fn cleared_todo_list_leaves_no_plan_step_rows() {
        use crate::tools::todo::{TodoList, TodoStatus};

        let todos = crate::tools::todo::new_shared_todo_list();
        let plan = crate::tools::plan::new_shared_plan_state();
        let work = crate::work_graph::new_shared_work_runtime(todos.clone(), plan);
        let mut written = TodoList::new();
        written.add("stale finished item".to_string(), TodoStatus::Completed);
        work.apply_todo_update("session-6546", "todo_write", &written.snapshot())
            .await
            .expect("write todo");
        work.publish_pending().await.expect("publish write");
        let rows_for = |graph: &WorkGraphSnapshot| {
            graph_rows(
                &mut surface(),
                graph,
                None,
                Vec::new(),
                None,
                SettledFileActivity::default(),
            )
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
        };
        let before = work
            .capture(Some("session-6546"))
            .expect("capture")
            .expect("graph");
        assert!(
            rows_for(&before.graph)
                .iter()
                .any(|label| label.contains("stale finished item")),
            "precondition: the written item renders"
        );

        work.apply_todo_update("session-6546", "todo_write", &TodoList::new().snapshot())
            .await
            .expect("clear todos");
        work.publish_pending().await.expect("publish clear");
        let after = work
            .capture(Some("session-6546"))
            .expect("capture")
            .expect("graph keeps its history");
        assert!(after.todos.is_empty());
        assert!(
            after
                .graph
                .nodes
                .iter()
                .any(|node| node.title == "stale finished item"),
            "precondition: the retired node is still in the graph"
        );
        let labels = rows_for(&after.graph);
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("stale finished item")),
            "cleared To-do item must leave the rail: {labels:?}"
        );
    }

    #[test]
    fn projection_keeps_durable_and_evidence_gated_terminal_operations() {
        let mut durable = operation(NodeState::Completed, "durable");
        durable.binding.as_mut().expect("binding").durable = true;
        let mut failed = operation(NodeState::Failed, "failed");
        failed.binding.as_mut().expect("binding").durable = true;
        let mut evidence_pending = operation(NodeState::Completed, "evidence-pending");
        evidence_pending.acceptance = vec![AcceptanceRequirement::EvidenceOfKind {
            kind: EvidenceKindTag::ToolRun,
        }];
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![durable, failed, evidence_pending];

        let rows = graph_rows(
            &mut surface(),
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();

        for expected in [
            "operation durable",
            "operation failed",
            "operation evidence-pending",
        ] {
            assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
        }
    }

    #[test]
    fn transient_failed_operation_is_recent_while_durable_failure_needs_input() {
        let transient = operation(NodeState::Failed, "shell transient");
        let mut durable = operation(NodeState::Failed, "durable");
        durable.binding.as_mut().expect("binding").durable = true;

        assert_eq!(node_bucket(&transient), WorkBucket::Recent);
        assert_eq!(node_bucket(&durable), WorkBucket::Attention);
    }

    #[test]
    fn projection_orders_attention_before_ready_and_recent() {
        let mut recent = operation(NodeState::Completed, "recent");
        recent.binding.as_mut().expect("binding").durable = true;
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![
            recent,
            operation(NodeState::Ready, "ready"),
            operation(NodeState::Blocked, "blocked"),
            operation(NodeState::Active, "active"),
        ];

        let labels = graph_rows(
            &mut surface(),
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        )
        .into_iter()
        .map(|row| row.label)
        .collect::<Vec<_>>();

        assert_eq!(
            labels,
            [
                "Work · Needs input: operation blocked · 1 blocked",
                "operation blocked",
                "operation active",
                "operation ready",
                "operation recent",
            ]
        );
    }

    #[test]
    fn activity_targets_keep_workspace_relative_paths_and_hide_external_paths() {
        let workspace = Path::new("/workspace/project");
        assert_eq!(
            privacy_safe_path(workspace, "/workspace/project/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            privacy_safe_path(workspace, "/Users/alice/private.txt"),
            None
        );
        assert_eq!(privacy_safe_path(workspace, "../private.txt"), None);
        assert_eq!(safe_pattern("needle\nsecret"), "needle secret");
    }

    #[test]
    fn settled_canonical_file_actions_keep_aggregates_and_safe_targets() {
        let mut app = test_app();
        let calls = [
            ("read", serde_json::json!({"path": "src/read.rs"})),
            ("list", serde_json::json!({"path": "src"})),
            ("search_name", serde_json::json!({"query": "lib.rs"})),
            (
                "search_content",
                serde_json::json!({"pattern": "needle\nprivate", "path": "src"}),
            ),
            (
                "write",
                serde_json::json!({"path": "src/new.rs", "content": "new\n"}),
            ),
            (
                "edit",
                serde_json::json!({
                    "path": "src/edit.rs",
                    "search": "old",
                    "replace": "new"
                }),
            ),
            (
                "patch",
                serde_json::json!({
                    "patch": "diff --git a/src/patch.rs b/src/patch.rs\n--- a/src/patch.rs\n+++ b/src/patch.rs\n@@ -1 +1 @@\n-old\n+new\n"
                }),
            ),
        ];

        for (action, payload) in calls {
            let id = format!("file-{action}");
            let mut input = payload;
            input["action"] = serde_json::json!(action);
            handle_tool_call_started(&mut app, &id, "File", &input);
            handle_tool_call_complete(&mut app, &id, "File", &Ok(ToolResult::success("ok")));
            app.flush_active_cell();
        }

        let activity = settled_file_activity(&mut app).clone();
        assert_eq!(
            activity.summary,
            FileActivitySummary {
                files_read: 1,
                dirs_listed: 1,
                patterns_searched: 2,
                files_written: 3,
            }
        );
        assert_eq!(activity.read, ["src/read.rs"]);
        assert_eq!(activity.list, ["src"]);
        assert_eq!(activity.search, ["lib.rs", "needle private"]);
        assert_eq!(
            activity.write,
            ["src/new.rs", "src/edit.rs", "src/patch.rs"]
        );
    }

    fn force_tool_status(cell: &mut ToolCell, status: ToolStatus) {
        match cell {
            ToolCell::Generic(generic) => generic.status = status,
            ToolCell::Exec(exec) => exec.status = status,
            ToolCell::PatchSummary(patch) => patch.status = status,
            // Reads/lists explore: one aggregate cell per turn whose status
            // derives from its entries, so flip them all.
            ToolCell::Exploring(explore) => {
                for entry in explore.entries.iter_mut() {
                    entry.status = status;
                }
            }
            _ => {}
        }
    }

    /// Every input class the settled file-activity cache keys on must
    /// invalidate it. Each step mutates through the real funnel (routing,
    /// completion, flush, in-place cell mutation, truncation, session
    /// clear, mode toggle) and then asserts the cached aggregates equal a
    /// fresh uncached recompute — a missed epoch bump anywhere in that
    /// chain surfaces here as a stale mismatch, never as a stale view.
    #[test]
    fn settled_file_activity_cache_invalidates_on_every_input_class() {
        let mut app = test_app();

        // Registration + completion + flush: a committed read appears.
        handle_tool_call_started(
            &mut app,
            "cache-read-1",
            "File",
            &serde_json::json!({"action": "read", "path": "src/one.rs"}),
        );
        handle_tool_call_complete(
            &mut app,
            "cache-read-1",
            "File",
            &Ok(ToolResult::success("ok")),
        );
        app.flush_active_cell();
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 1);
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);

        // Second tool through the same funnel: aggregates move 1 -> 2.
        handle_tool_call_started(
            &mut app,
            "cache-read-2",
            "File",
            &serde_json::json!({"action": "read", "path": "src/two.rs"}),
        );
        handle_tool_call_complete(
            &mut app,
            "cache-read-2",
            "File",
            &Ok(ToolResult::success("ok")),
        );
        app.flush_active_cell();
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 2);
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);

        // In-place committed cell mutation (`history_version` class): the
        // flipped status drops that record from the aggregates. Locate the
        // cell through the record map itself — index 0 assumptions don't
        // survive seeded cells in agent mode.
        let flip_index = app
            .tool_details_by_cell
            .iter()
            .find(|(_, detail)| {
                detail.input.get("path").and_then(serde_json::Value::as_str) == Some("src/one.rs")
            })
            .map(|(index, _)| *index)
            .expect("read-1 record to be committed");
        if let Some(HistoryCell::Tool(cell)) = app.cell_at_virtual_index_mut(flip_index) {
            force_tool_status(cell, ToolStatus::Failed);
        }
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 1);
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);

        // Truncation drops everything.
        app.truncate_history_to(0);
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 0);
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);

        // Session-switch clear (mirrors `ui/apply.rs`: maps drained + bump).
        handle_tool_call_started(
            &mut app,
            "cache-read-3",
            "File",
            &serde_json::json!({"action": "read", "path": "src/three.rs"}),
        );
        handle_tool_call_complete(
            &mut app,
            "cache-read-3",
            "File",
            &Ok(ToolResult::success("ok")),
        );
        app.flush_active_cell();
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 1);
        app.tool_cells.clear();
        app.tool_details_by_cell.clear();
        app.active_tool_details.clear();
        app.bump_details_epoch();
        assert_eq!(settled_file_activity(&mut app).summary.files_read, 0);
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);

        // Inline-diff mode ships inside the cached payload itself.
        app.inline_diff_mode = InlineDiffMode::Off;
        assert_eq!(
            settled_file_activity(&mut app).inline_diff_mode,
            InlineDiffMode::Off
        );
        let expect = compute_settled_file_activity(&app);
        assert_eq!(*settled_file_activity(&mut app), expect);
    }

    /// Deterministic random walk over the same mutation funnels: after
    /// every step the cache must equal a fresh recompute. Catches missed
    /// epoch bumps in orderings the per-class test never reaches.
    #[test]
    fn settled_file_activity_cache_tracks_random_mutation_sequences() {
        let mut app = test_app();
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let mut serial = 0usize;
        for step in 0..240 {
            let op = next() % 7;
            match op {
                0 | 1 => {
                    let id = format!("rand-{serial}");
                    serial += 1;
                    handle_tool_call_started(
                        &mut app,
                        &id,
                        "File",
                        &serde_json::json!({"action": "read", "path": format!("src/{id}.rs")}),
                    );
                    if next() % 2 == 0 {
                        handle_tool_call_complete(
                            &mut app,
                            &id,
                            "File",
                            &Ok(ToolResult::success("ok")),
                        );
                    }
                }
                2 => app.flush_active_cell(),
                3 => {
                    let len = app.history.len();
                    if len > 0 {
                        app.truncate_history_to((next() as usize) % (len + 1));
                    }
                }
                4 => {
                    let len = app.history.len();
                    if len > 0 {
                        let index = (next() as usize) % len;
                        let target = if next() % 2 == 0 {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Success
                        };
                        if let Some(HistoryCell::Tool(cell)) = app.cell_at_virtual_index_mut(index)
                        {
                            force_tool_status(cell, target);
                        }
                    }
                }
                5 => {
                    app.inline_diff_mode = match next() % 3 {
                        0 => InlineDiffMode::Full,
                        1 => InlineDiffMode::Summary,
                        _ => InlineDiffMode::Off,
                    };
                }
                _ => {}
            }
            let expect = compute_settled_file_activity(&app);
            assert_eq!(
                *settled_file_activity(&mut app),
                expect,
                "cache diverged from fresh recompute at step {step} (op {op})"
            );
        }
    }

    #[test]
    fn multifile_receipt_counts_semantic_file_outcomes_in_work_label() {
        let mut app = test_app();
        let input = serde_json::json!({
            "action": "patch",
            "patch": "--- a/update.rs\n+++ b/update.rs\n@@ -1 +1 @@\n-old\n+new\n"
        });
        handle_tool_call_started(&mut app, "file-multi", "File", &input);
        let result = ToolResult::success("ok").with_metadata(serde_json::json!({
            "mutation": {
                "diff": "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/update.rs\n+++ b/update.rs\n@@ -1 +1 @@\n-old\n+new\n--- /dev/null\n+++ b/create.rs\n@@ -0,0 +1 @@\n+created\n--- a/delete.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-deleted\n",
                "files": [
                    { "path": "update.rs", "outcome": "updated" },
                    { "path": "create.rs", "outcome": "created" },
                    { "path": "delete.rs", "outcome": "deleted" }
                ],
                "renames": [{ "from": "old.rs", "to": "new.rs" }]
            }
        }));
        handle_tool_call_complete(&mut app, "file-multi", "File", &Ok(result));
        app.flush_active_cell();

        let activity = settled_file_activity(&mut app).clone();
        assert_eq!(activity.summary.files_written, 4);
        let write_row = activity_rows(activity)
            .into_iter()
            .find(|row| row.row.label.starts_with("Wrote"))
            .expect("write row");
        assert_eq!(write_row.row.label, "Wrote 4 files");
        assert_eq!(
            write_row.row.detail,
            "4 files · 1 created · 1 updated · 1 deleted · 1 renamed · +2 -2"
        );
    }

    fn mutation_activity(mode: InlineDiffMode) -> SettledFileActivity {
        let result = ToolResult::success("ok").with_metadata(serde_json::json!({
            "mutation": {
                "diff": "--- /Users/alice/private.rs\n+++ /Users/alice/private.rs\n@@ -1 +1 @@\n-old\n+new\n",
                "files": [{
                    "path": "/Users/alice/private.rs",
                    "outcome": "updated"
                }],
                "renames": []
            }
        }));
        let receipt = FileMutationReceipt::from_success(Path::new("/workspace/project"), &result)
            .expect("receipt");
        SettledFileActivity {
            summary: FileActivitySummary {
                files_written: 1,
                ..FileActivitySummary::default()
            },
            write: vec!["src/public.rs".to_string()],
            mutations: vec![receipt],
            inline_diff_mode: mode,
            ..SettledFileActivity::default()
        }
    }

    fn mutation_activity_body(mode: InlineDiffMode) -> (String, String, String) {
        let row = activity_rows(mutation_activity(mode))
            .into_iter()
            .next()
            .expect("activity row")
            .row;
        let SidebarRowAction::InspectWork { body, .. } =
            row.primary_action.expect("inspect action")
        else {
            panic!("write row must open Work inspection")
        };
        (row.label, row.detail, body)
    }

    #[test]
    fn work_mutation_rows_keep_labels_privacy_and_all_inline_modes() {
        let (label, detail, full) = mutation_activity_body(InlineDiffMode::Full);
        assert_eq!(label, "Wrote 1 files");
        assert_eq!(detail, "Updated <external file> · +1 -1");
        assert!(full.contains("-old"), "{full}");
        assert!(full.contains("+new"), "{full}");
        assert!(!full.contains("alice"), "{full}");
        assert!(full.contains("exact change evidence"), "{full}");

        let (_, _, summary) = mutation_activity_body(InlineDiffMode::Summary);
        assert!(
            summary.contains("Updated <external file> · +1 -1"),
            "{summary}"
        );
        assert!(!summary.contains("-old"), "{summary}");
        assert!(!summary.contains("+new"), "{summary}");
        assert!(!summary.contains("alice"), "{summary}");

        let (_, detail, off) = mutation_activity_body(InlineDiffMode::Off);
        assert_eq!(detail, "Updated <external file>");
        assert!(off.contains("Updated <external file>"), "{off}");
        assert!(!off.contains("+1 -1"), "{off}");
        assert!(!off.contains("-old"), "{off}");
        assert!(!off.contains("alice"), "{off}");
        assert!(off.contains("exact change evidence"), "{off}");
    }

    #[test]
    fn recent_only_summary_expires_after_ttl_and_user_turn() {
        let mut recent = operation(NodeState::Completed, "recent");
        recent.binding.as_mut().expect("binding").durable = true;
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![recent];

        let mut surface = surface();
        surface.set_presentation_now_ms(0);
        let rows = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert!(
            rows.iter().any(|row| row.id.0.starts_with("section:")),
            "recent-only should surface briefly: {rows:?}"
        );

        surface.set_presentation_now_ms(RECENT_ONLY_TTL_MS);
        let expired = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert!(
            expired.is_empty(),
            "recent-only must collapse after TTL: {expired:?}"
        );
        // Catalog retains durable history for inspector/history.
        assert!(
            surface
                .catalog_rows
                .iter()
                .any(|row| row.label == "operation recent"),
            "catalog must keep recent work after live expiry"
        );

        // New completion fingerprint re-surfaces once.
        let mut newer = operation(NodeState::Completed, "newer");
        newer.binding.as_mut().expect("binding").durable = true;
        snapshot.nodes.push(newer);
        surface.set_presentation_now_ms(RECENT_ONLY_TTL_MS + 10);
        let resurfaced = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert!(
            !resurfaced.is_empty(),
            "a new completion may surface once after expiry"
        );

        // User turn hides immediately while still recent-only.
        surface.note_user_turn_or_new_operation();
        surface.set_presentation_now_ms(RECENT_ONLY_TTL_MS + 11);
        let after_turn = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert!(
            after_turn.is_empty(),
            "user turn must hide recent-only immediately: {after_turn:?}"
        );
    }

    #[test]
    fn needs_input_and_ready_never_expire_with_clock() {
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![
            operation(NodeState::Blocked, "blocked"),
            operation(NodeState::Ready, "ready"),
        ];
        let mut surface = surface();
        surface.set_presentation_now_ms(0);
        let _ = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        surface.set_presentation_now_ms(60_000);
        let rows = graph_rows(
            &mut surface,
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert!(
            rows[0].label.starts_with("Work · Needs input:"),
            "{}",
            rows[0].label
        );
        assert!(rows.iter().any(|row| row.label == "operation blocked"));
        assert!(rows.iter().any(|row| row.label == "operation ready"));
    }

    #[test]
    fn files_keep_all_unique_paths_while_activity_summary_is_capped() {
        use super::super::views::{files_rows, files_touched_count};
        let mut app = test_app();
        for action in ["read", "write"] {
            // Repeat the thirteenth path under a new call ID to check uniqueness.
            for index in 0..14 {
                let id = format!("{action}-{index}");
                let path = format!("{action}-{:02}.rs", index.min(12));
                let input = serde_json::json!({"action": action, "path": path, "content": "new"});
                handle_tool_call_started(&mut app, &id, "File", &input);
                handle_tool_call_complete(&mut app, &id, "File", &Ok(ToolResult::success("ok")));
                app.flush_active_cell();
            }
        }
        settled_file_activity(&mut app);
        assert!(app.work_surface.file_activity.mutations.is_empty());
        assert_eq!(files_touched_count(&mut app), 26);
        let rows = files_rows(&mut app);
        assert!(rows.iter().any(|row| row.label == "Read 13"));
        assert!(rows.iter().any(|row| row.label == "Edited 13"));
        for action in ["read", "write"] {
            for index in 0..13 {
                let path = format!("{action}-{index:02}.rs");
                assert_eq!(
                    rows.iter().filter(|row| row.label == path).count(),
                    1,
                    "{path}"
                );
            }
        }
        let summary = aggregate_activity_row(&app.work_surface.file_activity).unwrap();
        let Some(SidebarRowAction::InspectWork { body, .. }) = summary.row.primary_action else {
            panic!("activity summary opens its preview");
        };
        for action in ["read", "write"] {
            assert!(body.contains(&format!("{action}-11.rs")));
            assert!(!body.contains(&format!("{action}-12.rs")));
        }
    }

    #[test]
    fn activity_receipts_aggregate_and_expire_without_raw_payloads() {
        let activity = SettledFileActivity {
            summary: FileActivitySummary {
                files_read: 1,
                patterns_searched: 2,
                files_written: 1,
                ..FileActivitySummary::default()
            },
            read: vec!["src/lib.rs".to_string()],
            search: vec!["(?i)super_secret_pattern_xyz".to_string()],
            write: vec!["src/main.rs".to_string()],
            ..SettledFileActivity::default()
        };
        let mut surface = surface();
        surface.set_presentation_now_ms(0);
        let rows = ordered_rows(&mut surface, None, None, Vec::new(), None, activity.clone());
        let activity_row = rows
            .iter()
            .find(|row| row.id.0 == "activity:aggregate")
            .expect("aggregate activity");
        assert!(activity_row.label.contains("Read 1 files"));
        assert!(activity_row.label.contains("Searched 2 patterns"));
        assert!(!activity_row.label.contains("super_secret_pattern_xyz"));
        assert!(!activity_row.detail.contains("super_secret_pattern_xyz"));

        surface.set_presentation_now_ms(ACTIVITY_RECEIPT_TTL_MS);
        let expired = ordered_rows(&mut surface, None, None, Vec::new(), None, activity);
        assert!(
            expired.iter().all(|row| row.id.0 != "activity:aggregate"),
            "activity receipt must expire after TTL: {expired:?}"
        );
    }

    #[test]
    fn summary_subject_prefers_attention_over_active_and_ready() {
        let mut snapshot = WorkGraphSnapshot::new();
        snapshot.nodes = vec![
            operation(NodeState::Active, "running"),
            operation(NodeState::Blocked, "choose a release target"),
            operation(NodeState::Ready, "review rebuilt binary"),
        ];
        let rows = graph_rows(
            &mut surface(),
            &snapshot,
            None,
            Vec::new(),
            None,
            SettledFileActivity::default(),
        );
        assert_eq!(
            rows[0].label,
            "Work · Needs input: operation choose a release target · 1 blocked"
        );
    }

    fn running_shell_entry(id: &str, command: &str) -> crate::tui::app::TaskPanelEntry {
        crate::tui::app::TaskPanelEntry {
            exit_code: None,
            id: id.to_string(),
            status: "running".to_string(),
            prompt_summary: format!("shell: {command}"),
            duration_ms: Some(42_000),
            kind: crate::tui::app::TaskPanelEntryKind::Shell,
            stale: false,
            elapsed_since_output_ms: None,
            owner_agent_id: None,
            owner_agent_name: None,
            current_tool: None,
            role: None,
            files_touched: 0,
        }
    }

    #[test]
    fn live_shells_are_first_class_background_rows() {
        let mut app = test_app();
        app.work_surface.placement = WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = WorkSurfacePlacement::Top;
        app.task_panel.push(running_shell_entry(
            "shell_a1b2c3d4",
            "cd /workspace/example-project",
        ));
        app.subagent_cache
            .push(running_agent("doc-scout-spec-arch"));

        // Shells never crowd the agents or tasks views; they are the
        // background view, and the dock badges them there.
        let agents = visible_rows_for(&mut app, RailPanel::Agents);
        assert!(
            agents
                .iter()
                .any(|row| row.id.0 == "section:agents" && row.label.contains("Subagents")),
            "subagent visibility must not regress: {agents:?}"
        );
        assert!(!agents.iter().any(|row| row.id.0.starts_with("shell:")));
        let rows = visible_rows_for(&mut app, RailPanel::Background);
        let heading = rows
            .iter()
            .find(|row| row.id.0 == "section:shells")
            .expect("Shells group heading");
        assert_eq!(heading.label, "Shells 1");
        assert!(heading.selectable);
        let shell = rows
            .iter()
            .find(|row| row.id.0 == "shell:shell_a1b2c3d4")
            .expect("navigable shell row");
        // #6565: the command leads; the id stays in the detail.
        assert_eq!(shell.label, "cd /workspace/example-project");
        assert!(shell.detail.contains("shell_a1b2c3d4"), "{}", shell.detail);
        assert!(
            matches!(
                shell.primary_action.as_ref(),
                Some(SidebarRowAction::Command(command)) if command == "/jobs show shell_a1b2c3d4"
            ),
            "shell row must open the live inspector, got {:?}",
            shell.primary_action
        );
        let facts = shell
            .agent
            .as_ref()
            .expect("shell uses the same row columns");
        assert_eq!(facts.role_label, "shell");
        assert_eq!(facts.status, "running");
        assert!(
            facts.objective.contains("example-project"),
            "{}",
            facts.objective
        );
    }

    #[test]
    fn durable_tasks_are_not_promoted_as_shell_rows() {
        let mut app = test_app();
        app.work_surface.placement = WorkSurfacePlacement::Top;
        app.work_surface.effective_placement = WorkSurfacePlacement::Top;
        app.task_panel.push(crate::tui::app::TaskPanelEntry {
            exit_code: None,
            id: "run".to_string(),
            status: "running".to_string(),
            prompt_summary: "background confirmation test".to_string(),
            duration_ms: Some(99_000),
            kind: crate::tui::app::TaskPanelEntryKind::Background,
            stale: false,
            elapsed_since_output_ms: None,
            owner_agent_id: None,
            owner_agent_name: None,
            current_tool: None,
            role: None,
            files_touched: 0,
        });
        let rows = project_visible(&mut app);
        assert!(
            rows.iter()
                .all(|row| !row.id.0.starts_with("shell:") && row.id.0 != "section:shells"),
            "non-shell task_panel entries must not become Shells rows: {rows:?}"
        );
    }

    fn finished_entry(
        id: &str,
        summary: &str,
        status: &str,
        exit_code: Option<i64>,
    ) -> crate::tui::app::TaskPanelEntry {
        crate::tui::app::TaskPanelEntry {
            exit_code,
            status: status.to_string(),
            prompt_summary: summary.to_string(),
            duration_ms: Some(12_000),
            ..running_shell_entry(id, "unused")
        }
    }

    #[test]
    fn finished_shells_and_tasks_sit_muted_under_finished_and_never_count_as_live() {
        // #6565: a finished task looked live and reopened the dock; a
        // finished shell vanished with no trace of how it ended.
        let mut app = test_app();
        app.task_panel
            .push(running_shell_entry("shell_dev", "npm run dev"));
        app.task_panel.push(finished_entry(
            "shell_ok",
            "shell: cargo build",
            "completed",
            Some(0),
        ));
        app.task_panel.push(finished_entry(
            "shell_bad",
            "shell: npm test",
            "failed",
            Some(2),
        ));
        app.task_panel.push(finished_entry(
            "shell_kill",
            "shell: sleep 99",
            "killed",
            None,
        ));
        app.task_panel.push(finished_entry(
            "shell_slow",
            "shell: make e2e",
            "timed_out",
            None,
        ));
        app.task_panel.push(TaskPanelEntry {
            kind: TaskPanelEntryKind::Background,
            ..finished_entry("task_done", "summarize the logs", "completed", None)
        });
        let rows = visible_rows_for(&mut app, RailPanel::Background);
        let ids = rows.iter().map(|row| row.id.0.as_str()).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "section:shells",
                "shell:shell_dev",
                "section:finished",
                "shell:shell_ok",
                "shell:shell_bad",
                "shell:shell_kill",
                "shell:shell_slow",
                "task:task_done",
            ]
        );
        let row = |id: &str| rows.iter().find(|row| row.id.0 == id).unwrap();
        assert_eq!(row("section:finished").label, "Finished 5");
        assert_eq!(row("shell:shell_ok").label, "cargo build");
        assert_eq!(row("shell:shell_ok").detail, "exit 0 · 12s · shell_ok");
        assert_eq!(row("shell:shell_ok").tone, WorkTone::Muted);
        assert_eq!(row("shell:shell_bad").detail, "failed · exit 2 · shell_bad");
        assert_eq!(row("shell:shell_bad").tone, WorkTone::Failure);
        assert_eq!(row("shell:shell_kill").detail, "killed · shell_kill");
        assert_eq!(row("shell:shell_slow").detail, "timed out · shell_slow");
        assert_eq!(row("task:task_done").tone, WorkTone::Muted);
        assert!(
            row("shell:shell_ok")
                .agent
                .as_ref()
                .is_some_and(|facts| !facts.holds_dock_open && facts.role_label == "shell")
        );

        // The running dev server is the only live work.
        assert!(background_has_live_work(&mut app));
        app.task_panel.remove(0);
        assert!(!background_has_live_work(&mut app));
        assert_eq!(auto_work_rows(&mut app), 0);
    }

    #[test]
    fn a_quiet_running_agent_shows_the_engines_idle_clock_and_bound() {
        let mut app = test_app();
        let mut agent = running_agent("agent_quiet");
        agent.idle_ms = Some(125_000);
        agent.heartbeat_timeout_ms = Some(300_000);
        app.subagent_cache.push(agent);
        let detail = |app: &App| {
            agent_rows(app)
                .into_iter()
                .find(|ranked| ranked.row.id.0 == "worker:agent_quiet")
                .expect("row")
                .row
                .detail
        };
        assert!(
            detail(&app).ends_with("quiet 2m · auto-stop at 5m"),
            "{}",
            detail(&app)
        );
        // A tool in flight is never quiet.
        app.agent_progress_meta
            .entry("agent_quiet".to_string())
            .or_default()
            .current_tool = Some("exec_shell".to_string());
        assert!(!detail(&app).contains("quiet"), "{}", detail(&app));
        // Under a minute says nothing.
        app.agent_progress_meta.clear();
        app.subagent_cache[0].idle_ms = Some(20_000);
        assert!(!detail(&app).contains("quiet"), "{}", detail(&app));
    }

    #[test]
    fn progress_after_an_old_snapshot_keeps_a_working_agent_from_reading_quiet() {
        let mut app = test_app();
        let mut agent = running_agent("agent_busy");
        // The snapshot is ten minutes old; its idle clock would read quiet.
        agent.idle_ms = Some(0);
        agent.heartbeat_timeout_ms = Some(300_000);
        app.subagent_cache.push(agent);
        app.subagent_cache_received_at =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(600));
        let detail = |app: &App| {
            agent_rows(app)
                .into_iter()
                .find(|ranked| ranked.row.id.0 == "worker:agent_busy")
                .expect("row")
                .row
                .detail
        };
        assert!(detail(&app).contains("quiet 5m"), "{}", detail(&app));
        // A progress envelope since then, with no tool in flight between
        // read-only tools, means the agent is working.
        app.agent_progress_meta
            .entry("agent_busy".to_string())
            .or_default()
            .last_progress_at = Some(std::time::Instant::now());
        assert!(!detail(&app).contains("quiet"), "{}", detail(&app));
        // The TUI's clock only caps the engine's, never extends it: a child
        // quiet since its last envelope two minutes ago reads 2m.
        app.agent_progress_meta
            .get_mut("agent_busy")
            .expect("meta")
            .last_progress_at =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(125));
        assert!(detail(&app).contains("quiet 2m"), "{}", detail(&app));
    }

    #[test]
    fn background_review_shell_prefixed_tasks_keep_task_actions() {
        let mut app = test_app();
        for (id, summary) in [
            ("task_real", "shell: explain this command"),
            ("shell_legacy", "ordinary task"),
        ] {
            app.task_panel.push(TaskPanelEntry {
                kind: TaskPanelEntryKind::Background,
                prompt_summary: summary.into(),
                ..running_shell_entry(id, "unused")
            });
        }
        assert!(
            app.task_panel
                .iter()
                .all(|entry| !is_live_shell_entry(entry))
        );
        assert_eq!(
            crate::tui::background_indicator::pending_work_from_app(&app)
                .count(crate::tui::background_indicator::PendingItemKind::Task),
            2
        );
        for status in ["running", "completed"] {
            for entry in &mut app.task_panel {
                entry.status = status.into();
            }
            let rows = visible_rows_for(&mut app, RailPanel::Background);
            for id in ["task_real", "shell_legacy"] {
                let row = rows
                    .iter()
                    .find(|row| row.id.0 == format!("task:{id}"))
                    .expect("task row");
                assert!(
                    matches!(&row.primary_action, Some(SidebarRowAction::Command(command)) if command == &format!("/task show {id}"))
                );
            }
        }
    }

    #[test]
    fn background_review_snapshot_tool_is_never_quiet_without_progress_metadata() {
        let app = test_app();
        let mut agent = running_agent("agent_tool");
        agent.idle_ms = Some(125_000);
        agent.heartbeat_timeout_ms = Some(300_000);
        agent.worker_status = Some(AgentWorkerStatus::RunningTool);
        assert_eq!(quiet_fact(&app, &agent, None), None);
        assert_eq!(
            quiet_fact(&app, &agent, Some(&AgentProgressMeta::default())),
            None
        );
        agent.worker_status = None;
        assert!(quiet_fact(&app, &agent, None).is_some());
    }

    #[test]
    fn background_review_finished_rows_and_quiet_copy_use_the_selected_locale() {
        let mut app = test_app();
        app.ui_locale = Locale::Fr;
        app.task_panel.push(finished_entry(
            "shell_kill",
            "shell: sleep 99",
            "killed",
            None,
        ));
        let rows = visible_rows_for(&mut app, RailPanel::Background);
        assert_eq!(rows[0].label, "Terminés 1");
        assert_eq!(rows[0].detail, "Ouvrez une ligne pour voir sa sortie");
        assert!(rows[1].detail.starts_with("arrêté de force"));
        for (status, expected) in [
            ("completed", "code de sortie 0"),
            ("failed", "échec"),
            ("timed_out", "délai dépassé"),
        ] {
            let entry = finished_entry("shell", "shell: command", status, Some(0));
            assert!(finished_shell_outcome(Locale::Fr, &entry).contains(expected));
        }
        for (status, expected) in [("failed", "échec"), ("canceled", "annulation")] {
            app.task_panel = vec![TaskPanelEntry {
                kind: TaskPanelEntryKind::Background,
                ..finished_entry("task", "vérifier", status, None)
            }];
            let rows = finished_background_rows(&app);
            assert_eq!(rows[0].detail, format!("{expected} · 12s · task"));
        }
        let mut agent = running_agent("agent_quiet");
        agent.idle_ms = Some(125_000);
        agent.heartbeat_timeout_ms = Some(300_000);
        assert_eq!(
            quiet_fact(&app, &agent, None).as_deref(),
            Some("sans progrès depuis 2m · arrêt automatique à 5m")
        );
    }
}

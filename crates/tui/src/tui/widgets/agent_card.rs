//! In-transcript cards for sub-agent activity (issue #128).
//!
//! Two cards consume the #130 mailbox stream and render live in the chat
//! transcript:
//!
//! - [`DelegateCard`] — single `agent` invocation. Live tree of the
//!   last 3 actions plus a header with status / glyph / role.
//! - [`FanoutCard`] — `rlm` fanout (or any future multi-child dispatch).
//!   Dot-grid of worker slots (`●` filled, `○` pending); header owns lifecycle.
//!
//! Both cards are state machines updated by [`apply_to_delegate`] /
//! [`apply_to_fanout`]. The sidebar (see `tui/sidebar.rs`) defers detail
//! to whichever card is active in the transcript, so these are the
//! primary status surface.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::widgets::tool_card::{ToolFamily, family_glyph};
use codewhale_agent_contract::role::public_role_label;
use codewhale_engine::todo_snapshot::{
    TodoCardProjection, card_omission_line, card_todo_projection,
};
use codewhale_engine::tools::subagent::MailboxMessage;
use codewhale_engine::tools::todo::TodoListSnapshot;
use codewhale_palette as palette;
use codewhale_tui_kit::ui_text::truncate_line_to_width;
use unicode_width::UnicodeWidthStr;

/// Maximum number of recent actions kept on a `DelegateCard`. Older entries
/// are dropped from the head; an ellipsis row signals truncation.
pub const DELEGATE_MAX_ACTIONS: usize = 3;

/// Lifecycle of a delegated / fanned-out agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// Interrupted with a continuable checkpoint (e.g. API timeout); not
    /// running, but recoverable from its checkpoint.
    Interrupted,
}

impl AgentLifecycle {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// Semantic status color only — never the whole-card identity tint.
    /// cyan/teal = running, amber = waiting/pending, green = done, red = failed.
    #[must_use]
    pub fn ink(self) -> codewhale_palette::grammar::ChromeInk {
        match self {
            Self::Pending => codewhale_palette::grammar::ChromeInk::Waiting,
            Self::Running => codewhale_palette::grammar::ChromeInk::Active,
            Self::Completed => codewhale_palette::grammar::ChromeInk::Outcome,
            Self::Failed => codewhale_palette::grammar::ChromeInk::Failure,
            Self::Cancelled => codewhale_palette::grammar::ChromeInk::Metadata,
            Self::Interrupted => codewhale_palette::grammar::ChromeInk::Attention,
        }
    }

    #[must_use]
    pub fn color(self, theme: &palette::UiTheme) -> Color {
        self.ink().color(theme)
    }
}

/// Card for a single delegated `agent` invocation.
///
/// Stores the last [`DELEGATE_MAX_ACTIONS`] action lines; older entries are
/// truncated and a single ellipsis row is rendered above the visible tail.
#[derive(Debug, Clone)]
pub struct DelegateCard {
    pub agent_id: String,
    pub agent_type: String,
    pub status: AgentLifecycle,
    pub summary: Option<String>,
    actions: Vec<String>,
    truncated: bool,
    pub started_at: Option<Instant>,
    pub finished_at: Option<Instant>,
    /// The last To-do snapshot **this** agent published for itself (#4810).
    ///
    /// `None` means the child has never reported Work state — the card says
    /// nothing rather than borrowing the parent's or a sibling's list. The
    /// snapshot is only ever written from an envelope whose `agent_id` matches
    /// [`Self::agent_id`], which is what keeps sibling cards disjoint.
    todo: Option<TodoListSnapshot>,
}

impl DelegateCard {
    #[must_use]
    pub fn new(agent_id: impl Into<String>, agent_type: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            agent_type: agent_type.into(),
            status: AgentLifecycle::Pending,
            summary: None,
            actions: Vec::new(),
            truncated: false,
            started_at: None,
            finished_at: None,
            todo: None,
        }
    }

    /// Record this agent's own To-do snapshot. Returns whether the visible
    /// projection changed (an update that renders identically is not a
    /// redraw). Callers must only pass a snapshot published by this agent.
    pub fn set_todo(&mut self, todo: TodoListSnapshot) -> bool {
        let before = self.todo.as_ref().and_then(card_todo_projection);
        let after = card_todo_projection(&todo);
        self.todo = Some(todo);
        before != after
    }

    /// The child's own To-do projection, if it has reported any work.
    #[must_use]
    pub fn todo_projection(&self) -> Option<TodoCardProjection> {
        self.todo.as_ref().and_then(card_todo_projection)
    }

    /// Project this direct sub-agent card onto the shared workflow history
    /// renderer (#4122) so collapsed/expanded concepts stay aligned.
    #[must_use]
    #[allow(dead_code)] // public #4122 convergence API; covered by unit tests
    pub fn as_workflow_history_panel(
        &self,
        started_at_ms: u64,
        completed_at_ms: Option<u64>,
    ) -> crate::tui::widgets::workflow_panel::WorkflowPanel {
        use crate::tui::widgets::workflow_panel::{WorkflowPanel, WorkflowPanelLifecycle};
        let lifecycle = match self.status {
            AgentLifecycle::Pending => WorkflowPanelLifecycle::Pending,
            AgentLifecycle::Running => WorkflowPanelLifecycle::Running,
            AgentLifecycle::Completed => WorkflowPanelLifecycle::Succeeded,
            AgentLifecycle::Failed => WorkflowPanelLifecycle::Failed,
            AgentLifecycle::Cancelled => WorkflowPanelLifecycle::Cancelled,
            AgentLifecycle::Interrupted => WorkflowPanelLifecycle::Failed,
        };
        WorkflowPanel::from_direct_subagent(
            self.agent_id.clone(),
            public_role_label(&self.agent_type),
            lifecycle,
            started_at_ms,
            completed_at_ms,
            self.summary.clone(),
            if matches!(self.status, AgentLifecycle::Failed) {
                self.summary.clone()
            } else {
                None
            },
        )
    }

    pub fn push_action(&mut self, action: impl Into<String>) {
        self.actions.push(action.into());
        if self.actions.len() > DELEGATE_MAX_ACTIONS {
            // Drop one head entry per overflow so steady-state is exactly
            // DELEGATE_MAX_ACTIONS lines; the ellipsis row signals the rest.
            self.actions.remove(0);
            self.truncated = true;
        }
    }

    #[must_use]
    pub fn render_lines(&self, width: u16, theme: &palette::UiTheme) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.actions.len() + 3);
        let content_width = usize::from(width);
        let role = public_role_label(&self.agent_type);
        let short_id = codewhale_engine::session_manager::truncate_id(&self.agent_id).to_string();
        let detail = if self.status.is_terminal() {
            String::new()
        } else if let Some(action) = self.actions.last() {
            truncate_action(action, 72)
        } else {
            short_id
        };
        lines.push(delegate_header(
            self.status,
            &role,
            &detail,
            content_width,
            theme,
        ));
        // The child's own Work state sits directly under its header, above the
        // action tail: what it is working on outranks what it just did.
        if let Some(todo) = self.todo_projection() {
            let prefix = "\u{22EF} "; // ⋯
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(palette::TEXT_DIM)),
                Span::styled(
                    truncate_action(&todo.header, line_detail_width(content_width, prefix)),
                    Style::default()
                        .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
                ),
            ]));
            let item_prefix = "  ";
            for item in &todo.items {
                lines.push(Line::from(vec![
                    Span::raw(item_prefix),
                    Span::styled(
                        truncate_action(item, line_detail_width(content_width, item_prefix)),
                        Style::default()
                            .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
                    ),
                ]));
            }
            if todo.omitted > 0 {
                lines.push(Line::from(vec![
                    Span::raw(item_prefix),
                    Span::styled(
                        truncate_action(
                            &card_omission_line(todo.omitted),
                            line_detail_width(content_width, item_prefix),
                        ),
                        Style::default()
                            .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
                    ),
                ]));
            }
        }
        if self.truncated {
            lines.push(Line::from(Span::styled(
                "\u{2026}".to_string(), // …
                Style::default().fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
            )));
        }
        for action in self
            .actions
            .iter()
            .take(self.actions.len().saturating_sub(1))
        {
            let prefix = "\u{2502} ";
            lines.push(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
                ),
                Span::styled(
                    truncate_action(action, line_detail_width(content_width, prefix).min(200)),
                    Style::default()
                        .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
                ),
            ]));
        }
        if self.status.is_terminal() {
            let mut terminal = self.status.label().to_string();
            if let (Some(started), Some(finished)) = (self.started_at, self.finished_at) {
                terminal.push_str(" · ");
                terminal.push_str(&codewhale_command_contract::elapsed::format_elapsed_ms(
                    finished.duration_since(started).as_millis() as u64,
                ));
            }
            if let Some(summary) = self
                .summary
                .as_deref()
                .filter(|summary| !summary.is_empty())
            {
                terminal.push_str(" · ");
                terminal.push_str(summary);
            }
            let prefix = "\u{2570} ";
            lines.push(Line::from(Span::styled(
                format!("{prefix}{terminal}"),
                Style::default().fg(self.status.color(theme)),
            )));
        }
        lines
    }

    /// Number of actions held — exposed for tests; bounded at
    /// `DELEGATE_MAX_ACTIONS`.
    #[must_use]
    #[cfg(test)]
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// Whether the head was truncated (older actions dropped).
    #[must_use]
    #[cfg(test)]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// One worker slot in a fanout group.
#[derive(Debug, Clone)]
pub struct WorkerSlot {
    /// Stable logical worker key. Stays tied to the worker slot even after a
    /// concrete sub-agent id exists.
    pub worker_id: String,
    /// Concrete agent id once spawned; placeholders use the worker id.
    pub agent_id: String,
    pub status: AgentLifecycle,
}

impl WorkerSlot {
    #[must_use]
    pub fn new(worker_id: impl Into<String>, status: AgentLifecycle) -> Self {
        let worker_id = worker_id.into();
        Self {
            agent_id: worker_id.clone(),
            worker_id,
            status,
        }
    }
}

/// Card for `rlm` (or any multi-child dispatch) fanout: dot-grid +
/// aggregate counts.
///
/// Slots are added as `ChildSpawned` envelopes arrive (or pre-allocated by
/// the engine when the worker count is known up front); each slot
/// transitions independently as its `Completed` / `Failed` / `Cancelled`
/// envelope is observed.
#[derive(Debug, Clone)]
pub struct FanoutCard {
    pub kind: String,
    pub workers: Vec<WorkerSlot>,
}

impl FanoutCard {
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            workers: Vec::new(),
        }
    }

    /// Pre-seed worker slots when the fanout size is known up front.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn with_workers<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for id in ids {
            self.workers
                .push(WorkerSlot::new(id.into(), AgentLifecycle::Pending));
        }
        self
    }

    /// Update or insert a worker by id. Returns whether the visible state
    /// changed and the card should be redrawn. A settled worker is final: a
    /// delayed envelope for it never reopens or re-settles the slot.
    pub fn upsert_worker(&mut self, agent_id: &str, status: AgentLifecycle) -> bool {
        if let Some(slot) = self
            .workers
            .iter_mut()
            .find(|s| s.agent_id == agent_id || s.worker_id == agent_id)
        {
            if slot.status.is_terminal() || (slot.agent_id == agent_id && slot.status == status) {
                return false;
            }
            slot.agent_id = agent_id.to_string();
            slot.status = status;
            true
        } else {
            self.workers.push(WorkerSlot::new(agent_id, status));
            true
        }
    }

    /// Attach a real agent id to the first pending placeholder slot. Fanout
    /// cards are seeded from task ids before child agents exist; when a child
    /// starts, this keeps the dot count stable instead of appending a second
    /// circle for the same unit of work.
    pub fn claim_pending_worker(&mut self, agent_id: &str, status: AgentLifecycle) -> bool {
        if let Some(slot) = self.workers.iter_mut().find(|s| s.agent_id == agent_id) {
            if slot.status.is_terminal() || slot.status == status {
                return false;
            }
            slot.status = status;
            return true;
        }
        if let Some(slot) = self
            .workers
            .iter_mut()
            .find(|s| matches!(s.status, AgentLifecycle::Pending))
        {
            slot.agent_id = agent_id.to_string();
            slot.status = status;
            return true;
        }
        self.upsert_worker(agent_id, status)
    }

    fn counts(&self) -> (usize, usize, usize, usize) {
        let mut done = 0usize;
        let mut running = 0usize;
        let mut failed = 0usize;
        let mut pending = 0usize;
        for slot in &self.workers {
            match slot.status {
                AgentLifecycle::Completed => done += 1,
                AgentLifecycle::Running => running += 1,
                AgentLifecycle::Failed
                | AgentLifecycle::Cancelled
                | AgentLifecycle::Interrupted => failed += 1,
                AgentLifecycle::Pending => pending += 1,
            }
        }
        (done, running, failed, pending)
    }

    #[must_use]
    pub fn dot_grid(&self) -> String {
        let mut s = String::with_capacity(self.workers.len());
        for slot in &self.workers {
            let glyph = match slot.status {
                AgentLifecycle::Completed => '\u{25CF}',   // ●
                AgentLifecycle::Running => '\u{25D0}',     // ◐
                AgentLifecycle::Failed => '\u{00D7}',      // ×
                AgentLifecycle::Cancelled => '\u{2298}',   // ⊘
                AgentLifecycle::Pending => '\u{25CB}',     // ○
                AgentLifecycle::Interrupted => '\u{25CC}', // ◌
            };
            s.push(glyph);
        }
        s
    }

    /// Header plus dot grid, one glyph per worker. The grid never runs past
    /// `width`: when it does not fit beside the header it wraps onto its own
    /// rows, so no worker's state is clipped off the transcript edge.
    #[must_use]
    pub fn render_lines(&self, width: u16, theme: &palette::UiTheme) -> Vec<Line<'static>> {
        let header_status = self.aggregate_status();
        let count = self.workers.len();
        let count_label = if count == 1 { "agent" } else { "agents" };
        let glyph = family_glyph(ToolFamily::Fanout);
        let count_text = format!("{count} {count_label}");
        let grid_style = Style::default()
            .fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme))
            .add_modifier(Modifier::BOLD);
        // Every span fits the painted width, down to zero columns (U05-m6):
        // the glyph only when it fits, the count only with room after it.
        let width = usize::from(width);
        let glyph_width = UnicodeWidthStr::width(glyph);
        let mut header = Vec::new();
        if width >= glyph_width {
            header.push(Span::styled(
                glyph,
                Style::default()
                    .fg(header_status.color(theme))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if width > glyph_width + 1 {
            header.push(Span::raw(" "));
            header.push(Span::styled(
                truncate_action(&count_text, width - glyph_width - 1),
                Style::default()
                    .fg(codewhale_palette::grammar::ChromeInk::Identity.color(theme))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if width == 0 {
            return vec![Line::from(header)];
        }
        let grid: Vec<char> = self.dot_grid().chars().collect();
        let header_width = glyph_width + 1 + count_text.width() + 1;
        if header_width + grid.len() <= width {
            header.push(Span::raw(" "));
            header.push(Span::styled(grid.iter().collect::<String>(), grid_style));
            return vec![Line::from(header)];
        }
        let mut lines = vec![Line::from(header)];
        lines.extend(
            grid.chunks(width)
                .map(|row| Line::from(Span::styled(row.iter().collect::<String>(), grid_style))),
        );
        lines
    }

    fn aggregate_status(&self) -> AgentLifecycle {
        self.aggregate_status_public()
    }

    /// Public aggregate lifecycle for the activity shelf and other projectors.
    ///
    /// A settled fanout reports its worst outcome: one failed worker among
    /// finished ones is a failed fanout, not a completed one; an
    /// interrupted or cancelled worker likewise outranks the completions.
    #[must_use]
    pub fn aggregate_status_public(&self) -> AgentLifecycle {
        let (done, running, _, pending) = self.counts();
        let any = |status: AgentLifecycle| self.workers.iter().any(|slot| slot.status == status);
        if running > 0 {
            AgentLifecycle::Running
        } else if pending > 0 {
            // Pending workers wait — amber attention, not "running" teal.
            AgentLifecycle::Pending
        } else if any(AgentLifecycle::Failed) {
            AgentLifecycle::Failed
        } else if any(AgentLifecycle::Interrupted) {
            AgentLifecycle::Interrupted
        } else if any(AgentLifecycle::Cancelled) {
            AgentLifecycle::Cancelled
        } else if done > 0 {
            AgentLifecycle::Completed
        } else {
            AgentLifecycle::Pending
        }
    }

    /// Worker count (slots seeded or observed via mailbox).
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

fn delegate_header(
    status: AgentLifecycle,
    role: &str,
    detail: &str,
    width: usize,
    theme: &palette::UiTheme,
) -> Line<'static> {
    let glyph = format!("{} ", family_glyph(ToolFamily::Delegate));
    let fixed_parts: Vec<&str> = vec![glyph.as_str(), role, "  "];
    let fixed_width = fixed_parts
        .iter()
        .map(|text| UnicodeWidthStr::width(*text))
        .sum::<usize>();
    let detail = truncate_action(detail, width.saturating_sub(fixed_width));
    let spans = vec![
        Span::styled(
            glyph,
            Style::default()
                .fg(status.color(theme))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            role.to_string(),
            Style::default()
                .fg(codewhale_palette::grammar::ChromeInk::Identity.color(theme))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            detail,
            Style::default().fg(codewhale_palette::grammar::ChromeInk::Metadata.color(theme)),
        ),
    ];
    Line::from(spans)
}

fn line_detail_width(line_width: usize, prefix: &str) -> usize {
    line_width.saturating_sub(UnicodeWidthStr::width(prefix))
}

fn truncate_action(text: &str, max: usize) -> String {
    truncate_line_to_width(text.trim(), max)
}

/// Apply a mailbox envelope to a `DelegateCard`. Returns `true` if the
/// state changed (UI may want to redraw); `false` if the envelope was for
/// a different `agent_id`.
pub fn apply_to_delegate(card: &mut DelegateCard, msg: &MailboxMessage) -> bool {
    if msg.agent_id() != card.agent_id {
        return false;
    }
    let was_terminal = card.status.is_terminal();
    // A settled agent is final. Mailbox producers are concurrent (a tool
    // task can outlive its agent), so a Started/Progress/tool envelope may
    // land after the terminal one; it must not reopen the card, stretch its
    // elapsed time, or rewrite its outcome. Only the child's own Work
    // snapshot still applies (it keeps the terminal status), and a late
    // `Started` may still name the role of a card that a completion-first
    // delivery opened with the `unknown` placeholder — identity, not state.
    if was_terminal {
        if let MailboxMessage::Started { agent_type, .. } = msg
            && card.agent_type == "unknown"
            && agent_type != "unknown"
        {
            card.agent_type = agent_type.clone();
            return true;
        }
        if !matches!(msg, MailboxMessage::WorkState { .. }) {
            return false;
        }
    }
    if card.started_at.is_none()
        && matches!(
            msg,
            MailboxMessage::Started { .. }
                | MailboxMessage::Progress { .. }
                | MailboxMessage::ToolCallStarted { .. }
                | MailboxMessage::ToolCallCompleted { .. }
                | MailboxMessage::WorkState { .. }
        )
    {
        card.started_at = Some(Instant::now());
    }
    match msg {
        MailboxMessage::Started { .. } => {
            if card.status == AgentLifecycle::Running {
                return false;
            }
            card.status = AgentLifecycle::Running;
        }
        MailboxMessage::Progress { status, .. } => {
            let low_signal = is_low_signal_progress(status);
            if low_signal && card.status == AgentLifecycle::Running {
                return false;
            }
            card.status = AgentLifecycle::Running;
            if !low_signal {
                card.push_action(status);
            }
        }
        MailboxMessage::ToolCallStarted { tool_name, .. } => {
            card.push_action(format!("{tool_name} running"));
        }
        MailboxMessage::ToolCallCompleted { tool_name, ok, .. } => {
            card.push_action(format!("{tool_name} {}", if *ok { "ok" } else { "failed" }));
        }
        MailboxMessage::Completed { summary, .. } => {
            card.status = AgentLifecycle::Completed;
            card.summary = Some(summary.clone());
        }
        MailboxMessage::Failed { error, .. } => {
            card.status = AgentLifecycle::Failed;
            card.summary = Some(error.clone());
        }
        MailboxMessage::Interrupted { reason, .. } => {
            card.status = AgentLifecycle::Interrupted;
            card.summary = Some(reason.clone());
        }
        MailboxMessage::Cancelled { .. } => {
            card.status = AgentLifecycle::Cancelled;
        }
        MailboxMessage::WorkState { todo, .. } => {
            // agent_id already matched above, so this is this child's own
            // list. Publishing live work is evidence that a pending child
            // has started, while terminal cards keep both their terminal
            // status and the last snapshot the child published.
            let status_changed = if card.status == AgentLifecycle::Pending {
                card.status = AgentLifecycle::Running;
                true
            } else {
                false
            };
            return card.set_todo(todo.clone()) || status_changed;
        }
        MailboxMessage::ChildSpawned { .. } => {
            // Delegate cards represent a single agent; child spawns belong
            // to a sibling fanout card, not this one.
            return false;
        }
        MailboxMessage::TokenUsage { .. } => {
            // Cost accumulation happens in handle_subagent_mailbox (ui.rs)
            // before this apply function is called; TokenUsage never reaches
            // this arm in practice.
            return false;
        }
    }
    if !was_terminal && card.status.is_terminal() {
        card.finished_at = Some(Instant::now());
    }
    true
}

/// Known limitation: this still matches on message text, while the footer
/// path keys off the structured `routine_wait` flag (#6290). The mailbox is
/// a stable cross-crate (`protocol`) surface whose payloads may come from
/// another binary, so this arm cannot assume the flag exists — and no
/// in-crate producer sends routine waits here anyway (only "queued" and
/// "running" texts), so threading the flag would change nothing. If a future
/// producer sends routine waits over the mailbox, give `Progress` the flag
/// and match on it here instead of extending this list.
fn is_low_signal_progress(status: &str) -> bool {
    let status = status.trim().to_ascii_lowercase();
    status.contains("requesting model response")
        || status.starts_with("started (")
        || (status.starts_with("step ") && status.contains(": complete"))
}

/// Apply a mailbox envelope to a `FanoutCard`. Updates per-worker state
/// based on which child the envelope is about. Returns `true` on change.
pub fn apply_to_fanout(card: &mut FanoutCard, msg: &MailboxMessage) -> bool {
    let id = msg.agent_id();
    match msg {
        MailboxMessage::Started { .. } => card.claim_pending_worker(id, AgentLifecycle::Running),
        MailboxMessage::Progress { .. } => card.claim_pending_worker(id, AgentLifecycle::Running),
        MailboxMessage::ToolCallStarted { .. } => {
            card.claim_pending_worker(id, AgentLifecycle::Running)
        }
        MailboxMessage::ToolCallCompleted { .. } => true,
        MailboxMessage::Completed { .. } => card.upsert_worker(id, AgentLifecycle::Completed),
        MailboxMessage::Failed { .. } => card.upsert_worker(id, AgentLifecycle::Failed),
        MailboxMessage::Interrupted { .. } => card.upsert_worker(id, AgentLifecycle::Interrupted),
        MailboxMessage::Cancelled { .. } => card.upsert_worker(id, AgentLifecycle::Cancelled),
        MailboxMessage::ChildSpawned { child_id, .. } => {
            card.upsert_worker(child_id, AgentLifecycle::Pending)
        }
        // A fanout card is a dot grid of many workers with no per-worker row
        // to hang a list on. Rather than merge N children's lists into one
        // card — which would be exactly the cross-agent leak this surface must
        // not have — it shows none of them. WorkState is intentionally
        // unavailable on this fanout surface; an individually spawned child
        // may show its own To-do when it has a separate delegate card.
        MailboxMessage::WorkState { .. } => false,
        MailboxMessage::TokenUsage { .. } => {
            // Cost accumulation happens in handle_subagent_mailbox (ui.rs)
            // before this apply function is called; TokenUsage never reaches
            // this arm in practice.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn render_to_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn delegate_card_header_does_not_duplicate_verb_as_role() {
        let card = DelegateCard::new("agent_1", "explore");
        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(
            !rendered.contains("delegate"),
            "delegate must not be visible in the card: {rendered:?}"
        );
        assert!(!rendered.contains("[running]"), "{rendered:?}");
        let explore = DelegateCard::new("agent_2", "scout");
        let explore_rendered =
            render_to_strings(&explore.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(explore_rendered.contains("explore"), "{explore_rendered:?}");
    }

    #[test]
    fn delegate_card_cjk_text_respects_render_width() {
        let mut card = DelegateCard::new("agent_e0b2dcf1", "implementer");
        card.status = AgentLifecycle::Running;
        card.summary = Some(
            "抹香鲸 agent_e0b2dcf1 running 10+ 124838ms role: implementer git: branch codex/issue-3439-zhipu-glm-fixture @ issue-3439".into(),
        );
        card.push_action("objective: QUESTION: Add Zhipu GLM as a first-class provider-scoped route for 中文输出".to_string());

        let rendered = render_to_strings(&card.render_lines(40, &codewhale_palette::UI_THEME));

        assert!(rendered[0].contains("implement"), "{rendered:?}");
        for line in rendered {
            let width = UnicodeWidthStr::width(line.as_str());
            assert!(width <= 40, "line width {width} exceeds 40: {line:?}");
        }
    }

    #[test]
    fn delegate_card_truncates_to_last_three_actions_with_ellipsis() {
        let mut card = DelegateCard::new("agent_001", "general");
        card.push_action("read README.md");
        card.push_action("grep TODO");
        card.push_action("edit src/lib.rs");
        // Up to the limit — no truncation yet.
        assert!(!card.truncated());
        assert_eq!(card.action_count(), DELEGATE_MAX_ACTIONS);

        card.push_action("write tests");
        card.push_action("run cargo test");
        assert!(card.truncated(), "truncation flag flips on overflow");
        assert_eq!(
            card.action_count(),
            DELEGATE_MAX_ACTIONS,
            "stable steady-state size"
        );

        let rendered = render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME));
        assert!(
            rendered.iter().any(|line| line.contains('\u{2026}')),
            "ellipsis indicator must render: got {rendered:?}"
        );
        // The oldest two actions ("read README.md", "grep TODO") were dropped.
        assert!(
            !rendered.iter().any(|line| line.contains("read README.md")),
            "oldest action evicted: got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("run cargo test")),
            "newest action retained: got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("write tests")),
            "second-newest retained: got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("edit src/lib.rs")),
            "third-newest retained: got {rendered:?}"
        );
    }

    #[test]
    fn delegate_card_terminal_status_renders_summary_row() {
        let mut card = DelegateCard::new("agent_002", "explore");
        card.push_action("listing files");
        let msg = MailboxMessage::Completed {
            agent_id: "agent_002".into(),
            summary: "scanned 42 files, no TODOs found".into(),
        };
        assert!(apply_to_delegate(&mut card, &msg));
        assert_eq!(card.status, AgentLifecycle::Completed);
        let rendered = render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME));
        assert!(
            rendered.iter().any(|line| line.contains("╰ done")),
            "terminal status row renders done: got {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("scanned 42 files")),
            "summary row renders on terminal status: got {rendered:?}"
        );
    }

    #[test]
    fn delegate_card_ignores_low_signal_scheduler_progress() {
        let mut card = DelegateCard::new("agent_003", "general");
        let msg = MailboxMessage::progress("agent_003", "step 1/100: requesting model response");

        assert!(apply_to_delegate(&mut card, &msg));
        assert_eq!(card.status, AgentLifecycle::Running);
        assert_eq!(
            card.action_count(),
            0,
            "scheduler progress should not become a stale transcript row"
        );

        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(!rendered.contains("step 1/100"), "{rendered}");
        assert!(
            !rendered.contains("requesting model response"),
            "{rendered}"
        );
        assert!(
            !apply_to_delegate(&mut card, &msg),
            "repeated low-signal progress should not redraw the card"
        );
    }

    #[test]
    fn delegate_tool_rows_omit_internal_step_numbers() {
        let mut card = DelegateCard::new("agent_004", "general");

        assert!(apply_to_delegate(
            &mut card,
            &MailboxMessage::ToolCallStarted {
                agent_id: "agent_004".into(),
                tool_name: "read_file".into(),
                step: 7,
            }
        ));
        assert!(apply_to_delegate(
            &mut card,
            &MailboxMessage::ToolCallCompleted {
                agent_id: "agent_004".into(),
                tool_name: "read_file".into(),
                step: 7,
                ok: true,
            }
        ));

        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(rendered.contains("read_file"), "{rendered}");
        assert!(
            !rendered.contains("[7]"),
            "internal loop step numbers are not useful in the live card: {rendered}"
        );
    }

    #[test]
    fn delegate_card_ignores_envelopes_for_other_agents() {
        let mut card = DelegateCard::new("agent_a", "general");
        let other = MailboxMessage::progress("agent_b", "noise");
        assert!(!apply_to_delegate(&mut card, &other));
        assert_eq!(card.action_count(), 0);
    }

    #[test]
    fn fanout_card_dot_grid_renders_stateful_worker_slots() {
        let mut card = FanoutCard::new("fanout")
            .with_workers(["w_1", "w_2", "w_3", "w_4", "w_5", "w_6", "w_7"]);
        card.upsert_worker("w_1", AgentLifecycle::Completed);
        card.upsert_worker("w_2", AgentLifecycle::Completed);
        card.upsert_worker("w_3", AgentLifecycle::Running);
        card.upsert_worker("w_4", AgentLifecycle::Failed);
        // 5/6/7 stay Pending.

        // Completed fills; running and failed are distinct; pending stays open.
        assert_eq!(
            card.dot_grid(),
            "\u{25CF}\u{25CF}\u{25D0}\u{00D7}\u{25CB}\u{25CB}\u{25CB}"
        );
    }

    #[test]
    fn fanout_card_header_and_dot_grid_surface_aggregate_state() {
        let mut card = FanoutCard::new("rlm").with_workers(["w_1", "w_2", "w_3", "w_4"]);
        card.upsert_worker("w_1", AgentLifecycle::Completed);
        card.upsert_worker("w_2", AgentLifecycle::Completed);
        card.upsert_worker("w_3", AgentLifecycle::Completed);
        card.upsert_worker("w_4", AgentLifecycle::Failed);
        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(
            rendered.contains("4 agents"),
            "header should show count: {rendered}"
        );
        assert!(
            rendered.starts_with("⋮⋮ 4 agents"),
            "header should omit the fanout kind: {rendered}"
        );
        assert!(
            rendered.contains("\u{25CF}\u{25CF}\u{25CF}\u{00D7}"),
            "dot grid should mirror worker states: {rendered}"
        );
        assert!(
            !rendered.contains(" pending"),
            "redundant counts line should stay omitted: {rendered}"
        );
    }

    #[test]
    fn fanout_apply_inserts_unknown_worker_via_child_spawned() {
        let mut card = FanoutCard::new("fanout");
        let msg = MailboxMessage::ChildSpawned {
            parent_id: "root".into(),
            child_id: "agent_late".into(),
        };
        assert!(apply_to_fanout(&mut card, &msg));
        assert_eq!(card.worker_count(), 1);
        assert_eq!(card.workers[0].agent_id, "agent_late");
        assert_eq!(card.workers[0].status, AgentLifecycle::Pending);
    }

    #[test]
    fn fanout_started_claims_seeded_pending_slot_without_growing_grid() {
        let mut card = FanoutCard::new("fanout").with_workers(["task:a", "task:b"]);
        let started = MailboxMessage::started(
            "agent_live",
            codewhale_agent_contract::role::FleetRole::Worker,
        );

        assert!(apply_to_fanout(&mut card, &started));

        assert_eq!(card.worker_count(), 2);
        assert_eq!(card.workers[0].agent_id, "agent_live");
        assert_eq!(card.workers[0].status, AgentLifecycle::Running);
        assert_eq!(card.workers[1].agent_id, "task:b");
        assert_eq!(card.workers[1].status, AgentLifecycle::Pending);
        let progress =
            MailboxMessage::progress("agent_live", "step 1/100: requesting model response");
        assert!(
            !apply_to_fanout(&mut card, &progress),
            "repeated progress for a running worker should not redraw"
        );
    }

    #[test]
    fn fanout_apply_transitions_worker_through_lifecycle() {
        let mut card = FanoutCard::new("fanout").with_workers(["w_1"]);
        let started =
            MailboxMessage::started("w_1", codewhale_agent_contract::role::FleetRole::Worker);
        apply_to_fanout(&mut card, &started);
        assert_eq!(card.workers[0].status, AgentLifecycle::Running);

        let done = MailboxMessage::Completed {
            agent_id: "w_1".into(),
            summary: "ok".into(),
        };
        apply_to_fanout(&mut card, &done);
        assert_eq!(card.workers[0].status, AgentLifecycle::Completed);
    }

    #[test]
    fn fanout_dot_grid_arithmetic_for_various_n() {
        // Spot-check several fanout sizes with a mix of states; this is the
        // arithmetic snapshot the issue acceptance calls out.
        let cases: &[(usize, usize, &str)] = &[
            (1, 0, "\u{25CB}"),
            (1, 1, "\u{25CF}"),
            (3, 2, "\u{25CF}\u{25CF}\u{25CB}"),
            (
                7,
                3,
                "\u{25CF}\u{25CF}\u{25CF}\u{25CB}\u{25CB}\u{25CB}\u{25CB}",
            ),
        ];
        for (total, done, expected) in cases {
            let ids: Vec<String> = (0..*total).map(|i| format!("w_{i}")).collect();
            let mut card = FanoutCard::new("fanout").with_workers(ids.iter().cloned());
            for id in ids.iter().take(*done) {
                card.upsert_worker(id, AgentLifecycle::Completed);
            }
            assert_eq!(
                card.dot_grid(),
                *expected,
                "fanout dot-grid for total={total} done={done}",
            );
        }
    }

    #[test]
    fn delegate_interrupted_leaves_running_and_renders_reason() {
        let mut card = DelegateCard::new("agent_int", "general");
        apply_to_delegate(
            &mut card,
            &MailboxMessage::started(
                "agent_int",
                codewhale_agent_contract::role::FleetRole::Worker,
            ),
        );
        assert_eq!(card.status, AgentLifecycle::Running);

        let msg = MailboxMessage::Interrupted {
            agent_id: "agent_int".into(),
            reason: "API call timed out after 120000ms; checkpoint preserved for continuation"
                .into(),
        };
        assert!(apply_to_delegate(&mut card, &msg));
        assert_eq!(card.status, AgentLifecycle::Interrupted);

        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(rendered.contains("╰ interrupted"), "{rendered}");
        assert!(rendered.contains("API call timed out"), "{rendered}");
    }

    #[test]
    fn fanout_interrupted_worker_leaves_running_counts() {
        let mut card = FanoutCard::new("fanout").with_workers(["w_1", "w_2"]);
        apply_to_fanout(
            &mut card,
            &MailboxMessage::started("w_1", codewhale_agent_contract::role::FleetRole::Worker),
        );
        apply_to_fanout(
            &mut card,
            &MailboxMessage::started("w_2", codewhale_agent_contract::role::FleetRole::Worker),
        );

        let msg = MailboxMessage::Interrupted {
            agent_id: "w_1".into(),
            reason: "API call timed out".into(),
        };
        assert!(apply_to_fanout(&mut card, &msg));
        assert_eq!(card.workers[0].status, AgentLifecycle::Interrupted);
        assert_eq!(card.workers[1].status, AgentLifecycle::Running);

        // Copy dedupe (Wave 5c #4): the counts line is gone — the header and
        // dot grid carry the aggregate state instead.
        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(rendered.contains("2 agents"), "{rendered}");
        assert!(
            rendered.contains('\u{25D0}'),
            "dot grid should keep the running worker glyph: {rendered}"
        );
        assert!(
            rendered.contains('\u{25CC}'),
            "dot grid should mark the interrupted worker: {rendered}"
        );

        let msg = MailboxMessage::Interrupted {
            agent_id: "w_2".into(),
            reason: "API call timed out".into(),
        };
        assert!(apply_to_fanout(&mut card, &msg));
        let rendered =
            render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME)).join("\n");
        assert!(rendered.contains("2 agents"), "{rendered}");
    }

    #[test]
    fn fanout_card_omits_redundant_counts_line_when_header_and_grid_present() {
        let ids: Vec<String> = (0..16).map(|i| format!("w_{i}")).collect();
        let mut card = FanoutCard::new("fanout").with_workers(ids.iter().cloned());
        for id in ids.iter().take(12) {
            card.upsert_worker(id, AgentLifecycle::Completed);
        }
        card.upsert_worker("w_12", AgentLifecycle::Running);

        let rendered = render_to_strings(&card.render_lines(80, &codewhale_palette::UI_THEME));
        assert!(
            rendered.iter().any(|line| line.contains('\u{25CF}')),
            "dot grid should remain: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|line| line.contains('·')),
            "counts line should be dropped: {rendered:?}"
        );
    }

    // === #4810: a child's own To-do on its own card ===

    use codewhale_engine::tools::todo::{TodoItem, TodoStatus};

    fn todo(items: &[(u32, &str, TodoStatus)], in_progress_id: Option<u32>) -> TodoListSnapshot {
        let items: Vec<TodoItem> = items
            .iter()
            .map(|(id, content, status)| TodoItem {
                id: *id,
                content: (*content).to_string(),
                status: *status,
            })
            .collect();
        let settled = items.iter().filter(|item| item.status.is_settled()).count();
        let completion_pct = if items.is_empty() {
            0
        } else {
            ((settled * 100) / items.len()) as u8
        };
        TodoListSnapshot {
            items,
            completion_pct,
            in_progress_id,
        }
    }

    fn work_state(agent_id: &str, snapshot: TodoListSnapshot) -> MailboxMessage {
        MailboxMessage::WorkState {
            agent_id: agent_id.to_string(),
            todo: snapshot,
        }
    }

    #[test]
    fn delegate_card_renders_the_childs_own_todo_under_its_row() {
        let mut card = DelegateCard::new("agent_child", "implementer");
        apply_to_delegate(
            &mut card,
            &MailboxMessage::started(
                "agent_child",
                codewhale_agent_contract::role::FleetRole::Worker,
            ),
        );
        assert!(apply_to_delegate(
            &mut card,
            &work_state(
                "agent_child",
                todo(
                    &[
                        (1, "read the runtime seam", TodoStatus::Completed),
                        (2, "write the projection", TodoStatus::InProgress),
                    ],
                    Some(2),
                ),
            ),
        ));

        let rendered =
            render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
        assert!(rendered.contains("To-do 1/2"), "{rendered}");
        assert!(rendered.contains("50% settled"), "{rendered}");
        assert!(
            rendered.contains("[~] #2 write the projection"),
            "{rendered}"
        );
        assert!(
            rendered.contains("[x] #1 read the runtime seam"),
            "{rendered}"
        );
        // Role, model-facing lifecycle label, and identity stay exactly as the
        // row already reported them.
        assert!(rendered.contains("implement"), "{rendered}");
    }

    #[test]
    fn delegate_card_ignores_work_state_addressed_to_another_agent() {
        let mut card = DelegateCard::new("agent_a", "general");
        assert!(!apply_to_delegate(
            &mut card,
            &work_state(
                "agent_b",
                todo(&[(1, "sibling only work", TodoStatus::InProgress)], Some(1)),
            ),
        ));
        assert!(card.todo_projection().is_none());
        let rendered =
            render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
        assert!(!rendered.contains("sibling only work"), "{rendered}");
        assert!(!rendered.contains("To-do"), "{rendered}");
    }

    #[test]
    fn delegate_card_shows_a_same_turn_update_without_waiting_for_completion() {
        let mut card = DelegateCard::new("agent_child", "general");
        apply_to_delegate(
            &mut card,
            &work_state(
                "agent_child",
                todo(&[(1, "draft the fix", TodoStatus::InProgress)], Some(1)),
            ),
        );

        // Same step: the child calls work_update and immediately republishes.
        apply_to_delegate(
            &mut card,
            &MailboxMessage::ToolCallCompleted {
                agent_id: "agent_child".to_string(),
                tool_name: "work_update".to_string(),
                step: 1,
                ok: true,
            },
        );
        assert!(
            apply_to_delegate(
                &mut card,
                &work_state(
                    "agent_child",
                    todo(
                        &[
                            (1, "draft the fix", TodoStatus::Completed),
                            (2, "add the regression", TodoStatus::InProgress),
                        ],
                        Some(2),
                    ),
                ),
            ),
            "a changed list must redraw the card"
        );

        let rendered =
            render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
        assert_eq!(card.status, AgentLifecycle::Running, "still mid-turn");
        assert!(rendered.contains("[~] #2 add the regression"), "{rendered}");
        assert!(rendered.contains("[x] #1 draft the fix"), "{rendered}");
        assert!(rendered.contains("To-do 1/2"), "{rendered}");

        // Republishing the identical snapshot is not a visible change.
        assert!(!apply_to_delegate(
            &mut card,
            &work_state(
                "agent_child",
                todo(
                    &[
                        (1, "draft the fix", TodoStatus::Completed),
                        (2, "add the regression", TodoStatus::InProgress),
                    ],
                    Some(2),
                ),
            ),
        ));
    }

    #[test]
    fn delegate_card_empty_child_todo_renders_no_item_at_all() {
        let mut card = DelegateCard::new("agent_child", "general");
        card.push_action("read_file ok");
        apply_to_delegate(
            &mut card,
            &work_state("agent_child", TodoListSnapshot::default()),
        );

        assert!(card.todo_projection().is_none());
        let rendered =
            render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
        assert!(
            !rendered.contains("To-do"),
            "an empty list states nothing: {rendered}"
        );
        assert!(
            !rendered.contains('#'),
            "no synthesized item may appear: {rendered}"
        );
        assert!(rendered.contains("read_file ok"), "{rendered}");
    }

    #[test]
    fn delegate_card_todo_is_bounded_and_marks_what_it_elided() {
        let items: Vec<(u32, String, TodoStatus)> = (1..=9)
            .map(|id| {
                (
                    id,
                    format!("item {id} ").repeat(30),
                    if id == 8 {
                        TodoStatus::InProgress
                    } else {
                        TodoStatus::Pending
                    },
                )
            })
            .collect();
        let refs: Vec<(u32, &str, TodoStatus)> = items
            .iter()
            .map(|(id, content, status)| (*id, content.as_str(), *status))
            .collect();
        let mut card = DelegateCard::new("agent_child", "general");
        apply_to_delegate(&mut card, &work_state("agent_child", todo(&refs, Some(8))));

        let projection = card.todo_projection().expect("projection");
        assert_eq!(
            projection.items.len(),
            codewhale_engine::todo_snapshot::MAX_CARD_ITEM_LINES
        );
        assert_eq!(
            projection.omitted,
            9 - codewhale_engine::todo_snapshot::MAX_CARD_ITEM_LINES
        );
        assert!(
            projection
                .items
                .iter()
                .any(|line| line.starts_with("[~] #8")),
            "the active item is never the one dropped: {projection:?}"
        );

        let rendered = render_to_strings(&card.render_lines(60, &codewhale_palette::UI_THEME));
        assert!(
            rendered.iter().any(|line| line.contains("+6 more")),
            "elision must be stated: {rendered:?}"
        );
        for line in &rendered {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 60,
                "line exceeds the card width: {line:?}"
            );
        }
    }

    #[test]
    fn terminal_delegate_cards_keep_the_last_child_todo() {
        for terminal in [
            MailboxMessage::Completed {
                agent_id: "agent_child".to_string(),
                summary: "done".to_string(),
            },
            MailboxMessage::Failed {
                agent_id: "agent_child".to_string(),
                error: "boom".to_string(),
            },
            MailboxMessage::Cancelled {
                agent_id: "agent_child".to_string(),
            },
        ] {
            let mut card = DelegateCard::new("agent_child", "general");
            apply_to_delegate(
                &mut card,
                &work_state(
                    "agent_child",
                    todo(
                        &[
                            (1, "land the fix", TodoStatus::Completed),
                            (2, "run the suite", TodoStatus::InProgress),
                        ],
                        Some(2),
                    ),
                ),
            );
            apply_to_delegate(&mut card, &terminal);

            let rendered =
                render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
            assert!(card.status.is_terminal(), "{:?}", card.status);
            assert!(
                rendered.contains("[~] #2 run the suite"),
                "terminal card keeps the last truthful list ({:?}): {rendered}",
                card.status
            );
            assert!(rendered.contains("To-do 1/2"), "{rendered}");
        }
    }

    #[test]
    fn fanout_card_does_not_project_any_workers_todo() {
        let mut card = FanoutCard::new("fanout").with_workers(["w_1", "w_2"]);
        assert!(!apply_to_fanout(
            &mut card,
            &work_state(
                "w_1",
                todo(&[(1, "worker one work", TodoStatus::InProgress)], Some(1)),
            ),
        ));
        let rendered =
            render_to_strings(&card.render_lines(100, &codewhale_palette::UI_THEME)).join("\n");
        assert!(!rendered.contains("worker one work"), "{rendered}");
        assert!(!rendered.contains("To-do"), "{rendered}");
    }

    #[test]
    fn direct_subagent_projects_onto_shared_workflow_history_card() {
        use crate::tui::widgets::workflow_panel::WorkflowHistoryExtras;

        let mut card = DelegateCard::new("agent_xyz", "explore");
        card.status = AgentLifecycle::Completed;
        card.summary = Some("mapped 4 call sites".to_string());
        let panel = card.as_workflow_history_panel(1_000, Some(5_000));
        let compact = panel.render_history_card(100, false, &WorkflowHistoryExtras::default());
        let joined = render_to_strings(&compact).join("\n");
        assert!(
            joined.contains("success") || joined.contains("explore"),
            "shared compact lifecycle: {joined}"
        );
        assert!(
            joined.contains("1/1 done"),
            "shared settled/total count: {joined}"
        );
        let expanded = panel.render_history_card(
            100,
            true,
            &WorkflowHistoryExtras {
                result_summary: Some("mapped 4 call sites".to_string()),
                ..WorkflowHistoryExtras::default()
            },
        );
        let joined = render_to_strings(&expanded).join("\n");
        assert!(joined.contains("result:"), "{joined}");
        assert!(joined.contains("mapped 4 call sites"), "{joined}");
    }

    /// U05-04: mailbox producers are concurrent, so a Started/Progress/tool
    /// envelope can land after an agent's terminal one. It must not reopen
    /// the card or its fanout slot, and must not move a settled count.
    #[test]
    fn late_envelopes_never_reopen_a_settled_agent() {
        let role = || codewhale_agent_contract::role::FleetRole::Worker;
        let mut card = DelegateCard::new("agent_late", "general");
        apply_to_delegate(&mut card, &MailboxMessage::started("agent_late", role()));
        assert!(apply_to_delegate(
            &mut card,
            &MailboxMessage::Completed {
                agent_id: "agent_late".into(),
                summary: "done".into(),
            },
        ));
        let finished_at = card.finished_at;
        for late in [
            MailboxMessage::started("agent_late", role()),
            MailboxMessage::progress("agent_late", "step 4: reading files"),
            MailboxMessage::ToolCallStarted {
                agent_id: "agent_late".into(),
                tool_name: "read_file".into(),
                step: 4,
            },
            MailboxMessage::Failed {
                agent_id: "agent_late".into(),
                error: "late".into(),
            },
        ] {
            assert!(!apply_to_delegate(&mut card, &late), "{late:?}");
        }
        assert_eq!(card.status, AgentLifecycle::Completed);
        assert_eq!(card.summary.as_deref(), Some("done"));
        assert_eq!(card.finished_at, finished_at);

        // A completion-first recovery card may still learn its role from a
        // late `Started`, but it stays settled.
        let mut recovered = DelegateCard::new("agent_r", "unknown");
        apply_to_delegate(
            &mut recovered,
            &MailboxMessage::Completed {
                agent_id: "agent_r".into(),
                summary: "done".into(),
            },
        );
        assert!(apply_to_delegate(
            &mut recovered,
            &MailboxMessage::started("agent_r", role())
        ));
        assert_ne!(recovered.agent_type, "unknown");
        assert_eq!(recovered.status, AgentLifecycle::Completed);

        let mut fanout = FanoutCard::new("rlm");
        for id in ["w_1", "w_2"] {
            apply_to_fanout(
                &mut fanout,
                &MailboxMessage::ChildSpawned {
                    parent_id: "root".into(),
                    child_id: id.into(),
                },
            );
            apply_to_fanout(&mut fanout, &MailboxMessage::started(id, role()));
            apply_to_fanout(
                &mut fanout,
                &MailboxMessage::Completed {
                    agent_id: id.into(),
                    summary: "ok".into(),
                },
            );
        }
        assert!(!apply_to_fanout(
            &mut fanout,
            &MailboxMessage::progress("w_1", "step 9: late")
        ));
        assert!(!apply_to_fanout(
            &mut fanout,
            &MailboxMessage::started("w_2", role())
        ));
        assert_eq!(fanout.worker_count(), 2);
        assert_eq!(fanout.counts(), (2, 0, 0, 0));
        assert_eq!(fanout.aggregate_status_public(), AgentLifecycle::Completed);
    }

    /// U05-m2: a settled fanout headlines its worst outcome — one failure
    /// among completions is a failed fanout, not a completed one.
    #[test]
    fn mixed_fanout_outcomes_never_aggregate_to_completed() {
        let mut card = FanoutCard::new("rlm").with_workers(["w_1", "w_2", "w_3"]);
        card.upsert_worker("w_1", AgentLifecycle::Completed);
        card.upsert_worker("w_2", AgentLifecycle::Completed);
        card.upsert_worker("w_3", AgentLifecycle::Failed);
        assert_eq!(card.aggregate_status_public(), AgentLifecycle::Failed);

        let mut card = FanoutCard::new("rlm").with_workers(["w_1", "w_2"]);
        card.upsert_worker("w_1", AgentLifecycle::Completed);
        card.upsert_worker("w_2", AgentLifecycle::Cancelled);
        assert_eq!(card.aggregate_status_public(), AgentLifecycle::Cancelled);

        let mut card = FanoutCard::new("rlm").with_workers(["w_1"]);
        card.upsert_worker("w_1", AgentLifecycle::Completed);
        assert_eq!(card.aggregate_status_public(), AgentLifecycle::Completed);
    }

    /// U05-m6: a wide fanout wraps its dot grid instead of running past the
    /// transcript width, and every worker keeps its glyph.
    #[test]
    fn fanout_header_fits_zero_and_one_column_renders() {
        let ids: Vec<String> = (0..5).map(|i| format!("w_{i}")).collect();
        let card = FanoutCard::new("rlm").with_workers(ids.iter().cloned());
        // Every width from nothing up past the header, including the ones
        // where the count is truncated and the grid wraps.
        for width in 0_u16..=16 {
            let lines = render_to_strings(&card.render_lines(width, &codewhale_palette::UI_THEME));
            assert!(!lines.is_empty(), "{width}");
            for line in &lines {
                assert!(
                    UnicodeWidthStr::width(line.as_str()) <= usize::from(width),
                    "{width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn fanout_grid_wraps_within_the_render_width() {
        let ids: Vec<String> = (0..40).map(|i| format!("w_{i}")).collect();
        let card = FanoutCard::new("rlm").with_workers(ids.iter().cloned());
        for width in [12_u16, 20, 80] {
            let lines = render_to_strings(&card.render_lines(width, &codewhale_palette::UI_THEME));
            for line in &lines {
                assert!(
                    UnicodeWidthStr::width(line.as_str()) <= usize::from(width),
                    "{width}: {line:?}"
                );
            }
            let glyphs = lines
                .join("")
                .chars()
                .filter(|ch| *ch == '\u{25CB}')
                .count();
            assert_eq!(glyphs, 40, "{width}: {lines:?}");
        }
    }
}

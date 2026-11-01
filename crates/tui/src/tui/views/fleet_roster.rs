//! `/fleet` roster — the barracks view of the saved agent party.
//!
//! The roster view is the primary `/fleet` face. The first row is the
//! **operator** — the Fleet leader (your live session model). When a user
//! picks a session model they are picking the operator, and every member
//! below is that leader's team. The header names the selected saved Fleet and
//! whether it is user-global or folder-scoped, so scope is never ambiguous.
//! Below the operator sits the merged [`FleetRoster`] (built-in <
//! `[fleet.profiles]` config < `$CODEWHALE_HOME/agents/*.toml` personal <
//! `.codewhale/agents/*.toml` project members)
//! as a scrollable list with a detail pane for the selected row. The view
//! never writes anything; Enter opens the shared model/thinking picker for
//! the selected role, retaining this roster underneath. The existing saved
//! team/profile owner validates and persists assignments. Switch named
//! saved Fleets with `/fleet fleets` (`/fleet fleets` remains compatible).
//!
//! #5888: the default lineup folds the built-in `general` alias out of
//! presentation — it is the same posture as `worker` and stays dispatchable
//! (roster lookup and the identity selector both still resolve it) — so the
//! default surface is 11 rows: the live operator plus ten members.
//!
//! NOTE: like `fleet_setup.rs`, the copy below is intentionally English for
//! now (#3167 reworks Fleet UI localization); the command entry
//! (`CmdFleetDescription`) is already localized.

use std::cell::{Cell, RefCell};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::tui::app::App;
use codewhale_agent_contract::role::public_role_label;
use codewhale_engine::fleet::profile::AgentProfile;
use codewhale_engine::fleet::roster::{
    FleetRoster, ProfileLayer, ProfileOrigin, layers_from_parts,
};
use codewhale_engine::fleet::worker_runtime::roster_member_agent_type;
use codewhale_runtime::config::Config;
use codewhale_tui_kit::menu_style;

/// Rows one PageUp/PageDown travels. Pages clamp at the ends per the shared
/// vocabulary instead of wrapping (#6290).
const FLEET_ROSTER_PAGE: usize = 10;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    truncate_view_text,
};
use crate::tui::whales;
use codewhale_agent_contract::worker_profile::{ShellPolicy, WorkerRuntimeProfile};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

/// The live session route — the operator the roster works for. Read once at
/// open, the same way [`super::fleet_setup::FleetSetupSnapshot`] snapshots it.
#[derive(Debug, Clone)]
struct OperatorInfo {
    provider: String,
    /// Exact canonical route key, kept separate from the display label so
    /// capability lookup can use provider-scoped catalog facts.
    provider_id: String,
    model: String,
    reasoning: String,
}

impl OperatorInfo {
    fn from_app(app: &App) -> Self {
        let model = if app.auto_model {
            app.last_effective_model
                .as_deref()
                .map(|effective| format!("auto -> {effective}"))
                .unwrap_or_else(|| "auto".to_string())
        } else {
            app.model.clone()
        };
        let route_provider = if app.auto_model {
            app.last_effective_provider.unwrap_or(app.api_provider)
        } else {
            app.api_provider
        };
        let provider_id = if app.auto_model {
            app.last_effective_provider_identity
                .clone()
                .unwrap_or_else(|| {
                    if route_provider == codewhale_config::ProviderKind::Custom {
                        app.provider_identity_for_persistence().to_string()
                    } else {
                        route_provider.as_str().to_string()
                    }
                })
        } else {
            app.provider_identity_for_persistence().to_string()
        };
        let provider = if route_provider == codewhale_config::ProviderKind::Custom {
            provider_id.clone()
        } else {
            route_provider.provider().display_name().to_string()
        };
        Self {
            provider,
            provider_id,
            model,
            reasoning: app.reasoning_effort_display_label(),
        }
    }
}

/// Which named Fleet (if any) this session is using, and where that selection
/// is pinned — user-global vs this folder only.
#[derive(Debug, Clone)]
struct SelectedFleetSummary {
    name: String,
    scope: codewhale_engine::fleet::store::FleetScope,
}

/// View-owned action attached to a painted saved-profile row.
///
/// This stays deliberately separate from Tideline's live-worker targets,
/// which are backed by `SubAgentStatus`, not editable profiles in this roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetRosterRowAction {
    SelectOrActivate { row: usize },
}

impl FleetRosterRowAction {
    const fn row(self) -> usize {
        match self {
            Self::SelectOrActivate { row } => row,
        }
    }
}

pub struct FleetRosterView {
    operator: OperatorInfo,
    members: Vec<AgentProfile>,
    /// Shadow records from the roster load (#5098): which lower-precedence
    /// files the displayed members are ignoring.
    shadowed: Vec<codewhale_engine::fleet::roster::ShadowedProfile>,
    /// Selected named Fleet + scope, when one is active for this session.
    selected_fleet: Option<SelectedFleetSummary>,
    /// A selected Fleet existed but could not become the runtime roster.
    load_error: Option<String>,
    /// Selected row: 0 is the pinned operator row, members follow at 1..
    selected: usize,
    detail_scroll: usize,
    /// Exact visible row geometry from the latest render. This is a
    /// frame-scoped projection, not a second roster or navigation owner.
    row_hitboxes: RefCell<Vec<(Rect, FleetRosterRowAction)>>,
    /// A first click selects/reveals details; a consecutive click on the same
    /// row activates the exact same handoff as Enter.
    last_mouse_selected: Option<usize>,
    /// Row under the pointer, tinted with the shared hover style. Hover
    /// never moves the keyboard selection; only painted rows answer.
    hovered_row: Cell<Option<usize>>,
    workers_hitbox: Cell<Option<Rect>>,
    hovered_workers: Cell<bool>,
    /// Canonical active-theme surface captured from `App`; Terminal owns
    /// `Color::Reset`, while explicit themes retain their resolved surface.
    surface_bg: Color,
    /// UI locale captured from the app at construction (#4057 wave 2).
    locale: Locale,
}

impl FleetRosterView {
    #[must_use]
    pub fn new(app: &App, config: &Config) -> Self {
        let selected_fleet =
            codewhale_engine::fleet::store::selected_fleet(&app.workspace).map(|sel| {
                SelectedFleetSummary {
                    name: sel.name,
                    scope: sel.scope,
                }
            });
        let mut view = Self::from_parts(
            OperatorInfo::from_app(app),
            codewhale_engine::fleet::identity::load_effective_roster(
                &config.fleet_config(),
                &app.workspace,
                Some(app.plugin_registry.as_ref()),
            ),
            selected_fleet,
        );
        view.locale = app.ui_locale;
        view.surface_bg = app.ui_theme.surface_bg;
        view
    }

    fn from_parts(
        operator: OperatorInfo,
        roster: FleetRoster,
        selected_fleet: Option<SelectedFleetSummary>,
    ) -> Self {
        let load_error = roster.load_error().map(str::to_string);
        Self {
            operator,
            // The operator is pinned as its own row 0 (the live session route),
            // so exclude the built-in "operator" profile from the dispatchable
            // member list to avoid rendering it twice (#dogfood 0.8.67). The
            // engine's FleetRoster is untouched, so role/dispatch semantics are
            // unchanged; only this view drops the duplicate.
            members: roster
                .members()
                .iter()
                .filter(|m| !m.id.trim().eq_ignore_ascii_case("operator"))
                .cloned()
                .collect(),
            shadowed: roster.shadowed().to_vec(),
            selected_fleet,
            load_error,
            selected: 0,
            detail_scroll: 0,
            row_hitboxes: RefCell::new(Vec::new()),
            last_mouse_selected: None,
            hovered_row: Cell::new(None),
            workers_hitbox: Cell::new(None),
            hovered_workers: Cell::new(false),
            surface_bg: palette::UI_THEME.surface_bg,
            locale: Locale::En,
        }
    }

    /// Rebuild this roster from the current workspace, keeping the cursor.
    ///
    /// #5954: the roster now stays parked under the saved-teams list, so a
    /// team switch or delete has to refresh the parked view in place — the
    /// user pops back to it, and it must not keep painting the pre-change
    /// selection. Cursor and detail scroll survive because losing them is
    /// exactly the disruption the back path exists to avoid.
    pub fn reload(&mut self, app: &App, config: &Config) {
        let selected = self.selected;
        let detail_scroll = self.detail_scroll;
        *self = Self::new(app, config);
        self.selected = selected.min(self.row_count().saturating_sub(1));
        self.detail_scroll = detail_scroll;
    }

    /// Total selectable rows: the operator plus every roster member.
    fn row_count(&self) -> usize {
        1 + self.members.len()
    }

    fn operator_selected(&self) -> bool {
        self.selected == 0
    }

    fn selected_member(&self) -> Option<&AgentProfile> {
        self.selected.checked_sub(1).and_then(|idx| {
            self.members
                .get(idx.min(self.members.len().saturating_sub(1)))
        })
    }

    fn move_up(&mut self) {
        self.selected =
            codewhale_tui_kit::list_nav::wrap_index(self.selected, self.row_count(), -1);
        self.detail_scroll = 0;
        self.last_mouse_selected = None;
        self.hovered_row.set(None);
    }

    fn move_down(&mut self) {
        self.selected = codewhale_tui_kit::list_nav::wrap_index(self.selected, self.row_count(), 1);
        self.detail_scroll = 0;
        self.last_mouse_selected = None;
        self.hovered_row.set(None);
    }

    /// Apply one [`list_nav`](codewhale_tui_kit::list_nav) motion (#6290), returning
    /// whether it was consumed. Steps wrap; pages travel [`FLEET_ROSTER_PAGE`]
    /// rows and clamp. The region axis is declined so Tab keeps opening the
    /// workers view through the explicit arm below.
    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        use codewhale_tui_kit::list_nav::Motion;
        match motion {
            Motion::Prev => {
                self.move_up();
                true
            }
            Motion::Next => {
                self.move_down();
                true
            }
            Motion::RegionPrev | Motion::RegionNext => false,
            _ => {
                let count = self.row_count();
                if count == 0 {
                    return false;
                }
                let Some(next) = codewhale_tui_kit::list_nav::apply(
                    self.selected,
                    count,
                    FLEET_ROSTER_PAGE,
                    motion,
                ) else {
                    return false;
                };
                self.selected = next;
                self.detail_scroll = 0;
                self.last_mouse_selected = None;
                self.hovered_row.set(None);
                true
            }
        }
    }

    fn select_row(&mut self, row: usize) {
        self.selected = row.min(self.row_count().saturating_sub(1));
        self.detail_scroll = 0;
    }

    fn activate_selected(&self) -> ViewAction {
        if let Some(member) = self.selected_member() {
            let member_id = member.id.clone();
            // Carry the exact member the operator already chose. The host
            // focuses it in the selected v2 Fleet editor, or starts legacy
            // setup from its member id when no Fleet is selected.
            ViewAction::Emit(ViewEvent::FleetRosterOpenSetupRequested { member_id })
        } else {
            ViewAction::Emit(ViewEvent::FleetRosterOpenCoordinatorRequested)
        }
    }

    fn select_or_activate_mouse_row(&mut self, row: usize) -> ViewAction {
        let activate = self.last_mouse_selected == Some(row) && self.selected == row;
        self.select_row(row);
        self.last_mouse_selected = Some(row);
        if activate {
            self.activate_selected()
        } else {
            ViewAction::None
        }
    }

    /// One navigation grammar (grokbuild): arrows move, Enter acts, Tab
    /// moves across the header tabs, Esc closes. `f` is the one named
    /// destination this room has that is not a tab. Detail scrolling
    /// (PgUp/PgDn) works but is not advertised — the pane is short now.
    fn footer_hints(&self) -> Vec<ActionHint> {
        let mut hints = vec![
            ActionHint::new("↑↓", "move"),
            ActionHint::new("Enter", "model & thinking"),
        ];
        hints.extend([
            ActionHint::new("Tab", tr(self.locale, MessageId::FleetRosterWorkers)),
            ActionHint::new("f", "saved teams"),
            ActionHint::new("Esc", "close"),
        ]);
        hints
    }
}

impl ModalView for FleetRosterView {
    fn kind(&self) -> ModalKind {
        ModalKind::FleetRoster
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // A keyboard gesture ends any pending mouse double-click sequence so
        // a later single click can never activate a stale row.
        self.last_mouse_selected = None;
        self.hovered_workers.set(false);
        self.hovered_row.set(None);
        // Shift-modified paging scrolls the detail pane; bare keys drive the
        // row list through the shared vocabulary (#6290, #6014-style split).
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(8);
                    return ViewAction::None;
                }
                KeyCode::PageDown => {
                    self.detail_scroll = self.detail_scroll.saturating_add(8);
                    return ViewAction::None;
                }
                KeyCode::Home => {
                    self.detail_scroll = 0;
                    return ViewAction::None;
                }
                _ => {}
            }
        }
        // Movement keys come from the shared vocabulary (#6290), j/k aliases
        // included — this surface captures no text.
        if let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Enter => self.activate_selected(),
            // #5954: the roster stays on the stack under the view it opens,
            // so `Esc` in workers / saved teams pops back here instead of
            // closing the window. `Emit` (not `EmitAndClose`) is what makes
            // the three Fleet views one stack.
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('w') => {
                ViewAction::Emit(ViewEvent::FleetRosterOpenWorkersRequested)
            }
            KeyCode::Char('f') => ViewAction::Emit(ViewEvent::FleetRosterOpenFleetsRequested),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                self.hovered_workers.set(
                    self.workers_hitbox
                        .get()
                        .is_some_and(|rect| rect.contains((mouse.column, mouse.row).into())),
                );
                let hovered = self
                    .row_hitboxes
                    .borrow()
                    .iter()
                    .find_map(|(rect, action)| {
                        rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                            .then_some(action.row())
                    });
                self.hovered_row.set(hovered);
                ViewAction::None
            }
            MouseEventKind::ScrollUp => {
                self.move_up();
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.move_down();
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self
                    .workers_hitbox
                    .get()
                    .is_some_and(|rect| rect.contains((mouse.column, mouse.row).into()))
                {
                    self.last_mouse_selected = None;
                    return ViewAction::Emit(ViewEvent::FleetRosterOpenWorkersRequested);
                }
                let action = self
                    .row_hitboxes
                    .borrow()
                    .iter()
                    .find_map(|(rect, action)| {
                        rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                            .then_some(*action)
                    });
                action.map_or(ViewAction::None, |action| {
                    self.select_or_activate_mouse_row(action.row())
                })
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        Block::default()
            .style(Style::default().bg(self.surface_bg))
            .render(area, buf);

        let hints = self.footer_hints();
        let content = render_modal_footer(area, buf, &hints);

        // A compact, honest header: the roster is the current room, Workers
        // is a real destination. Editing belongs to the selected member,
        // rather than a decorative Setup tab that never handled clicks.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(content);
        let roster_label = format!(
            " {} · {} ",
            tr(self.locale, MessageId::FleetRosterHeaderLabel),
            tr(self.locale, MessageId::FleetRosterTabRoster)
        );
        let workers_label = format!(" {} ", tr(self.locale, MessageId::FleetRosterWorkers));
        let roster_width = unicode_width::UnicodeWidthStr::width(roster_label.as_str()) as u16;
        let workers_width = unicode_width::UnicodeWidthStr::width(workers_label.as_str()) as u16;
        self.workers_hitbox.set(None);
        // Place Workers at the right edge so even a compact screen retains
        // its full action target; the room label yields first.
        let workers_width = workers_width.min(chunks[0].width);
        let workers = Rect::new(
            chunks[0].right().saturating_sub(workers_width),
            chunks[0].y,
            workers_width,
            u16::from(chunks[0].height > 0),
        );
        if workers.width > 0 && workers.height > 0 {
            self.workers_hitbox.set(Some(workers));
        }
        let title = Rect::new(
            chunks[0].x,
            chunks[0].y,
            roster_width.min(chunks[0].width.saturating_sub(workers_width)),
            workers.height,
        );
        Paragraph::new(Line::from(Span::styled(
            roster_label,
            Style::default().fg(palette::TEXT_PRIMARY).bold(),
        )))
        .render(title, buf);
        let workers_style = if self.hovered_workers.get() {
            menu_style::hovered_row_style().fg(palette::WHALE_ACTION)
        } else {
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::UNDERLINED)
        };
        Paragraph::new(Line::from(Span::styled(workers_label, workers_style))).render(workers, buf);
        if chunks[0].height > 1 {
            let summary = format!(
                " {} · {}",
                self.selected_fleet_line(),
                tr(self.locale, MessageId::FleetRosterMembersCount)
                    .replace("{count}", &(self.members.len() + 1).to_string())
            );
            Paragraph::new(Line::from(Span::styled(
                truncate_view_text(&summary, usize::from(chunks[0].width)),
                Style::default().fg(palette::TEXT_MUTED),
            )))
            .render(
                Rect::new(chunks[0].x, chunks[0].y + 1, chunks[0].width, 1),
                buf,
            );
        }

        self.render_body(chunks[1], buf);
    }
}

impl FleetRosterView {
    /// Scope-explicit selected Fleet line. Paths stay out — receipts name them.
    fn selected_fleet_line(&self) -> String {
        if let Some(error) = &self.load_error {
            return format!("Team selection error — {error}");
        }
        match &self.selected_fleet {
            Some(sel) => format!("Team `{}` · {}", sel.name, sel.scope.long_label()),
            None => "No team selected — built-in team".to_string(),
        }
    }

    fn render_body(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Two columns when there is room, stacked otherwise — same responsive
        // shape as the setup wizard's choice step so nothing truncates at
        // 80x24.
        let (list_area, detail_area) = if area.width >= 56 {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(45),
                    Constraint::Length(2),
                    Constraint::Min(20),
                ])
                .split(area);
            (cols[0], cols[2])
        } else {
            let list_height =
                (self.row_count() as u16 + 1).min(area.height.saturating_sub(1).max(1));
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(list_height), Constraint::Min(1)])
                .split(area);
            (rows[0], rows[1])
        };

        // Row list: the pinned operator first, then one row per member,
        // scrolled so the selection stays visible when the party outgrows
        // the pane.
        let visible_rows = usize::from(list_area.height).max(1);
        let first = self
            .selected
            .saturating_sub(visible_rows.saturating_sub(1))
            .min(
                self.row_count()
                    .saturating_sub(visible_rows.min(self.row_count())),
            );
        let list_width = usize::from(list_area.width);
        let mut list_lines: Vec<Line> = Vec::with_capacity(visible_rows);
        for (line_offset, idx) in (first..(first + visible_rows).min(self.row_count())).enumerate()
        {
            self.row_hitboxes.borrow_mut().push((
                Rect::new(
                    list_area.x,
                    list_area
                        .y
                        .saturating_add(u16::try_from(line_offset).unwrap_or(u16::MAX)),
                    list_area.width,
                    1,
                ),
                FleetRosterRowAction::SelectOrActivate { row: idx },
            ));
            let is_selected = idx == self.selected;
            // Hover tints but never steals the keyboard selection.
            let hovered = !is_selected && self.hovered_row.get() == Some(idx);
            let hover_tint = || menu_style::hovered_row_style();
            let pointer = format!(
                "{} ",
                codewhale_tui_kit::glyphs::selection_marker(is_selected)
            );
            let shadow_badge = idx.checked_sub(1).and_then(|index| {
                member_shadow_badge(self.locale, &self.members[index], &self.shadowed)
            });
            let (text, base_style) = if idx == 0 {
                (
                    format!(
                        "{pointer}@ {}",
                        tr(self.locale, MessageId::FleetRosterOperatorRow)
                    ),
                    Style::default().fg(palette::TEXT_PRIMARY).bold(),
                )
            } else {
                let member = &self.members[idx - 1];
                let mark = member_role_mark(member);
                let member_name = member
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(&member.id))
                    .map_or_else(
                        || member.id.clone(),
                        |name| format!("{name} ({})", member.id),
                    );
                (
                    format!(
                        "{pointer}{mark} {member_name}{}",
                        shadow_badge.as_deref().unwrap_or("")
                    ),
                    Style::default().fg(palette::TEXT_PRIMARY),
                )
            };
            let text = if list_width >= 28 && shadow_badge.is_none() {
                let route = if idx == 0 {
                    self.operator.model.as_str()
                } else {
                    let profile = &self.members[idx - 1].profile;
                    profile
                        .model
                        .as_deref()
                        .filter(|model| !model.trim().is_empty())
                        .unwrap_or_else(|| {
                            if profile.loadout.as_str() == "inherit" {
                                "follow Coordinator"
                            } else {
                                profile.loadout.as_str()
                            }
                        })
                };
                let role_width = (list_width / 2).clamp(14, 24);
                let label = truncate_view_text(&text, role_width);
                let pad = role_width
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(label.as_str()));
                format!(
                    "{label}{}  {}",
                    " ".repeat(pad),
                    truncate_view_text(route, list_width.saturating_sub(role_width + 2))
                )
            } else {
                text
            };
            let style = if is_selected {
                menu_style::selected_row_style()
            } else if hovered {
                base_style.patch(hover_tint())
            } else {
                base_style
            };
            if is_selected || hovered {
                buf.set_style(
                    Rect::new(
                        list_area.x,
                        list_area.y + line_offset as u16,
                        list_area.width,
                        1,
                    ),
                    style,
                );
            }
            list_lines.push(Line::from(Span::styled(
                truncate_view_text(&text, list_width),
                style,
            )));
        }
        Paragraph::new(list_lines).render(list_area, buf);

        // Detail pane for the selected row.
        let lines = if self.operator_selected() {
            operator_detail_lines(&self.operator)
        } else if let Some(member) = self.selected_member() {
            // Whale Teams identity first: the species badge plus species and
            // job. Rendered without a state — a roster member is a profile,
            // not a runtime, so this claims nothing about whether anyone is
            // working. (The hand-drawn portrait that used to open this pane
            // was deleted per the 2026-08-29 founder directive.)
            let mut lines = whale_identity_lines(member, self.locale);
            // Session model is the operator route so "fast" loadouts resolve
            // to the fast sibling the runtime will actually launch.
            lines.extend(member_detail_lines_with_session(
                member,
                Some(self.operator.model.as_str()),
                &self.shadowed,
                self.locale,
            ));
            lines
        } else {
            vec![Line::from(Span::styled(
                "Fleet is empty.",
                Style::default().fg(palette::TEXT_MUTED),
            ))]
        };

        // Same wrapped-row scroll bound as the setup review step: count
        // visual rows so the tail stays reachable.
        let wrap_width = usize::from(detail_area.width).max(1);
        let visual_rows: usize = lines
            .iter()
            .map(|line| line.width().div_ceil(wrap_width).max(1))
            .sum();
        let max_scroll = visual_rows.saturating_sub(usize::from(detail_area.height).max(1));
        let scroll = self.detail_scroll.min(max_scroll);
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((scroll as u16, 0))
            .render(detail_area, buf);
    }
}

/// Species for a roster member: the profile id first (built-in ids are role
/// names), then the resolved worker agent type. Unknown → the plain whale.
fn member_species(member: &AgentProfile) -> whales::WhaleSpecies {
    match whales::WhaleSpecies::for_role_id(&member.id) {
        whales::WhaleSpecies::Plain => {
            whales::WhaleSpecies::for_fleet_role(&roster_member_agent_type(member))
        }
        species => species,
    }
}

/// Identity block for the detail pane: the species badge, then
/// `Name · species · job`. No state is drawn or claimed — a roster member is
/// a profile, not a runtime.
fn whale_identity_lines(member: &AgentProfile, locale: Locale) -> Vec<Line<'static>> {
    let species = member_species(member);
    let theme = &palette::UI_THEME;
    let mut lines: Vec<Line> = Vec::new();
    let mut caption = whales::badge(species, theme);
    caption.push(Span::styled(
        format!(
            " {} · {} · {}",
            species.name(),
            species.animal(locale),
            species.job(locale)
        ),
        Style::default().fg(palette::TEXT_PRIMARY),
    ));
    lines.push(Line::from(caption));
    lines.push(Line::from(""));
    lines
}

fn member_role_mark(member: &AgentProfile) -> &'static str {
    let role = public_role_label(&member.id);
    match role.as_str() {
        "manager" | "explore" => codewhale_tui_kit::glyphs::ROLE_MANAGER,
        "implement" => codewhale_tui_kit::glyphs::ROLE_BUILDER,
        "reviewer" => codewhale_tui_kit::glyphs::ROLE_REVIEWER,
        "test" => codewhale_tui_kit::glyphs::ROLE_VERIFIER,
        "synthesizer" => codewhale_tui_kit::glyphs::ROLE_SYNTHESIZER,
        _ => match roster_member_agent_type(member).as_str() {
            "explore" | "manager" => codewhale_tui_kit::glyphs::ROLE_MANAGER,
            "implement" => codewhale_tui_kit::glyphs::ROLE_BUILDER,
            "reviewer" => codewhale_tui_kit::glyphs::ROLE_REVIEWER,
            "test" => codewhale_tui_kit::glyphs::ROLE_VERIFIER,
            "synthesizer" => codewhale_tui_kit::glyphs::ROLE_SYNTHESIZER,
            _ => codewhale_tui_kit::glyphs::NEUTRAL,
        },
    }
}

/// Shared field renderer for the detail pane.
fn detail_field(lines: &mut Vec<Line<'static>>, label: &str, body: String) {
    lines.push(Line::from(vec![
        Span::styled(
            format!("{label}  "),
            Style::default().fg(palette::TEXT_MUTED).bold(),
        ),
        Span::styled(body, Style::default().fg(palette::TEXT_PRIMARY)),
    ]));
    lines.push(Line::from(""));
}

/// Detail pane for the pinned operator row: the live session route, plus the
/// product truth that the operator is this Fleet's leader.
fn operator_detail_lines(operator: &OperatorInfo) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    detail_field(
        &mut lines,
        "Role",
        "Coordinator — this session's model leads the Fleet".to_string(),
    );
    // Model, provider, and reasoning are one route: one line, same shape
    // as a member's.
    let mut route = format!("{} · {}", operator.model, operator.provider);
    if !operator.reasoning.trim().is_empty() {
        route.push_str(" · ");
        route.push_str(&operator.reasoning);
    }
    detail_field(&mut lines, "Model", route);
    detail_field(&mut lines, "Access", "full session access".to_string());
    // Session-route capability badges (#5038). Use the exact route key rather
    // than the display label so built-in routes get provider-scoped catalog
    // facts; custom routes still fall back conservatively to registry facts.
    if let Some(badges) =
        codewhale_engine::fleet::capability_badges::resolve_route_capability_badges(
            Some(&operator.provider_id),
            &operator.model,
        )
    {
        detail_field(&mut lines, "Capabilities", badges.summary());
    }
    detail_field(
        &mut lines,
        "Description",
        "The Coordinator leads this session. Press Enter to change its model and thinking. \
         Roles set to follow the Coordinator use this route; pinned roles keep their own models."
            .to_string(),
    );
    lines.push(Line::from(Span::styled(
        "saved for this session only",
        Style::default().fg(palette::TEXT_MUTED),
    )));
    lines
}

/// The resolved worker posture for a roster member: what the runtime would
/// actually grant when this member is dispatched (role posture, not the
/// profile's requested permissions).
/// Plain-Access summary for a roster member: what it may do, derived from the
/// same runtime profile dispatch would grant. No internal role/posture words.
fn member_access_summary(member: &AgentProfile) -> String {
    let agent_type = roster_member_agent_type(member);
    let runtime = WorkerRuntimeProfile::for_role(agent_type.clone());
    let write = if runtime.permissions.write {
        "can edit files"
    } else {
        "read-only files"
    };
    let shell = match runtime.shell {
        ShellPolicy::None => "cannot run commands",
        ShellPolicy::ReadOnly => "read-only commands",
        ShellPolicy::Full => "can run commands",
    };
    let network = if runtime.permissions.network {
        "network"
    } else {
        "no network"
    };
    format!("{write} · {shell} · {network}")
}

/// The model truth for a member: explicit model choice, else saved model set,
/// else the session's model. `[subagents]` overrides still win at dispatch.
///
/// When the loadout is `fast`, show that the runtime picks the **fast sibling
/// of the active session model** — not a stale on-disk profile name — so the
/// roster matches what Fleet will actually launch.
fn member_routing_with_session(member: &AgentProfile, session_model: Option<&str>) -> String {
    if let Some(model) = member
        .profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        if let Some(provider) = member
            .profile
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            return format!("model {provider}/{model}");
        }
        return format!("model {model}");
    }
    match member.profile.loadout.as_str() {
        "inherit" => "same model as this session".to_string(),
        "fast" => match session_model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(session) => format!("fast model for {session}"),
            None => "fast model, picked at launch".to_string(),
        },
        loadout => format!("saved model set {loadout}"),
    }
}

fn member_shadow_badge(
    locale: Locale,
    member: &AgentProfile,
    shadowed: &[codewhale_engine::fleet::roster::ShadowedProfile],
) -> Option<String> {
    let layers = layers_from_parts(member, shadowed);
    if layers.len() < 2 {
        return None;
    }
    let personal_ignored = layers
        .iter()
        .any(|layer| !layer.wins && layer.origin == ProfileOrigin::Personal);
    let id = if personal_ignored {
        MessageId::FleetRosterShadowBadgePersonalIgnored
    } else {
        match member.origin {
            ProfileOrigin::Workspace => MessageId::FleetRosterShadowBadgeProjectOverride,
            ProfileOrigin::Personal => MessageId::FleetRosterShadowBadgePersonalOverride,
            ProfileOrigin::Config => MessageId::FleetRosterShadowBadgeConfigOverride,
            ProfileOrigin::Plugin | ProfileOrigin::BuiltIn | ProfileOrigin::ClaudeCode => {
                return None;
            }
        }
    };
    Some(format!("  {}", tr(locale, id)))
}

fn format_profile_layer(layer: &ProfileLayer, locale: Locale) -> String {
    let mark = if layer.wins {
        tr(locale, MessageId::FleetRosterLayerWins)
    } else {
        tr(locale, MessageId::FleetRosterLayerIgnored)
    };
    format!("{} · {} ({mark})", layer.origin, layer.source.display())
}

fn member_detail_lines_with_session(
    member: &AgentProfile,
    session_model: Option<&str>,
    shadowed: &[codewhale_engine::fleet::roster::ShadowedProfile],
    locale: Locale,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    // Role is the member's primary identity; the id/display name only
    // appears when it says something the role does not.
    let role = member.profile.role.name.trim().to_string();
    let display_name = member
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(&member.id));
    let role_line = match display_name {
        Some(name) => format!("{role} — {name} ({})", member.id),
        None if member.id.trim().eq_ignore_ascii_case(&role) => role.clone(),
        None => format!("{role} ({})", member.id),
    };
    detail_field(&mut lines, "Role", role_line);
    // #5098: every layer found for this id, with the winner named. The
    // Origin field still shows the effective copy; this list is the full
    // stack so a personal/config edit is visible when project wins.
    let layers = layers_from_parts(member, shadowed);
    if layers.len() > 1 {
        let body = layers
            .iter()
            .map(|layer| format_profile_layer(layer, locale))
            .collect::<Vec<_>>()
            .join("\n");
        detail_field(
            &mut lines,
            &tr(locale, MessageId::FleetRosterLayersLabel),
            body,
        );
    }
    // Model and provider are attributes of the role: one line, together.
    let model = match (
        member.profile.model.as_deref(),
        codewhale_engine::fleet::identity::friendly_model_name(member),
    ) {
        (Some(model), Some(name)) if !name.eq_ignore_ascii_case(model.trim()) => {
            format!("{name} ({})", model.trim())
        }
        _ => member_routing_with_session(member, session_model),
    };
    let route = match member
        .profile
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        Some(provider) => format!("{model} · {provider}"),
        None => model,
    };
    detail_field(&mut lines, "Model", route);
    detail_field(
        &mut lines,
        "Thinking",
        member
            .profile
            .reasoning_effort
            .clone()
            .unwrap_or_else(|| "Follow Coordinator".to_string()),
    );
    // Slot is internal dispatch vocabulary and duplicates Role — never shown.
    detail_field(&mut lines, "Access", member_access_summary(member));

    // Capability badges for a pinned model, from the shared Fleet resolver
    // (#5038). Unknown models omit the field rather than fabricating facts.
    if let Some(model) = member
        .profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        && let Some(badges) =
            codewhale_engine::fleet::capability_badges::resolve_route_capability_badges(
                member.profile.provider.as_deref(),
                model,
            )
    {
        detail_field(&mut lines, "Capabilities", badges.summary());
    }

    let delegation = &member.profile.delegation;
    if delegation.max_spawn_depth.is_some() || delegation.max_concurrency.is_some() {
        let mut bounds: Vec<String> = Vec::new();
        if let Some(depth) = delegation.max_spawn_depth {
            bounds.push(format!("spawn depth {depth}"));
        }
        if let Some(concurrency) = delegation.max_concurrency {
            bounds.push(format!("concurrency {concurrency}"));
        }
        detail_field(&mut lines, "Delegation", bounds.join(" · "));
    }

    // Only a real overlay earns a field; "none" is the default and says
    // nothing.
    if member.profile.role.instructions.is_some() {
        detail_field(
            &mut lines,
            "Instructions",
            match member.origin {
                ProfileOrigin::Workspace => {
                    format!("custom overlay ({})", member.source.display())
                }
                ProfileOrigin::Personal => {
                    format!("personal overlay ({})", member.source.display())
                }
                _ => "custom overlay".to_string(),
            },
        );
    }

    if let Some(description) = member
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
    {
        detail_field(&mut lines, "Description", description.to_string());
    }

    // Where the member is saved, last and muted: provenance, not identity.
    lines.push(Line::from(Span::styled(
        match member.origin {
            ProfileOrigin::BuiltIn => "saved for all projects (built-in team)".to_string(),
            ProfileOrigin::Workspace => "saved for this project".to_string(),
            _ => format!("saved: {} · {}", member.origin, member.source.display()),
        },
        Style::default().fg(palette::TEXT_MUTED),
    )));

    lines
}

#[cfg(test)]
mod tests;

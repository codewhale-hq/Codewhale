//! Fleet detail — open a saved named team and edit it.
//!
//! Row 0 is the Coordinator's own model; below it one row per member.
//! Editing a Fleet edits that Fleet's file — never the live session route and
//! never a global collection of role profiles. Every write goes through
//! [`codewhale_engine::fleet::store`] with an atomic save and a receipt naming the exact
//! file and scope.

use std::cell::RefCell;
use std::path::PathBuf;

/// Rows a PageUp/PageDown travels. Both lists here are modal and short, so a
/// page is a readable jump rather than a screenful measured at paint time.
const DETAIL_PAGE: usize = 10;
const ROUTE_PICK_PAGE: usize = 10;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::tui::app::App;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
};
use codewhale_agent_contract::role::public_role_label;
use codewhale_engine::fleet::store::{
    FleetFile, FleetMember, FleetOperator, FleetScope, MemberCapability, load_fleet_in_scope,
    save_fleet, set_selected,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;
use codewhale_tui_kit::menu_style;

/// The built-in role vocabulary offered when adding a member, in a useful
/// order. A Fleet member is a role; the user can name anything, these are the
/// known postures.
const KNOWN_ROLES: [&str; 8] = [
    "explore",
    "implement",
    "reviewer",
    "test",
    "manager",
    "advisor",
    // "synthesizer", not "summarizer": the setup wizard's ROLES table owns
    // this token's spelling (profile role_hint + file stem contract), and
    // the whale table + docs agree. Both resolve to Planner posture, but
    // the picker must offer the same word setup does (#6087 item 3).
    "synthesizer",
    "general",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailStep {
    Overview,
    PickRoute,
}

/// The Fleet editor row a route applies to: the Coordinator (row 0) or one
/// member by roster index. Shared with the `/model` picker, which hands a pick
/// back to the editor addressed by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetRouteTarget {
    Operator,
    Member(usize),
}

/// The selected row's saved route, independent of the session's picker memory.
pub struct FleetRouteSelection {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<codewhale_engine::reasoning_preference::ReasoningEffort>,
    pub allow_inherit: bool,
}

/// One selectable route row in the picker step: inherit or a concrete
/// provider/model with its readiness label.
#[derive(Debug, Clone)]
struct RouteRow {
    label: String,
    summary: String,
    provider: Option<String>,
    model: Option<String>,
    /// The provider's fresh live roster no longer lists this model (#6035).
    /// A bundled catalog row can outlive the account's roster, so offering
    /// the route is not proof it is still listed.
    roster_missing: bool,
}

pub struct FleetDetailView {
    fleet: FleetFile,
    editor_id: uuid::Uuid,
    pub(crate) return_to_roster: bool,
    saved_source: Option<String>,
    locale: Locale,
    scope: FleetScope,
    source: PathBuf,
    workspace: PathBuf,
    /// 0 = operator row; 1.. = members.
    selected: usize,
    row_hitboxes: RefCell<Vec<(Rect, usize)>>,
    last_mouse_selected: Option<usize>,
    hovered_row: Option<usize>,
    step: DetailStep,
    pick_target: FleetRouteTarget,
    routes: Vec<RouteRow>,
    /// Highlight position *within the filtered list*, not into `routes`.
    pick_row: usize,
    /// Typed filter for the route picker. Letters filter directly — no mode
    /// to discover — because the list is every configured provider/model
    /// route and arrowing through it was the whole complaint.
    pick_query: String,
    // Inline rename.
    rename_mode: bool,
    rename_input: String,
    // Delete confirmation.
    pending_remove: bool,
    /// The resolved Scout route shown before a run (pinned / verified
    /// companion / inherited / unavailable), refreshed on route edits.
    scout_receipt: Option<String>,
    /// Session route at open, used to resolve the unpinned Scout.
    session_provider: String,
    session_model: String,
}

impl FleetDetailView {
    /// Open a saved Fleet by name and scope. The caller (the list view) names
    /// the scope explicitly, so ambiguity is impossible here.
    pub fn open(app: &App, config: &Config, name: &str, scope: FleetScope) -> Option<Self> {
        Self::open_for_member(app, config, name, scope, None)
    }

    /// Open the exact named team and, when the request came from a roster
    /// member, focus that member in the v2 editor.
    pub(crate) fn open_for_member(
        app: &App,
        config: &Config,
        name: &str,
        scope: FleetScope,
        member_id: Option<&str>,
    ) -> Option<Self> {
        let (fleet, source) = load_fleet_in_scope(name, scope, &app.workspace).ok()?;
        let session_provider = if app.auto_model {
            app.last_effective_provider_identity
                .clone()
                .unwrap_or_else(|| app.provider_identity_for_persistence().to_string())
        } else {
            app.provider_identity_for_persistence().to_string()
        };
        let session_model = if app.auto_model {
            app.last_effective_model
                .clone()
                .unwrap_or_else(|| "auto".to_string())
        } else {
            app.model.clone()
        };
        let mut view = Self::from_parts(
            fleet,
            app.ui_locale,
            scope,
            source,
            app.workspace.clone(),
            config,
            &session_provider,
            &session_model,
        );
        if let Some(member_id) = member_id.map(str::trim).filter(|id| !id.is_empty()) {
            let index = view
                .fleet
                .members
                .iter()
                .position(|member| member.id.eq_ignore_ascii_case(member_id))?;
            view.selected = index + 1;
        }
        Some(view)
    }

    fn from_parts(
        fleet: FleetFile,
        locale: Locale,
        scope: FleetScope,
        source: PathBuf,
        workspace: PathBuf,
        config: &Config,
        session_provider: &str,
        session_model: &str,
    ) -> Self {
        let routes = build_route_rows(config);
        let saved_source = std::fs::read_to_string(&source)
            .ok()
            .filter(|text| FleetFile::parse(text).ok().as_ref() == Some(&fleet));
        let mut view = Self {
            fleet,
            editor_id: uuid::Uuid::new_v4(),
            return_to_roster: false,
            saved_source,
            locale,
            scope,
            source,
            workspace,
            selected: 0,
            row_hitboxes: RefCell::new(Vec::new()),
            last_mouse_selected: None,
            hovered_row: None,
            step: DetailStep::Overview,
            pick_target: FleetRouteTarget::Operator,
            routes,
            pick_row: 0,
            pick_query: String::new(),
            rename_mode: false,
            rename_input: String::new(),
            pending_remove: false,
            scout_receipt: None,
            session_provider: session_provider.to_string(),
            session_model: session_model.to_string(),
        };
        view.refresh_scout_receipt();
        view
    }

    /// Recompute the resolved Scout route from the current fleet draft and
    /// session route. Called at open and after every route edit.
    fn refresh_scout_receipt(&mut self) {
        self.scout_receipt = self.fleet.has_scout().then(|| {
            codewhale_engine::fleet::scout::resolve_scout_route(
                self.fleet.member("scout"),
                &self.session_provider,
                &self.session_model,
            )
            .receipt_line()
        });
    }

    fn row_count(&self) -> usize {
        1 + self.fleet.members.len()
    }

    fn selected_member_idx(&self) -> Option<usize> {
        self.selected.checked_sub(1)
    }

    fn selected_member(&self) -> Option<&FleetMember> {
        self.selected_member_idx()
            .and_then(|idx| self.fleet.members.get(idx))
    }

    /// Whether the file on disk is still exactly what this editor loaded (or
    /// last wrote). Every write from the editor checks it first, so a save
    /// never replaces a newer version written by another session or by hand,
    /// and never recreates a file that was removed meanwhile.
    ///
    /// Known limitation: this is check-then-write, not a lock; a write that
    /// lands between the check and the atomic rename is not detected.
    fn source_unchanged(&self) -> bool {
        self.saved_source.is_some()
            && std::fs::read_to_string(&self.source).ok() == self.saved_source
    }

    /// The pager shown when the source moved underneath the editor.
    fn stale_source_action(&self, title: &str) -> ViewAction {
        ViewAction::Emit(ViewEvent::OpenTextPager {
            title: title.to_string(),
            content: format!(
                "Nothing was written.\n\n{} changed on disk after this editor opened it \
                 (another session or an external edit). Saving now would overwrite \
                 that newer version.\n\nClose this editor and reopen the team to load \
                 the current file, then make the change again.",
                self.source.display()
            ),
        })
    }

    fn start_rename(&mut self) {
        self.rename_mode = true;
        self.rename_input = self.fleet.name.clone();
    }

    fn commit_rename(&mut self) -> Option<ViewAction> {
        let new_name = self.rename_input.trim().to_string();
        if new_name.is_empty() {
            self.rename_mode = false;
            return Some(ViewAction::None);
        }
        if new_name == self.fleet.name {
            self.rename_mode = false;
            return Some(ViewAction::None);
        }
        if !self.source_unchanged() {
            self.rename_mode = false;
            return Some(self.stale_source_action("Rename refused"));
        }
        // The rename must not collide with a different Fleet of the same slug
        // in this scope (the store refuses that at save).
        let old_name = self.fleet.name.clone();
        self.fleet.name = new_name.clone();
        self.rename_mode = false;
        match save_fleet(&self.fleet, self.scope, &self.workspace) {
            Ok(path) => Some(ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged {
                message: format!(
                    "Renamed Team `{old_name}` → `{new_name}` ({}) — wrote {}",
                    self.scope.label(),
                    path.display()
                ),
            })),
            Err(err) => {
                self.fleet.name = old_name;
                Some(ViewAction::Emit(ViewEvent::OpenTextPager {
                    title: "Rename failed".to_string(),
                    content: format!("{err:#}"),
                }))
            }
        }
    }

    /// Rows passing the typed filter, as indices into `routes`.
    fn filtered_routes(&self) -> Vec<usize> {
        let shortlist = matches!(self.pick_target, FleetRouteTarget::Member(idx)
            if self.fleet.members.get(idx).is_some_and(|member| member.shortlist));
        (0..self.routes.len())
            .filter(|idx| {
                let route = &self.routes[*idx];
                if shortlist && (route.provider.is_none() || route.model.is_none()) {
                    return false;
                }
                crate::tui::views::fleet_setup::route_matches_query(
                    &self.pick_query,
                    route.provider.as_deref().unwrap_or(""),
                    route.model.as_deref().unwrap_or(""),
                    *idx == 0,
                )
            })
            .collect()
    }

    /// The `routes` index currently highlighted, or `None` when the filter
    /// excludes everything.
    fn picked_route_index(&self) -> Option<usize> {
        let filtered = self.filtered_routes();
        filtered
            .get(self.pick_row.min(filtered.len().saturating_sub(1)))
            .copied()
    }

    /// Enter the route-picker step for the target.
    fn open_route_picker(&mut self, target: FleetRouteTarget) {
        self.step = DetailStep::PickRoute;
        self.pick_target = target;
        // Preselect the row matching the current pin (or the inherit row).
        self.pick_row = 0;
        self.pick_query.clear();
        let current: Option<(&str, &str)> = match target {
            FleetRouteTarget::Operator => self
                .fleet
                .operator
                .as_ref()
                .map(|op| (op.provider.as_str(), op.model.as_str())),
            FleetRouteTarget::Member(idx) => self
                .fleet
                .members
                .get(idx)
                .and_then(|m| m.provider.as_deref().zip(m.model.as_deref())),
        };
        if let Some((provider, model)) = current {
            for (idx, route_idx) in self.filtered_routes().into_iter().enumerate() {
                let route = &self.routes[route_idx];
                if route.provider.as_deref() == Some(provider)
                    && route.model.as_deref() == Some(model)
                {
                    self.pick_row = idx;
                    break;
                }
            }
        }
    }

    fn apply_route_pick(&mut self) -> Option<ViewAction> {
        let route = self.routes.get(self.picked_route_index()?)?;
        let (provider, model) = (route.provider.clone(), route.model.clone());
        self.set_route(self.pick_target, provider, model);
        self.step = DetailStep::Overview;
        self.rename_mode = false;
        self.route_edit_needs_refresh();
        Some(ViewAction::None)
    }

    /// Pin `target` to `provider`/`model`, or clear its pin when either is
    /// absent so the row inherits the session route again. The Coordinator
    /// keeps its reasoning tier across a route change.
    fn set_route(
        &mut self,
        target: FleetRouteTarget,
        provider: Option<String>,
        model: Option<String>,
    ) {
        match target {
            FleetRouteTarget::Operator => {
                self.fleet.operator = match (provider, model) {
                    (Some(provider), Some(model)) => Some(FleetOperator {
                        provider,
                        model,
                        reasoning: self
                            .fleet
                            .operator
                            .as_ref()
                            .and_then(|op| op.reasoning.clone()),
                    }),
                    _ => None,
                };
            }
            FleetRouteTarget::Member(idx) => {
                if let Some(member) = self.fleet.members.get_mut(idx) {
                    match (provider, model) {
                        (Some(provider), Some(model)) => {
                            member.provider = Some(provider);
                            member.model = Some(model);
                        }
                        _ => {
                            member.provider = None;
                            member.model = None;
                        }
                    }
                }
            }
        }
    }

    /// The row Enter acts on: the Coordinator on row 0, else that member.
    fn selected_route_target(&self) -> FleetRouteTarget {
        match self.selected_member_idx() {
            Some(idx) => FleetRouteTarget::Member(idx),
            None => FleetRouteTarget::Operator,
        }
    }

    pub(crate) fn direct_assignment(&mut self) -> (uuid::Uuid, FleetRouteTarget) {
        self.return_to_roster = true;
        (self.editor_id, self.selected_route_target())
    }

    pub(crate) fn assignment_context(&self) -> (String, String) {
        let role = self
            .selected_member()
            .map(|member| member.id.clone())
            .unwrap_or_else(|| "Coordinator".into());
        (
            role,
            format!("{} · {}", self.fleet.name, self.scope.label()),
        )
    }

    pub(crate) fn is_direct_assignment(&self, editor_id: uuid::Uuid) -> bool {
        self.return_to_roster && self.editor_id == editor_id
    }

    pub(crate) fn route_selection(
        &self,
        editor_id: uuid::Uuid,
        target: FleetRouteTarget,
    ) -> Option<FleetRouteSelection> {
        if editor_id != self.editor_id {
            return None;
        }
        let (provider, model, reasoning, allow_inherit) = match target {
            FleetRouteTarget::Operator => (
                self.fleet.operator.as_ref().map(|op| op.provider.clone()),
                self.fleet.operator.as_ref().map(|op| op.model.clone()),
                self.fleet
                    .operator
                    .as_ref()
                    .and_then(|op| op.reasoning.as_deref()),
                true,
            ),
            FleetRouteTarget::Member(idx) => {
                let member = self.fleet.members.get(idx)?;
                (
                    member.provider.clone(),
                    member.model.clone(),
                    member.reasoning.as_deref(),
                    !member.shortlist,
                )
            }
        };
        Some(FleetRouteSelection {
            provider,
            model,
            reasoning: reasoning.and_then(|value| {
                codewhale_engine::reasoning_preference::ReasoningEffort::parse_strict(value).ok()
            }),
            allow_inherit,
        })
    }

    /// Apply a route the standard `/model` picker resolved for `target` and
    /// write the Fleet file at once, so an Enter-pick is one gesture: pick,
    /// saved. `None`/`None` clears the pin. Returns the receipt to show, or
    /// the reason nothing was written.
    pub(crate) fn apply_picked_route(
        &mut self,
        editor_id: uuid::Uuid,
        target: FleetRouteTarget,
        provider: Option<String>,
        model: Option<String>,
        reasoning: Option<codewhale_engine::reasoning_preference::ReasoningEffort>,
    ) -> Result<String, String> {
        // A picker belongs to one editor instance and the exact saved file
        // it opened. Never recreate a removed file or overwrite newer edits.
        if editor_id != self.editor_id || !self.source_unchanged() {
            return Err(tr(self.locale, MessageId::FleetRoutePickUnavailable).into_owned());
        }
        if let FleetRouteTarget::Member(idx) = target
            && idx >= self.fleet.members.len()
        {
            return Err(tr(self.locale, MessageId::FleetRoutePickUnavailable).into_owned());
        }
        let previous = self.fleet.clone();
        self.set_route(target, provider, model);
        let reasoning = reasoning.map(|effort| effort.as_setting().to_string());
        match target {
            FleetRouteTarget::Operator => {
                if let Some(operator) = self.fleet.operator.as_mut() {
                    operator.reasoning = reasoning;
                }
            }
            FleetRouteTarget::Member(idx) => self.fleet.members[idx].reasoning = reasoning,
        }
        self.route_edit_needs_refresh();
        let route = match target {
            FleetRouteTarget::Operator => self
                .fleet
                .operator
                .as_ref()
                .map(|op| format!("{}/{}", op.provider, op.model)),
            FleetRouteTarget::Member(idx) => {
                let member = &self.fleet.members[idx];
                member
                    .provider
                    .as_deref()
                    .zip(member.model.as_deref())
                    .map(|(provider, model)| format!("{provider}/{model}"))
            }
        };
        match save_fleet(&self.fleet, self.scope, &self.workspace) {
            Ok(path) => {
                self.saved_source = self.fleet.render_toml().ok();
                Ok(tr(self.locale, MessageId::FleetRouteSaved)
                    .replace("{fleet}", &self.fleet.name)
                    .replace(
                        "{route}",
                        &route.unwrap_or_else(|| {
                            tr(self.locale, MessageId::FleetRouteInherited).into_owned()
                        }),
                    )
                    .replace("{path}", &path.display().to_string()))
            }
            Err(err) => {
                self.fleet = previous;
                self.route_edit_needs_refresh();
                Err(tr(self.locale, MessageId::FleetToggleFailed)
                    .replace("{error}", &err.to_string()))
            }
        }
    }

    /// Cycle the reasoning level of the selected row through the supported
    /// tiers. The tier list is the provider's documented vocabulary; a tier a
    /// route cannot genuinely express is never offered.
    fn cycle_reasoning(&mut self) {
        let tiers: &[&str] = match self.selected {
            0 => {
                if let Some(op) = &self.fleet.operator {
                    reasoning_tiers_for_provider(&op.provider)
                } else {
                    &[]
                }
            }
            _ => {
                if let Some(member) = self.selected_member()
                    && !member.shortlist
                    && let Some(provider) = &member.provider
                {
                    reasoning_tiers_for_provider(provider)
                } else {
                    &[]
                }
            }
        };
        if tiers.is_empty() {
            return;
        }
        let slot = match self.selected {
            0 => self.fleet.operator.as_mut().map(|op| &mut op.reasoning),
            _ => self
                .selected_member_idx()
                .and_then(|idx| self.fleet.members.get_mut(idx))
                .map(|m| &mut m.reasoning),
        };
        let Some(slot) = slot else { return };
        let current = slot.as_deref().unwrap_or("inherit");
        let next = match tiers.iter().position(|t| *t == current) {
            Some(pos) => tiers[(pos + 1) % tiers.len()],
            None => tiers[0],
        };
        *slot = if next == "inherit" {
            None
        } else {
            Some(next.to_string())
        };
    }

    /// The scout receipt depends on the member pin and the session route;
    /// reasoning edits don't affect it. Pins refresh it at the next open;
    /// the marker exists so route-edit call sites document that intent.
    fn route_edit_needs_refresh(&mut self) {
        self.refresh_scout_receipt();
    }

    fn toggle_vision_requirement(&mut self) {
        if let Some(member) = self.selected_member_idx()
            && let Some(member) = self.fleet.members.get_mut(member)
            && !member.shortlist
        {
            if member.requires.iter().any(|r| r == "vision") {
                member.requires.retain(|r| r != "vision");
            } else {
                member
                    .requires
                    .push(MemberCapability::Vision.wire_name().to_string());
            }
        }
    }

    fn add_member(&mut self) {
        // First known role not already present.
        let Some(role) = KNOWN_ROLES.iter().find(|role| {
            !self.fleet.members.iter().any(|member| {
                !member.shortlist
                    && public_role_label(member.role_label())
                        .eq_ignore_ascii_case(&public_role_label(role))
            })
        }) else {
            return;
        };
        let id = codewhale_engine::fleet::members::unique_member_id(&self.fleet, role, "role");
        self.fleet.members.push(FleetMember {
            id,
            display_name: None,
            shortlist: false,
            role: role.to_string(),
            provider: None,
            model: None,
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
    }

    fn remove_selected_member(&mut self) {
        if let Some(idx) = self.selected_member_idx() {
            self.fleet.members.remove(idx);
            self.pending_remove = false;
            if self.selected >= self.row_count() {
                self.selected = self.row_count().saturating_sub(1);
            }
        }
    }

    fn save(&self) -> Option<ViewAction> {
        if !self.source_unchanged() {
            return Some(self.stale_source_action("Save refused"));
        }
        match save_fleet(&self.fleet, self.scope, &self.workspace) {
            Ok(path) => Some(ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged {
                message: format!(
                    "Saved Team `{}` ({}) — wrote {}",
                    self.fleet.name,
                    self.scope.long_label(),
                    path.display()
                ),
            })),
            Err(err) => Some(ViewAction::Emit(ViewEvent::OpenTextPager {
                title: "Save failed".to_string(),
                content: format!(
                    "Nothing was written.\n\n{err:#}\n\nFix the issue and save again."
                ),
            })),
        }
    }

    fn copy_to_other_scope(&self) -> Option<ViewAction> {
        let target = self.scope.toggled();
        match save_fleet(&self.fleet, target, &self.workspace) {
            Ok(path) => Some(ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged {
                message: format!(
                    "Copied Team `{}` to {} scope — wrote {}",
                    self.fleet.name,
                    target.label(),
                    path.display()
                ),
            })),
            Err(err) => Some(ViewAction::Emit(ViewEvent::OpenTextPager {
                title: "Copy failed".to_string(),
                content: format!("{err:#}"),
            })),
        }
    }

    fn select_scope(&self, scope: FleetScope) -> Option<ViewAction> {
        match set_selected(&self.fleet.name, scope, &self.workspace) {
            Ok(path) => Some(ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged {
                message: format!(
                    "Selected Team `{}` as {} default — wrote {}",
                    self.fleet.name,
                    scope.label(),
                    path.display()
                ),
            })),
            Err(err) => Some(ViewAction::Emit(ViewEvent::OpenTextPager {
                title: "Selection failed".to_string(),
                content: format!("{err:#}"),
            })),
        }
    }

    fn footer_hints(&self) -> Vec<ActionHint> {
        match self.step {
            DetailStep::PickRoute => vec![
                ActionHint::new("type", "filter"),
                ActionHint::new("↑/↓", "move"),
                ActionHint::new("Enter", "pick"),
                ActionHint::new(
                    "Esc",
                    if self.pick_query.is_empty() {
                        "back"
                    } else {
                        "clear filter"
                    },
                ),
            ],
            DetailStep::Overview => {
                let shortlist = self
                    .selected_member()
                    .is_some_and(|member| member.shortlist);
                let mut hints = vec![
                    ActionHint::new("↑/↓", "move"),
                    ActionHint::new("Enter", tr(self.locale, MessageId::PickerActionModels)),
                    ActionHint::new("r", "rename"),
                    ActionHint::new("s", "save"),
                    ActionHint::new("c", "copy destination"),
                    ActionHint::new("u/w", "select"),
                ];
                if !shortlist {
                    hints.push(ActionHint::new("t", "reasoning"));
                }
                if self.selected > 0 {
                    if !shortlist {
                        hints.push(ActionHint::new("v", "vision"));
                    }
                    hints.push(ActionHint::new("a/d", "add/remove"));
                }
                hints.push(ActionHint::new("Esc", "back"));
                hints
            }
        }
    }
}

impl ModalView for FleetDetailView {
    fn kind(&self) -> ModalKind {
        ModalKind::FleetDetail
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        self.last_mouse_selected = None;
        self.hovered_row = None;
        match self.step {
            DetailStep::PickRoute => match key.code {
                // Esc clears a filter before it leaves, so a mistyped query
                // does not cost the step.
                KeyCode::Esc if !self.pick_query.is_empty() => {
                    self.pick_query.clear();
                    self.pick_row = 0;
                    ViewAction::None
                }
                KeyCode::Esc => {
                    self.step = DetailStep::Overview;
                    ViewAction::None
                }
                // Typing-safe set only: this step feeds `Char(c)` into
                // `pick_query`, so a letter alias would eat the query (#6290).
                _ if codewhale_tui_kit::list_nav::motion_while_typing(&key)
                    .and_then(|m| {
                        codewhale_tui_kit::list_nav::apply(
                            self.pick_row,
                            self.filtered_routes().len(),
                            ROUTE_PICK_PAGE,
                            m,
                        )
                    })
                    .is_some_and(|row| {
                        self.pick_row = row;
                        true
                    }) =>
                {
                    ViewAction::None
                }
                KeyCode::Enter => self.apply_route_pick().unwrap_or(ViewAction::None),
                KeyCode::Backspace => {
                    self.pick_query.pop();
                    self.pick_row = 0;
                    ViewAction::None
                }
                // Letters filter. `j`/`k` used to navigate here, which is why
                // typing a model name did nothing useful.
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.pick_query.push(c);
                    self.pick_row = 0;
                    ViewAction::None
                }
                _ => ViewAction::None,
            },
            DetailStep::Overview => {
                if self.rename_mode {
                    return match key.code {
                        KeyCode::Enter => self.commit_rename().unwrap_or(ViewAction::None),
                        KeyCode::Esc => {
                            self.rename_mode = false;
                            ViewAction::None
                        }
                        KeyCode::Char(c) => {
                            self.rename_input.push(c);
                            ViewAction::None
                        }
                        KeyCode::Backspace => {
                            self.rename_input.pop();
                            ViewAction::None
                        }
                        _ => ViewAction::None,
                    };
                }
                if self.pending_remove {
                    return match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            self.remove_selected_member();
                            ViewAction::None
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            self.pending_remove = false;
                            ViewAction::None
                        }
                        _ => ViewAction::None,
                    };
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
                    _ if codewhale_tui_kit::list_nav::motion(&key)
                        .and_then(|m| {
                            codewhale_tui_kit::list_nav::apply(
                                self.selected,
                                self.row_count(),
                                DETAIL_PAGE,
                                m,
                            )
                        })
                        .is_some_and(|row| {
                            self.selected = row;
                            true
                        }) =>
                    {
                        ViewAction::None
                    }
                    // Enter opens the standard `/model` picker for the row —
                    // catalog, search, readiness and all — and the pick comes
                    // back through `FleetRoutePicked` already saved. `o`/`e`
                    // keep the inline route list for hands that know it.
                    KeyCode::Enter => ViewAction::Emit(ViewEvent::FleetDetailRoutePickRequested {
                        target: self.selected_route_target(),
                        editor_id: self.editor_id,
                    }),
                    KeyCode::Char('o') => {
                        self.open_route_picker(FleetRouteTarget::Operator);
                        ViewAction::None
                    }
                    KeyCode::Char('e') => {
                        if let Some(idx) = self.selected_member_idx() {
                            self.open_route_picker(FleetRouteTarget::Member(idx));
                        }
                        ViewAction::None
                    }
                    KeyCode::Char('t') => {
                        self.cycle_reasoning();
                        ViewAction::None
                    }
                    KeyCode::Char('v') => {
                        self.toggle_vision_requirement();
                        ViewAction::None
                    }
                    KeyCode::Char('a') => {
                        self.add_member();
                        ViewAction::None
                    }
                    KeyCode::Char('d') if self.selected > 0 => {
                        self.pending_remove = true;
                        ViewAction::None
                    }
                    KeyCode::Char('r') => {
                        self.start_rename();
                        ViewAction::None
                    }
                    KeyCode::Char('s') => self.save().unwrap_or(ViewAction::None),
                    KeyCode::Char('c') => self.copy_to_other_scope().unwrap_or(ViewAction::None),
                    KeyCode::Char('u') => self
                        .select_scope(FleetScope::Personal)
                        .unwrap_or(ViewAction::None),
                    KeyCode::Char('w') => self
                        .select_scope(FleetScope::Workspace)
                        .unwrap_or(ViewAction::None),
                    _ => ViewAction::None,
                }
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        // Mouse browsing cannot bypass a rename or removal confirmation.
        if self.step != DetailStep::Overview || self.rename_mode || self.pending_remove {
            return ViewAction::None;
        }
        let hit = self.row_hitboxes.borrow().iter().find_map(|(rect, row)| {
            rect.contains((mouse.column, mouse.row).into())
                .then_some(*row)
        });
        match mouse.kind {
            MouseEventKind::Moved => self.hovered_row = hit,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                return self.handle_key(KeyEvent::new(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                ));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(row) = hit {
                    let open = self.selected == row && self.last_mouse_selected == Some(row);
                    self.selected = row;
                    self.last_mouse_selected = Some(row);
                    if open {
                        return self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                } else {
                    self.last_mouse_selected = None;
                }
            }
            _ => {}
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        Clear.render(area, buf);
        Block::default()
            .style(Style::default().bg(palette::WHALE_BG))
            .render(area, buf);

        let hints = self.footer_hints();
        let content = render_modal_footer(area, buf, &hints);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(1)])
            .split(content);

        // Header.
        let title = if self.rename_mode {
            format!("Renaming: {}▏", self.rename_input)
        } else {
            format!(
                "Team `{}` · {} scope · {}",
                self.fleet.name,
                self.scope.label(),
                self.source.display()
            )
        };
        let mut header = vec![
            Line::from(vec![
                Span::styled("─ Team ", Style::default().fg(palette::WHALE_ACTION).bold()),
                Span::styled(title, Style::default().fg(palette::TEXT_SECONDARY)),
            ]),
            Line::from(""),
        ];
        let operator_line = match &self.fleet.operator {
            Some(op) => format!("  Coordinator: {}/{}", op.provider, op.model),
            None => "  Coordinator: uses the session's model".to_string(),
        };
        header.push(Line::from(Span::styled(
            operator_line,
            Style::default().fg(palette::TEXT_DIM),
        )));
        if let Some(scout) = &self.scout_receipt {
            header.push(Line::from(Span::styled(
                format!("  scout → {scout}"),
                Style::default().fg(palette::TEXT_DIM),
            )));
        }
        Paragraph::new(header)
            .wrap(Wrap { trim: false })
            .render(chunks[0], buf);

        match self.step {
            DetailStep::Overview => self.render_overview(chunks[1], buf),
            DetailStep::PickRoute => self.render_pick_route(chunks[1], buf),
        }
    }
}

impl FleetDetailView {
    /// A saved route pin is drifted when the `(provider, model)` pair is not
    /// among the routes the picker can currently offer (the provider table
    /// was removed), or when the provider's fresh live roster no longer
    /// lists the model even though a bundled row still offers it (#6035).
    /// The pin may still serve upstream, so this only flags; it never
    /// rewrites.
    fn pin_drifted(&self, provider: &str, model: &str) -> bool {
        self.routes
            .iter()
            .find(|row| {
                row.provider
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case(provider))
                    && row
                        .model
                        .as_deref()
                        .is_some_and(|m| m.eq_ignore_ascii_case(model))
            })
            .is_none_or(|row| row.roster_missing)
    }

    fn render_overview(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let rows_visible = usize::from(area.height).max(1);
        let scroll = self.selected.saturating_sub(rows_visible.saturating_sub(1));
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Keep the Coordinator in place until it actually scrolls offscreen.
        if scroll == 0 {
            let selected = self.selected == 0;
            let base = if selected {
                Style::default().fg(palette::WHALE_ACTION).bold()
            } else {
                Style::default().fg(palette::TEXT_SECONDARY)
            };
            let operator_text = match &self.fleet.operator {
                Some(op) => format!("{}/{}", op.provider, op.model),
                None => "inherits session route".to_string(),
            };
            let reasoning = self
                .fleet
                .operator
                .as_ref()
                .and_then(|op| op.reasoning.as_deref())
                .unwrap_or("inherit");
            let drifted = self
                .fleet
                .operator
                .as_ref()
                .is_some_and(|op| self.pin_drifted(&op.provider, &op.model));
            let mut spans = vec![
                Span::styled(if selected { "» " } else { "  " }, base),
                Span::styled("operator", base),
                Span::styled("  ", Style::default()),
                Span::styled(operator_text, Style::default().fg(palette::TEXT_MUTED)),
                Span::styled(
                    format!(" · reasoning: {reasoning}"),
                    Style::default().fg(palette::TEXT_DIM),
                ),
            ];
            if drifted {
                spans.push(Span::styled(
                    tr(self.locale, MessageId::FleetRouteNotInCatalog),
                    Style::default().fg(palette::STATUS_WARNING),
                ));
            }
            self.row_hitboxes
                .borrow_mut()
                .push((Rect::new(area.x, area.y, area.width, 1), 0));
            lines.push(Line::from(spans));
        }

        for (idx, member) in self.fleet.members.iter().enumerate() {
            let row = 1 + idx;
            if row < scroll || row >= scroll + rows_visible {
                continue;
            }
            self.row_hitboxes.borrow_mut().push((
                Rect::new(area.x, area.y + lines.len() as u16, area.width, 1),
                row,
            ));
            let selected = row == self.selected;
            let base = if selected {
                Style::default().fg(palette::WHALE_ACTION).bold()
            } else {
                Style::default().fg(palette::TEXT_SECONDARY)
            };
            let route = match (&member.provider, &member.model) {
                (Some(p), Some(m)) => format!("model {p}/{m}"),
                _ => "same model as this session".to_string(),
            };
            let reasoning = member.reasoning.as_deref().unwrap_or("inherit");
            let vision = if member.requires.iter().any(|r| r == "vision") {
                " · vision"
            } else {
                ""
            };
            if self.pending_remove && selected {
                lines.push(Line::from(vec![Span::styled(
                    format!("  Remove member `{}`? y/n", member.id),
                    Style::default().fg(palette::WHALE_ERROR),
                )]));
            } else {
                let role = if member.shortlist {
                    String::new()
                } else {
                    format!(" · role {}", public_role_label(member.role_label()))
                };
                let member_label = member
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(&member.id))
                    .map_or_else(
                        || member.id.clone(),
                        |name| format!("{name} ({})", member.id),
                    );
                let drifted = matches!(
                    (&member.provider, &member.model),
                    (Some(provider), Some(model)) if self.pin_drifted(provider, model)
                );
                let mut spans = vec![
                    Span::styled(if selected { "» " } else { "  " }, base),
                    Span::styled(member_label, base),
                    Span::styled(role, Style::default().fg(palette::TEXT_SECONDARY)),
                    Span::styled("  ", Style::default()),
                    Span::styled(route, Style::default().fg(palette::TEXT_MUTED)),
                    Span::styled(
                        if member.shortlist {
                            String::new()
                        } else {
                            format!(" · reasoning: {reasoning}{vision}")
                        },
                        Style::default().fg(palette::TEXT_DIM),
                    ),
                ];
                if drifted {
                    spans.push(Span::styled(
                        tr(self.locale, MessageId::FleetRouteNotInCatalog),
                        Style::default().fg(palette::STATUS_WARNING),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
        for ((rect, row), line) in self.row_hitboxes.borrow().iter().zip(lines.iter_mut()) {
            let style = if *row == self.selected && !self.pending_remove {
                Some(menu_style::selected_row_style())
            } else if self.hovered_row == Some(*row) {
                Some(menu_style::hovered_row_style())
            } else {
                None
            };
            if let Some(style) = style {
                buf.set_style(*rect, style);
                line.style = style;
                for span in &mut line.spans {
                    // Focus owns the fill, while warnings retain their
                    // semantic ink and explicit diagnostic label.
                    span.style = match span.style.fg {
                        Some(ink)
                            if ink == palette::STATUS_WARNING || ink == palette::WHALE_ERROR =>
                        {
                            style.fg(ink)
                        }
                        _ => style,
                    };
                }
            }
        }
        Paragraph::new(ratatui::text::Text::from(lines)).render(area, buf);
    }

    fn render_pick_route(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let filtered = self.filtered_routes();
        let rows_visible = usize::from(area.height).max(1);
        let pick_scroll = self.pick_row.saturating_sub(rows_visible.saturating_sub(1));
        let target_label = match self.pick_target {
            FleetRouteTarget::Operator => "operator",
            FleetRouteTarget::Member(idx) => self
                .fleet
                .members
                .get(idx)
                .map(|m| m.id.as_str())
                .unwrap_or("member"),
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            if self.pick_query.is_empty() {
                format!("  Model for {target_label} — type to filter, Enter picks.")
            } else {
                format!(
                    "  Model for {target_label} — filter: {} ({} of {})",
                    self.pick_query,
                    filtered.len(),
                    self.routes.len()
                )
            },
            Style::default().fg(palette::TEXT_MUTED),
        )));
        lines.push(Line::from(""));
        if filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No route matches. Backspace to widen the filter.",
                Style::default().fg(palette::TEXT_DIM),
            )));
        }
        for (position, route_idx) in filtered.iter().enumerate() {
            if position < pick_scroll || position >= pick_scroll + rows_visible {
                continue;
            }
            let route = &self.routes[*route_idx];
            let selected = position == self.pick_row.min(filtered.len().saturating_sub(1));
            let base = if selected {
                Style::default().fg(palette::WHALE_ACTION).bold()
            } else {
                Style::default().fg(palette::TEXT_SECONDARY)
            };
            let mut spans = vec![
                Span::styled(if selected { "» " } else { "  " }, base),
                Span::styled(route.label.clone(), base),
                Span::styled("  ", Style::default()),
                Span::styled(
                    route.summary.clone(),
                    Style::default().fg(palette::TEXT_DIM),
                ),
            ];
            // Where the pin is edited, say so before it is picked (#6035).
            if route.roster_missing {
                spans.push(Span::styled(
                    tr(self.locale, MessageId::FleetRouteNotInCatalog),
                    Style::default().fg(palette::STATUS_WARNING),
                ));
            }
            lines.push(Line::from(spans));
        }
        Paragraph::new(ratatui::text::Text::from(lines)).render(area, buf);
    }
}

/// Build the model-picker rows: "same as session" first, then every
/// concrete model across configured providers, with its readiness label —
/// the same list the fleet setup wizard's Model step shows.
fn build_route_rows(config: &Config) -> Vec<RouteRow> {
    let mut rows = vec![RouteRow {
        label: "same as session".to_string(),
        summary: String::new(),
        provider: None,
        model: None,
        roster_missing: false,
    }];
    let health = codewhale_engine::provider_readiness::ProviderReadinessSnapshot::default();
    let active = config.active_provider_identity().ok();
    let routes = super::fleet_setup::cross_provider_model_routes(config, active.as_ref(), &health);
    for (provider, model, readiness) in routes {
        let provider_label = crate::tui::views::fleet_setup::provider_display_label(&provider);
        let readiness_label = readiness
            .blocked_reason()
            .map(|r| r.into_owned())
            .unwrap_or_else(|| readiness.label().into_owned());
        let roster_missing = codewhale_engine::provider_catalog_live::pin_missing_from_fresh_roster(
            config, &provider, &model,
        ) == Some(true);
        rows.push(RouteRow {
            label: format!("{provider_label}/{model}"),
            summary: readiness_label,
            provider: Some(provider),
            model: Some(model),
            roster_missing,
        });
    }
    rows
}

/// Reasoning tiers a route may genuinely express, keyed by provider class.
/// A tier a provider cannot wire is never offered (no `max` where a route
/// has none). `inherit` is always first.
fn reasoning_tiers_for_provider(provider: &str) -> &'static [&'static str] {
    // Tiers are keyed by the provider's exact id from the catalog, never
    // guessed from a display name. A tier a route cannot genuinely express
    // is not offered.
    match provider
        .to_ascii_lowercase()
        .replace(['_', '-'], "")
        .as_str()
    {
        "deepseek" | "deepseekcn" | "deepseekanthropic" => {
            &["inherit", "off", "low", "high", "max"]
        }
        "moonshot" | "kimi" | "kimicode" => &["inherit", "off", "low", "medium", "high"],
        "openaicodex" => &["inherit", "off", "minimal", "high"],
        _ => &["inherit", "off", "low", "medium", "high"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiOptions};
    use codewhale_engine::fleet::store::FleetFile;
    use codewhale_runtime::config::Config;

    fn app_in(workspace: PathBuf) -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(workspace.clone())
        };
        let mut app = App::new(options, &Config::default());
        app.workspace = workspace;
        app
    }

    fn sample_fleet(name: &str) -> FleetFile {
        let mut fleet = FleetFile::new(name.to_string(), None).unwrap();
        fleet.operator = Some(FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            reasoning: None,
        });
        fleet.members.push(FleetMember {
            id: "scout".to_string(),
            display_name: Some("Flash Scout".to_string()),
            shortlist: false,
            role: "scout".to_string(),
            provider: None,
            model: None,
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
        fleet
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn open_loads_the_fleet_by_name_and_scope() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("DeepSeek Flash");
        let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut app = app_in(ws.path().to_path_buf());
        let view = FleetDetailView::open(
            &app,
            &Config::default(),
            "DeepSeek Flash",
            FleetScope::Workspace,
        )
        .expect("open");
        assert_eq!(view.fleet.name, "DeepSeek Flash");
        assert_eq!(view.scope, FleetScope::Workspace);
        assert_eq!(view.source, path);
        assert_eq!(view.row_count(), 2); // operator + scout

        let mut duplicate_roles = sample_fleet("Duplicate Roles");
        duplicate_roles.members.push(FleetMember {
            id: "fast-scout".to_string(),
            display_name: Some("Fast Scout".to_string()),
            shortlist: false,
            role: "scout".to_string(),
            provider: None,
            model: None,
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
        save_fleet(&duplicate_roles, FleetScope::Workspace, ws.path())
            .expect("save duplicate-role Fleet");
        let focused = FleetDetailView::open_for_member(
            &app,
            &Config::default(),
            "Duplicate Roles",
            FleetScope::Workspace,
            Some("fast-scout"),
        )
        .expect("open focused member");
        assert_eq!(focused.selected, 2);
        assert_eq!(
            focused.selected_member().map(|member| member.id.as_str()),
            Some("fast-scout")
        );

        // A missing fleet fails to open (the host shows the error receipt).
        app.workspace = ws.path().to_path_buf();
        assert!(
            FleetDetailView::open(&app, &Config::default(), "Nope", FleetScope::Workspace)
                .is_none()
        );
    }

    #[test]
    fn fleet_role_mouse_selection_opens_the_same_model_picker_as_enter() {
        let ws = tempfile::TempDir::new().unwrap();
        save_fleet(&sample_fleet("Team"), FleetScope::Workspace, ws.path()).unwrap();
        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Team",
            FleetScope::Workspace,
        )
        .expect("open");
        for (width, height) in [(40, 12), (80, 24), (140, 40)] {
            view.selected = 0;
            view.last_mouse_selected = None;
            let area = Rect::new(0, 0, width, height);
            view.render(area, &mut Buffer::empty(area));
            let hit = view
                .row_hitboxes
                .borrow()
                .iter()
                .find_map(|(rect, row)| (*row == 1).then_some(*rect))
                .expect("member row");
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            };
            assert!(matches!(view.handle_mouse(click), ViewAction::None));
            assert_eq!(view.selected, 1);
            // Selection must not move the Coordinator out of the viewport.
            view.render(area, &mut Buffer::empty(area));
            assert_eq!(view.row_hitboxes.borrow()[0].1, 0);
            assert_eq!(view.row_hitboxes.borrow()[1].0, hit);
            assert!(matches!(view.handle_mouse(click),
                ViewAction::Emit(ViewEvent::FleetDetailRoutePickRequested {
                    target: FleetRouteTarget::Member(0), editor_id,
                }) if editor_id == view.editor_id));
            assert!(matches!(view.handle_key(key(KeyCode::Enter)),
                ViewAction::Emit(ViewEvent::FleetDetailRoutePickRequested {
                    target: FleetRouteTarget::Member(0), editor_id,
                }) if editor_id == view.editor_id));
            // Clicking never skips an active destructive confirmation.
            view.pending_remove = true;
            assert!(matches!(view.handle_mouse(click), ViewAction::None));
            view.pending_remove = false;
            let empty = Rect::new(0, 0, 0, 0);
            view.render(empty, &mut Buffer::empty(empty));
            assert!(view.row_hitboxes.borrow().is_empty());
        }
    }

    /// Enter on a row asks the host for the standard `/model` picker, and the
    /// route it hands back is applied and written in one step.
    #[test]
    fn enter_asks_for_the_model_picker_and_a_pick_saves_the_row() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Team");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Team",
            FleetScope::Workspace,
        )
        .expect("open");

        // Row 0 is the Coordinator; the first member sits under it.
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::Emit(ViewEvent::FleetDetailRoutePickRequested {
                target: FleetRouteTarget::Operator, editor_id
            }) if editor_id == view.editor_id
        ));
        view.handle_key(key(KeyCode::Down));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter)),
            ViewAction::Emit(ViewEvent::FleetDetailRoutePickRequested {
                target: FleetRouteTarget::Member(0), editor_id
            }) if editor_id == view.editor_id
        ));

        let receipt = view
            .apply_picked_route(
                view.editor_id,
                FleetRouteTarget::Member(0),
                Some("openai".to_string()),
                Some("gpt-5.6".to_string()),
                Some(codewhale_engine::reasoning_preference::ReasoningEffort::High),
            )
            .expect("saved");
        assert!(receipt.contains("openai/gpt-5.6"), "{receipt}");
        let (saved, _) = load_fleet_in_scope("Team", FleetScope::Workspace, ws.path()).unwrap();
        assert_eq!(saved.members[0].provider.as_deref(), Some("openai"));
        assert_eq!(saved.members[0].model.as_deref(), Some("gpt-5.6"));
        assert_eq!(saved.members[0].reasoning.as_deref(), Some("high"));
        assert!(
            view.scout_receipt
                .as_deref()
                .unwrap()
                .contains("openai/gpt-5.6")
        );

        // Clearing the pin returns the member to the session route.
        view.apply_picked_route(
            view.editor_id,
            FleetRouteTarget::Member(0),
            None,
            None,
            None,
        )
        .expect("saved");
        let (saved, _) = load_fleet_in_scope("Team", FleetScope::Workspace, ws.path()).unwrap();
        assert_eq!(saved.members[0].provider, None);
        assert_eq!(saved.members[0].model, None);

        // A row that no longer exists writes nothing.
        assert!(
            view.apply_picked_route(
                view.editor_id,
                FleetRouteTarget::Member(99),
                Some("openai".to_string()),
                Some("gpt-5.6".to_string()),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn route_pick_refuses_changed_missing_or_different_editor_without_overwriting() {
        for change in ["replace", "reorder", "edit", "remove", "different-editor"] {
            let ws = tempfile::TempDir::new().unwrap();
            let mut fleet = sample_fleet("Team");
            let mut second = fleet.members[0].clone();
            second.id = "reviewer".into();
            second.role = "reviewer".into();
            fleet.members.push(second);
            let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
            let mut view = FleetDetailView::open(
                &app_in(ws.path().to_path_buf()),
                &Config::default(),
                "Team",
                FleetScope::Workspace,
            )
            .unwrap();
            match change {
                "replace" => {
                    fleet.members[0].id = "replacement".into();
                    save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
                }
                "reorder" => {
                    fleet.members.swap(0, 1);
                    save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
                }
                "edit" => {
                    let bytes = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(&path, format!("{bytes}\n# new user note\n")).unwrap();
                }
                "remove" => std::fs::remove_file(&path).unwrap(),
                _ => {}
            }
            let before = std::fs::read(&path).ok();
            let draft = view.fleet.clone();
            let editor_id = if change == "different-editor" {
                uuid::Uuid::new_v4()
            } else {
                view.editor_id
            };
            assert!(
                view.apply_picked_route(
                    editor_id,
                    FleetRouteTarget::Member(0),
                    Some("openai".into()),
                    Some("gpt-5.6".into()),
                    None,
                )
                .is_err(),
                "{change}"
            );
            assert_eq!(std::fs::read(&path).ok(), before, "{change}");
            assert_eq!(view.fleet, draft, "{change}");
        }
    }

    #[test]
    fn failed_route_pick_preserves_the_editor_and_saved_team() {
        let ws = tempfile::TempDir::new().unwrap();
        let mut fleet = sample_fleet("Shortlist");
        let member = &mut fleet.members[0];
        member.shortlist = true;
        member.role.clear();
        member.provider = Some("openai".to_string());
        member.model = Some("gpt-5.6".to_string());
        let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Shortlist",
            FleetScope::Workspace,
        )
        .unwrap();
        // A shortlisted row must have an explicit route. Failed validation
        // must not leave the editor displaying a change that never saved.
        assert!(
            view.apply_picked_route(
                view.editor_id,
                FleetRouteTarget::Member(0),
                None,
                None,
                None
            )
            .is_err()
        );
        assert_eq!(view.fleet.members[0].provider.as_deref(), Some("openai"));
        assert_eq!(view.fleet.members[0].model.as_deref(), Some("gpt-5.6"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn rename_commits_and_names_the_receipt() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Old Name");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Old Name",
            FleetScope::Workspace,
        )
        .expect("open");

        view.handle_key(key(KeyCode::Char('r')));
        assert!(view.rename_mode);
        // The input starts filled with the current name; clear it, then type.
        for _ in "Old Name".chars() {
            view.handle_key(key(KeyCode::Backspace));
        }
        for ch in "New Name".chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message }) = action else {
            panic!("expected FleetStoreChanged, got {action:?}");
        };
        assert!(
            message.contains("Renamed Team `Old Name` → `New Name`"),
            "{message}"
        );
        // The on-disk file now carries the new name.
        let (loaded, _) = codewhale_engine::fleet::store::load_fleet_in_scope(
            "New Name",
            FleetScope::Workspace,
            ws.path(),
        )
        .expect("reload");
        assert_eq!(loaded.name, "New Name");
    }

    #[test]
    fn operator_route_pick_pins_and_inherit_clears() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Fleet A");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet A",
            FleetScope::Workspace,
        )
        .expect("open");
        assert!(view.fleet.operator.is_some());

        // Enter the operator picker, choose the inherit row.
        view.handle_key(key(KeyCode::Char('o')));
        assert_eq!(view.step, DetailStep::PickRoute);
        view.pick_row = 0;
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, DetailStep::Overview);
        assert!(view.fleet.operator.is_none(), "inherit row clears the pin");

        // Re-enter and pick the first concrete route row.
        view.handle_key(key(KeyCode::Char('o')));
        view.pick_row = 1;
        view.handle_key(key(KeyCode::Enter));
        let op = view.fleet.operator.as_ref().expect("pinned operator");
        assert!(!op.provider.is_empty() && !op.model.is_empty());
    }

    /// "It is too hard to assign a model from a specific provider to a
    /// specific fleet role." The picker listed every configured
    /// provider/model route and offered only arrow keys; `j` and `k` moved
    /// the highlight instead of typing. Letters now narrow the list.
    #[test]
    fn typing_narrows_the_route_picker_and_enter_picks_from_the_narrowed_list() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Fleet Filter");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet Filter",
            FleetScope::Workspace,
        )
        .expect("open");

        view.handle_key(key(KeyCode::Char('o')));
        assert_eq!(view.step, DetailStep::PickRoute);
        let unfiltered = view.filtered_routes().len();
        assert!(unfiltered > 1, "the picker needs rows to narrow");

        // Type the provider of a concrete row and confirm the list shrinks to
        // rows that actually mention it.
        let target = view.routes[1..]
            .iter()
            .find_map(|route| route.provider.clone())
            .expect("a concrete route row");
        for ch in target.chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        let filtered = view.filtered_routes();
        assert!(!filtered.is_empty(), "the typed provider must match itself");
        assert!(
            filtered.len() < unfiltered || unfiltered == filtered.len(),
            "filtering must never grow the list"
        );
        for idx in &filtered {
            let route = &view.routes[*idx];
            assert!(
                crate::tui::views::fleet_setup::route_matches_query(
                    &target,
                    route.provider.as_deref().unwrap_or(""),
                    route.model.as_deref().unwrap_or(""),
                    *idx == 0,
                ),
                "row {:?} survived a filter it does not match",
                route.label
            );
        }

        // Enter picks from the narrowed list, not from the raw index.
        let expected = view.routes[filtered[0]].clone();
        view.handle_key(key(KeyCode::Enter));
        assert_eq!(view.step, DetailStep::Overview);
        match (expected.provider, expected.model) {
            (Some(provider), Some(model)) => {
                let op = view.fleet.operator.as_ref().expect("pinned operator");
                assert_eq!(op.provider, provider);
                assert_eq!(op.model, model);
            }
            _ => assert!(view.fleet.operator.is_none(), "inherit row clears the pin"),
        }

        // Backspace widens again, and Esc clears a filter before it leaves.
        view.handle_key(key(KeyCode::Char('o')));
        view.handle_key(key(KeyCode::Char('z')));
        view.handle_key(key(KeyCode::Char('z')));
        view.handle_key(key(KeyCode::Backspace));
        assert_eq!(view.pick_query, "z");
        view.handle_key(key(KeyCode::Esc));
        assert!(view.pick_query.is_empty());
        assert_eq!(
            view.step,
            DetailStep::PickRoute,
            "the first Esc spends itself on the filter"
        );
        view.handle_key(key(KeyCode::Esc));
        assert_eq!(view.step, DetailStep::Overview);
    }

    #[test]
    fn member_edit_pins_route_and_toggles_vision() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Fleet B");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet B",
            FleetScope::Workspace,
        )
        .expect("open");

        // Select the scout member (row 1) and pin the first concrete route.
        view.selected = 1;
        view.handle_key(key(KeyCode::Char('e')));
        assert_eq!(view.step, DetailStep::PickRoute);
        view.pick_row = 1;
        view.handle_key(key(KeyCode::Enter));
        let member = view.fleet.member("scout").expect("scout");
        assert!(
            member.provider.is_some() && member.model.is_some(),
            "scout must be pinned: {member:?}"
        );

        // Vision requirement toggles on and off.
        view.handle_key(key(KeyCode::Char('v')));
        assert!(
            view.fleet
                .member("scout")
                .unwrap()
                .requires
                .contains(&"vision".to_string())
        );
        view.handle_key(key(KeyCode::Char('v')));
        assert!(view.fleet.member("scout").unwrap().requires.is_empty());
    }

    #[test]
    fn shortlist_editor_preserves_exact_route_and_cannot_select_inherit() {
        let _lock = crate::test_support::lock_test_env();
        let ws = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", ws.path().join("home"));
        let mut fleet = FleetFile::new("Shortlist editor".into(), None).unwrap();
        fleet.members.push(
            serde_json::from_value(serde_json::json!({
                "id": "choice", "shortlist": true,
                "provider": "deepseek", "model": "deepseek-v4-pro",
            }))
            .unwrap(),
        );
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        let config = {
            let __cw_config_init_0_0 = Some("deepseek".into());
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        }
        .with_legacy_root(Some("test-key".into()), None);
        let mut view = FleetDetailView::open_for_member(
            &app_in(ws.path().to_path_buf()),
            &config,
            &fleet.name,
            FleetScope::Workspace,
            Some("choice"),
        )
        .unwrap();

        let before = view.fleet.members[0].clone();
        for code in [KeyCode::Char('t'), KeyCode::Char('v')] {
            view.handle_key(key(code));
        }
        assert_eq!(
            view.fleet.members[0], before,
            "role-only keys cannot alter a shortlist choice"
        );
        assert!(
            view.footer_hints()
                .iter()
                .all(|hint| !matches!(hint.key.as_ref(), "t" | "v"))
        );
        let area = Rect::new(0, 0, 160, 8);
        let mut buf = Buffer::empty(area);
        view.render_overview(area, &mut buf);
        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let choice_row = rows
            .iter()
            .find(|row| row.contains("choice"))
            .expect("shortlist row rendered");
        assert!(
            !choice_row.contains("role ")
                && !choice_row.contains("reasoning:")
                && !choice_row.contains("vision"),
            "{choice_row}"
        );

        view.handle_key(key(KeyCode::Char('e')));
        assert_eq!(view.step, DetailStep::PickRoute);
        let filtered = view.filtered_routes();
        assert!(!filtered.is_empty());
        assert!(
            filtered.iter().all(|idx| {
                view.routes[*idx].provider.is_some() && view.routes[*idx].model.is_some()
            }),
            "shortlist entries must not offer inherited routes"
        );
        let selected = &view.routes[view.picked_route_index().expect("current route selected")];
        assert_eq!(selected.provider.as_deref(), Some("deepseek"));
        assert_eq!(selected.model.as_deref(), Some("deepseek-v4-pro"));
        view.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s'))),
            ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { .. })
        ));
        let (reloaded, _) =
            load_fleet_in_scope(&fleet.name, FleetScope::Workspace, ws.path()).unwrap();
        assert_eq!(
            reloaded, fleet,
            "editing a shortlist preserves the complete route and marker"
        );
    }

    #[test]
    fn overview_flags_a_saved_route_the_picker_can_no_longer_offer() {
        let ws = tempfile::TempDir::new().unwrap();
        let mut fleet = sample_fleet("Drifted");
        fleet.members[0].provider = Some("gone-provider".to_string());
        fleet.members[0].model = Some("gone-model".to_string());
        fleet.members.push(FleetMember {
            id: "kept".to_string(),
            display_name: None,
            shortlist: false,
            role: "reviewer".to_string(),
            provider: Some("deepseek".to_string()),
            model: Some("deepseek-v4-pro".to_string()),
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        });
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Drifted",
            FleetScope::Workspace,
        )
        .expect("open");

        assert!(view.pin_drifted("gone-provider", "gone-model"));
        assert!(!view.pin_drifted("deepseek", "deepseek-v4-pro"));

        let area = Rect::new(0, 0, 160, 8);
        let mut buf = Buffer::empty(area);
        view.render_overview(area, &mut buf);
        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let drifted_row = rows
            .iter()
            .find(|row| row.contains("gone-model"))
            .expect("drifted member row rendered");
        assert!(
            drifted_row.contains("not in current catalog"),
            "{drifted_row}"
        );
        let offered_row = rows
            .iter()
            .find(|row| row.contains("deepseek-v4-pro"))
            .expect("offerable member row rendered");
        assert!(
            !offered_row.contains("not in current catalog"),
            "{offered_row}"
        );
    }

    /// #6035: a bundled catalog row can outlive the provider's live roster.
    /// A route the fresh roster dropped is flagged in the overview and in the
    /// pin editor, and the pin is never rewritten. (`build_route_rows` asks
    /// `pin_missing_from_fresh_roster`, which carries its own roster tests;
    /// seeding the process-wide catalog here would leak into parallel tests.)
    #[test]
    fn a_pin_the_fresh_roster_dropped_is_flagged_in_overview_and_picker() {
        let ws = tempfile::TempDir::new().unwrap();
        // The operator pins deepseek/deepseek-v4-flash.
        let fleet = sample_fleet("Roster");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Roster",
            FleetScope::Workspace,
        )
        .expect("open");
        let dropped = |row: &RouteRow| {
            row.provider.as_deref() == Some("deepseek")
                && row.model.as_deref() == Some("deepseek-v4-flash")
        };
        match view.routes.iter_mut().find(|row| dropped(row)) {
            Some(row) => row.roster_missing = true,
            None => view.routes.push(RouteRow {
                label: "DeepSeek/deepseek-v4-flash".to_string(),
                summary: String::new(),
                provider: Some("deepseek".to_string()),
                model: Some("deepseek-v4-flash".to_string()),
                roster_missing: true,
            }),
        }
        assert!(view.pin_drifted("deepseek", "deepseek-v4-flash"));

        let render = |view: &FleetDetailView, pick: bool| {
            let area = Rect::new(0, 0, 160, 12);
            let mut buf = Buffer::empty(area);
            if pick {
                view.render_pick_route(area, &mut buf);
            } else {
                view.render_overview(area, &mut buf);
            }
            (0..area.height)
                .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
                .collect::<Vec<String>>()
        };
        let overview = render(&view, false);
        let operator_row = overview
            .iter()
            .find(|row| row.contains("deepseek-v4-flash"))
            .expect("operator row rendered");
        assert!(
            operator_row.contains("not in current catalog"),
            "{operator_row}"
        );

        view.open_route_picker(FleetRouteTarget::Operator);
        view.pick_query = "deepseek-v4-flash".to_string();
        let picker = render(&view, true);
        let row = picker
            .iter()
            .find(|row| row.contains("/deepseek-v4-flash"))
            .expect("dropped route still offered in the picker");
        assert!(row.contains("not in current catalog"), "{row}");
        assert_eq!(
            view.fleet.operator.as_ref().map(|op| op.model.as_str()),
            Some("deepseek-v4-flash"),
            "a warning never rewrites the pin"
        );
    }

    #[test]
    fn save_writes_the_file_and_receipt_names_the_path() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Fleet C");
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet C",
            FleetScope::Workspace,
        )
        .expect("open");

        // Change the operator model, then save.
        view.fleet.operator = Some(FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning: Some("high".to_string()),
        });
        let action = view.handle_key(key(KeyCode::Char('s')));
        let ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message }) = action else {
            panic!("expected FleetStoreChanged, got {action:?}");
        };
        assert!(message.contains("Saved Team `Fleet C`"), "{message}");
        // The receipt names the path as this platform writes it, so build the
        // expected tail the same way instead of hard-coding `/` — on Windows
        // `Path::display` renders the separators as `\`.
        let expected_tail = std::path::Path::new(".codewhale")
            .join("fleets")
            .join("fleet-c.toml")
            .display()
            .to_string();
        assert!(message.contains(&expected_tail), "{message}");

        let (loaded, _) = codewhale_engine::fleet::store::load_fleet_in_scope(
            "Fleet C",
            FleetScope::Workspace,
            ws.path(),
        )
        .expect("reload");
        let op = loaded.operator.expect("operator");
        assert_eq!(op.model, "deepseek-v4-pro");
        assert_eq!(op.reasoning.as_deref(), Some("high"));

        {
            let _env = crate::test_support::lock_test_env();
            let root = tempfile::tempdir().unwrap();
            let _home =
                crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
            let config: Config = toml::from_str(
                r#"
provider = "ReviewerRoute"
[providers.ReviewerRoute]
kind = "openai-compatible"
api_key = "fixture-review-key"
base_url = "http://127.0.0.1:9/v1"
model = "fixture-review-model"
"#,
            )
            .unwrap();
            let mut fleet = FleetFile::new("Editor restart acceptance".into(), None).unwrap();
            fleet.members.push(
                serde_json::from_value(serde_json::json!({
                    "id":"review-pin", "role":"reviewer", "instructions":"SAVED_REVIEW_INSTRUCTION"
                }))
                .unwrap(),
            );
            save_fleet(&fleet, FleetScope::Workspace, root.path()).unwrap();
            let mut app = App::new(
                crate::tui::test_support::test_tui_options(root.path()),
                &config,
            );
            app.workspace = root.path().to_path_buf();
            let mut view = FleetDetailView::open_for_member(
                &app,
                &config,
                &fleet.name,
                FleetScope::Workspace,
                Some("review-pin"),
            )
            .unwrap();
            let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
            view.handle_key(key(KeyCode::Char('e')));
            for ch in "ReviewerRoute".chars() {
                view.handle_key(key(KeyCode::Char(ch)));
            }
            view.handle_key(key(KeyCode::Enter));
            // Off survives generic route normalization and distinguishes the saved
            // choice from default reasoning without inventing fixture capabilities.
            view.handle_key(key(KeyCode::Char('t')));
            assert!(matches!(
                view.handle_key(key(KeyCode::Char('s'))),
                ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { .. })
            ));
            let (saved, _) =
                load_fleet_in_scope(&fleet.name, FleetScope::Workspace, root.path()).unwrap();
            let pin = &saved.members[0];
            assert!(!pin.shortlist);
            assert_eq!(pin.role, "reviewer");
            assert_eq!(pin.provider.as_deref(), Some("ReviewerRoute"));
            assert_eq!(pin.model.as_deref(), Some("fixture-review-model"));
            assert_eq!(pin.reasoning.as_deref(), Some("off"));
        }
    }

    #[test]
    fn roster_picker_spells_roles_like_setup() {
        // The setup wizard's ROLES table owns the "synthesizer" spelling
        // (profile role_hint + file stem contract); the roster add-member
        // picker must offer the same word, not the "summarizer" near-twin
        // (#6087 item 3). Both resolve to Planner posture, so this pins
        // the presented vocabulary, not behavior.
        assert!(KNOWN_ROLES.contains(&"synthesizer"));
        assert!(!KNOWN_ROLES.contains(&"summarizer"));
    }

    #[test]
    fn add_member_uses_role_occupancy_and_preserves_colliding_shortlist() {
        let ws = tempfile::TempDir::new().unwrap();
        let mut fleet = sample_fleet("Fleet D");
        fleet.members.push(
            serde_json::from_value(serde_json::json!({
                "id": "implement", "shortlist": true,
                "provider": "custom-a", "model": "implement",
            }))
            .unwrap(),
        );
        save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();

        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet D",
            FleetScope::Workspace,
        )
        .expect("open");

        view.handle_key(key(KeyCode::Char('a')));
        let ids: Vec<&str> = view.fleet.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["scout", "implement", "implement-role"]);
        assert_eq!(view.fleet.members[2].role, "implement");
        assert!(!view.fleet.members[2].shortlist);
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s'))),
            ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { .. })
        ));
        let (reloaded, _) =
            load_fleet_in_scope("Fleet D", FleetScope::Workspace, ws.path()).unwrap();
        assert_eq!(reloaded.members[..2], fleet.members);
        let roster = codewhale_engine::fleet::identity::roster_from_fleet(
            &reloaded,
            FleetScope::Workspace,
            PathBuf::from("fleet-d.toml").as_path(),
        );
        assert_eq!(roster.members().len(), 2);
        assert!(roster.get("implement").is_none());
        assert_eq!(
            roster.get("implement-role").unwrap().profile.role.name,
            "implement"
        );

        // Remove the new member with the confirmed delete flow.
        view.selected = 3;
        view.handle_key(key(KeyCode::Char('d')));
        view.handle_key(key(KeyCode::Char('y')));
        assert_eq!(view.fleet.members, fleet.members);
    }

    /// U09-01: an ordinary Save or rename never overwrites a Fleet file that
    /// changed on disk after the editor opened it; the newer version survives
    /// and the editor says to reload.
    #[test]
    fn save_and_rename_refuse_an_externally_newer_source() {
        let ws = tempfile::TempDir::new().unwrap();
        let fleet = sample_fleet("Fleet S");
        let path = save_fleet(&fleet, FleetScope::Workspace, ws.path()).unwrap();
        let mut view = FleetDetailView::open(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
            "Fleet S",
            FleetScope::Workspace,
        )
        .expect("open");

        // Another session saves a newer version of the same team.
        let mut newer = fleet.clone();
        newer.operator = Some(FleetOperator {
            provider: "openai".to_string(),
            model: "gpt-5.6".to_string(),
            reasoning: None,
        });
        save_fleet(&newer, FleetScope::Workspace, ws.path()).unwrap();
        let on_disk = std::fs::read(&path).unwrap();

        view.fleet.operator = Some(FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning: None,
        });
        let action = view.handle_key(key(KeyCode::Char('s')));
        assert!(
            matches!(&action, ViewAction::Emit(ViewEvent::OpenTextPager { title, .. }) if title == "Save refused"),
            "{action:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), on_disk);

        view.handle_key(key(KeyCode::Char('r')));
        for _ in "Fleet S".chars() {
            view.handle_key(key(KeyCode::Backspace));
        }
        for ch in "Fleet T".chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        let action = view.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&action, ViewAction::Emit(ViewEvent::OpenTextPager { title, .. }) if title == "Rename refused"),
            "{action:?}"
        );
        assert_eq!(view.fleet.name, "Fleet S");
        assert_eq!(std::fs::read(&path).unwrap(), on_disk);
    }
}

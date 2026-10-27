//! `/fleet fleets` — named saved-Fleet picker (secondary surface; `/fleet fleets`
//! remains a compatibility alias).
//!
//! Bare `/fleet` opens the roster/setup face for the selected Fleet. This view
//! is only for switching between named configurations. One row per saved Fleet
//! across both scopes: user-global (`$CODEWHALE_HOME/fleets/`) and folder
//! (`.codewhale/fleets/`). Rows show name, scope badge, and operator summary —
//! not filesystem paths (paths belong in receipts). Same-name Fleets in both
//! scopes are two rows, never a silent shadow. Legacy per-role profiles get
//! one migration banner, not a pile of shadow badges.
//!
//! The view reads and writes the Fleet store directly (local, atomic file
//! operations); it never touches the live session route.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use crate::fleet::store::{
    FleetEntry, FleetScope, SelectedFleet, delete_fleet, list_fleets, migrate_legacy_roster,
    selected_fleet, set_selected,
};
use crate::tui::app::App;
use crate::tui::menu_style;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;

/// Rows one PageUp/PageDown travels. Pages clamp at the ends per the shared
/// vocabulary instead of wrapping (#6290).
const FLEET_LIST_PAGE: usize = 10;

/// What the host should do after this view acted on the store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // OpenDetail/None are reserved for the qualified-name flow
pub enum FleetListOutcome {
    /// A store mutation happened; show `message`, refresh, and pop the stack.
    Done { message: String },
    /// Open the detail view for the given Fleet.
    OpenDetail { name: String, scope: FleetScope },
    /// Nothing to do.
    None,
}

pub struct FleetListView {
    entries: Vec<FleetEntry>,
    /// The active selection snapshot (workspace then personal).
    selected: Option<SelectedFleet>,
    /// Legacy profile count (how many per-role files exist), for the banner.
    legacy_profile_count: usize,
    /// Whether a v2 Fleet named "Default" already exists (so migration is
    /// offered only when it would not clobber).
    default_fleet_exists: bool,
    row: usize,
    /// Saved scope of the row being acted on (delete/select flow through
    /// confirmation state).
    pending_delete: Option<usize>,
    /// Exact painted cells of each entry row. Clicks and hover hit-test
    /// these painted rects — never a hardcoded row offset — so pointer
    /// targets stay truthful when the list scrolls or the footer wraps.
    row_hitboxes: RefCell<Vec<(Rect, usize)>>,
    /// Entry under the pointer, tinted with the shared hover style. Hover
    /// never moves the keyboard row.
    hovered_row: Cell<Option<usize>>,
    fleet_config: codewhale_config::FleetConfigToml,
    workspace: PathBuf,
    /// UI locale, for the one hint whose wording changes with the entry path.
    locale: Locale,
    /// True when the Fleet roster is parked directly underneath on the view
    /// stack (#5954), i.e. this list was opened with `f` from the roster.
    /// `Esc` pops one view either way — the flag only decides whether the
    /// footer promises `back` or `close`. Direct entry (`/fleet list`)
    /// leaves it false, so `Esc` closes as before.
    back_to_fleet_roster: bool,
}

impl FleetListView {
    #[must_use]
    pub fn new(app: &App, config: &Config) -> Self {
        let workspace = app.workspace.clone();
        let entries = list_fleets(&workspace);
        let selected = selected_fleet(&workspace);
        let legacy_profile_count = legacy_profile_file_count(&workspace);
        let default_fleet_exists = entries
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case("default") && !e.legacy);
        Self {
            entries,
            selected,
            legacy_profile_count,
            default_fleet_exists,
            row: 0,
            pending_delete: None,
            row_hitboxes: RefCell::new(Vec::new()),
            hovered_row: Cell::new(None),
            fleet_config: config.fleet_config(),
            workspace,
            locale: app.ui_locale,
            back_to_fleet_roster: false,
        }
    }

    /// Mark this list as pushed on top of the Fleet roster (#5954), so the
    /// footer says `back` rather than `close`.
    #[must_use]
    pub fn over_fleet_roster(mut self) -> Self {
        self.back_to_fleet_roster = true;
        self
    }

    fn selected_entry(&self) -> Option<&FleetEntry> {
        self.entries.get(self.row)
    }

    fn banner_visible(&self) -> bool {
        self.legacy_profile_count > 0 && !self.default_fleet_exists
    }

    fn move_row(&mut self, delta: isize) {
        let rows = self.entries.len();
        if rows == 0 {
            return;
        }
        self.row = crate::tui::list_nav::wrap_index(self.row, rows, delta);
        self.hovered_row.set(None);
    }

    /// Apply one [`list_nav`](crate::tui::list_nav) motion (#6290), returning
    /// whether it was consumed. Steps wrap; pages travel [`FLEET_LIST_PAGE`]
    /// rows and clamp. The list is single-column, so the region axis is a
    /// no-op.
    fn apply_motion(&mut self, motion: crate::tui::list_nav::Motion) -> bool {
        let len = self.entries.len();
        if len == 0 {
            return false;
        }
        let Some(next) = crate::tui::list_nav::apply(self.row, len, FLEET_LIST_PAGE, motion) else {
            return false;
        };
        self.row = next;
        self.hovered_row.set(None);
        true
    }

    fn hit_row(&self, mouse: MouseEvent) -> Option<usize> {
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        self.row_hitboxes
            .borrow()
            .iter()
            .find_map(|(rect, idx)| rect.contains(position).then_some(*idx))
    }

    fn footer_hints(&self) -> Vec<ActionHint> {
        let mut hints = vec![
            ActionHint::new("↑/↓", "move"),
            ActionHint::new("Enter", "select"),
        ];
        if !self.entries.is_empty() {
            // Explicit scope overrides; Enter selects in the row's own scope.
            hints.push(ActionHint::new("u", "as user default"));
            hints.push(ActionHint::new("w", "for this folder"));
            hints.push(ActionHint::new("d", "delete"));
        }
        if self.banner_visible() {
            hints.push(ActionHint::new("m", "migrate"));
        }
        // The hint has to name what `Esc` actually does: pop back to the
        // parked roster, or close the window at the root (#5954).
        hints.push(if self.back_to_fleet_roster {
            ActionHint::new("Esc", tr(self.locale, MessageId::SetupActionBack))
        } else {
            ActionHint::new("Esc", "close")
        });
        hints
    }

    /// Select the highlighted Fleet in `scope` and close with a receipt that
    /// names the exact file written. Editing stays on `/fleet setup` / roster —
    /// this surface is a switcher, not a file manager.
    fn select_highlighted(&self, scope: FleetScope) -> Option<FleetListOutcome> {
        let entry = self.selected_entry()?;
        if entry.legacy {
            return None;
        }
        match set_selected(&entry.name, scope, &self.workspace) {
            Ok(path) => Some(FleetListOutcome::Done {
                message: format!(
                    "Selected Team `{}` ({}) — wrote {}",
                    entry.name,
                    scope.long_label(),
                    path.display()
                ),
            }),
            Err(err) => Some(FleetListOutcome::Done {
                message: format!("Selection failed: {err}"),
            }),
        }
    }

    fn confirm_delete(&mut self) -> Option<FleetListOutcome> {
        let idx = self.pending_delete?;
        let entry = self.entries.get(idx)?;
        let name = entry.name.clone();
        let scope = entry.scope;
        match delete_fleet(&name, scope, &self.workspace) {
            Ok(path) => Some(FleetListOutcome::Done {
                message: format!(
                    "Deleted Team `{name}` ({}) — removed {}",
                    scope.label(),
                    path.display()
                ),
            }),
            Err(err) => Some(FleetListOutcome::Done {
                message: format!("Delete failed: {err}"),
            }),
        }
    }
}

impl ModalView for FleetListView {
    fn kind(&self) -> ModalKind {
        ModalKind::FleetList
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if self.pending_delete.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let outcome = self.confirm_delete();
                    self.pending_delete = None;
                    return outcome_to_action(outcome);
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.pending_delete = None;
                    return ViewAction::None;
                }
                _ => return ViewAction::None,
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ViewAction::Close,
            // Movement keys come from the shared vocabulary (#6290), j/k
            // aliases included — this surface captures no text. Pages are new
            // here; they used to do nothing.
            _ if crate::tui::list_nav::motion(&key).is_some_and(|m| self.apply_motion(m)) => {
                ViewAction::None
            }
            KeyCode::Enter => {
                let Some(entry) = self.selected_entry() else {
                    return ViewAction::None;
                };
                if entry.legacy {
                    return ViewAction::Emit(ViewEvent::OpenTextPager {
                        title: format!("Team `{}` — legacy format", entry.name),
                        content: format!(
                            "This team file predates the named-team format ({}).\n\n\
                             It is listed so nothing you saved disappears, but it is \
                             read-only here. To edit it, create a new team and copy \
                             the settings you want; legacy files are never migrated \
                             silently.\n\nParse error: {}",
                            entry.path.display(),
                            entry.parse_error.as_deref().unwrap_or("unknown")
                        ),
                    });
                }
                // Enter selects in the row's own scope and returns to the
                // roster/setup face. The list/detail editor is not the product.
                outcome_to_action(self.select_highlighted(entry.scope))
            }
            KeyCode::Char('u') => outcome_to_action(self.select_highlighted(FleetScope::Personal)),
            KeyCode::Char('w') => outcome_to_action(self.select_highlighted(FleetScope::Workspace)),
            KeyCode::Char('d') => {
                let Some(entry) = self.selected_entry() else {
                    return ViewAction::None;
                };
                if entry.legacy {
                    return ViewAction::None;
                }
                self.pending_delete = Some(self.row);
                ViewAction::None
            }
            KeyCode::Char('m') if self.banner_visible() => {
                match migrate_legacy_roster(
                    &self.fleet_config,
                    &self.workspace,
                    true,
                    FleetScope::Personal,
                ) {
                    Ok(receipt) => {
                        let mut content = format!(
                            "Migrated {} legacy role profiles into Team `Default` \
                             (user-global) — wrote {}\n\n",
                            receipt.rows.len(),
                            receipt.saved_to.display()
                        );
                        for row in &receipt.rows {
                            let pin = match &row.pin {
                                Some((model, provider)) => {
                                    format!("{provider}/{model}")
                                }
                                None => "same model as this session".to_string(),
                            };
                            content.push_str(&format!("- {} → {pin}", row.id));
                            if let Some(shadow) = &row.conflicting_shadow {
                                content.push_str(&format!(" [conflict: {shadow}]"));
                            }
                            content.push('\n');
                        }
                        content.push_str(
                            "\nLegacy profile files were left untouched — they are no \
                             longer live configuration once a team is selected.",
                        );
                        if let Ok(path) =
                            set_selected("Default", FleetScope::Personal, &self.workspace)
                        {
                            content.push_str(&format!(
                                "\n\nTeam `Default` is now your user-global default — wrote {}.",
                                path.display()
                            ));
                        }
                        ViewAction::Emit(ViewEvent::OpenTextPager {
                            title: "Legacy migration receipt".to_string(),
                            content,
                        })
                    }
                    Err(err) => ViewAction::Emit(ViewEvent::OpenTextPager {
                        title: "Legacy migration failed".to_string(),
                        content: format!("{err:#}"),
                    }),
                }
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                self.hovered_row.set(self.hit_row(mouse));
                ViewAction::None
            }
            // The wheel moves this list, not the transcript behind it.
            MouseEventKind::ScrollUp => {
                self.move_row(-1);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.move_row(1);
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.pending_delete.is_some() {
                    self.pending_delete = None;
                    return ViewAction::None;
                }
                if let Some(idx) = self.hit_row(mouse) {
                    self.row = idx;
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
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

        // Header: name + selected summary.
        let selected_line = match &self.selected {
            Some(sel) => format!("Selected: `{}` ({})", sel.name, sel.scope.label()),
            None => "No team selected — built-in team".to_string(),
        };
        let mut header = vec![
            Line::from(vec![
                Span::styled(
                    "─ Saved Teams ",
                    Style::default().fg(palette::WHALE_ACTION).bold(),
                ),
                Span::styled(
                    "· pick which named team the session uses",
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {selected_line}"),
                Style::default().fg(palette::TEXT_SECONDARY),
            )),
        ];
        if self.banner_visible() {
            header.push(Line::from(vec![Span::styled(
                format!(
                    "  ⚠ {} legacy role profile(s) found — press m to migrate them into a \
                         Fleet (nothing is changed until you do)",
                    self.legacy_profile_count
                ),
                Style::default().fg(palette::WHALE_HUMAN),
            )]));
        }
        Paragraph::new(header)
            .wrap(Wrap { trim: false })
            .render(chunks[0], buf);

        self.render_rows(chunks[1], buf);
    }
}

impl FleetListView {
    fn render_rows(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.entries.is_empty() {
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  No saved teams yet.",
                    Style::default().fg(palette::TEXT_MUTED),
                ),
                Span::styled(
                    "  Select a model with /model and /provider, then /fleet save or \
                     /fleet save-as. Editing stays on /fleet setup.",
                    Style::default().fg(palette::TEXT_DIM),
                ),
            ]))
            .render(area, buf);
            return;
        }

        // Entries paint two rows (name + summary), one while a delete is
        // armed. Scroll and fit by painted rows, not entry count, so the
        // selected entry is always fully on screen.
        let height = usize::from(area.height).max(1);
        let entry_height = |idx: usize| {
            if self.pending_delete == Some(idx) {
                1
            } else {
                2
            }
        };
        let row = self.row.min(self.entries.len() - 1);
        let mut scroll = row;
        let mut used = entry_height(row);
        while scroll > 0 && used + entry_height(scroll - 1) <= height {
            scroll -= 1;
            used += entry_height(scroll);
        }

        let mut lines = Vec::new();
        let mut hitboxes = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate().skip(scroll) {
            if !lines.is_empty() && lines.len() + entry_height(idx) > height {
                break;
            }
            let selected = idx == self.row;
            let is_selected_fleet = self
                .selected
                .as_ref()
                .is_some_and(|sel| sel.name == entry.name && sel.scope == entry.scope);

            let mut spans = Vec::new();
            let marker = if is_selected_fleet { "▸ " } else { "  " };
            let base = if selected {
                Style::default().fg(palette::WHALE_ACTION).bold()
            } else {
                Style::default().fg(palette::TEXT_SECONDARY)
            };
            spans.push(Span::styled(marker, base));
            let name = if selected {
                "» ".to_string()
            } else {
                "  ".to_string()
            };
            spans.push(Span::styled(format!("{name}{}", entry.name), base));

            let scope_badge = match entry.scope {
                FleetScope::Personal => " [user]",
                FleetScope::Workspace => " [folder]",
            };
            spans.push(Span::styled(
                scope_badge,
                Style::default().fg(palette::TEXT_MUTED),
            ));

            if entry.legacy {
                spans.push(Span::styled(
                    "  legacy format — read-only",
                    Style::default().fg(palette::WHALE_HUMAN),
                ));
            } else if let Some(err) = &entry.parse_error {
                spans.push(Span::styled(
                    format!("  unreadable: {err}"),
                    Style::default().fg(palette::WHALE_HUMAN),
                ));
            }

            if is_selected_fleet && !selected {
                spans.push(Span::styled(
                    "  ★",
                    Style::default().fg(palette::WHALE_ACTION),
                ));
            }

            let start = lines.len();
            if self.pending_delete == Some(idx) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  Delete `{}` ({})? y/n", entry.name, entry.scope.label()),
                    Style::default().fg(palette::WHALE_HUMAN),
                )]));
            } else {
                lines.push(Line::from(spans));
                // Second line: source + operator summary (compact).
                let summary = self.entry_summary(entry);
                lines.push(Line::from(Span::styled(
                    format!("    {summary}"),
                    Style::default().fg(palette::TEXT_DIM),
                )));
            }
            // Hover tints the painted entry but never steals the keyboard
            // row; the spans keep their ink, so the band cannot recolor
            // scope badges or warning text.
            if idx != self.row && self.hovered_row.get() == Some(idx) {
                for line in &mut lines[start..] {
                    line.style = line.style.patch(menu_style::hovered_row_style());
                }
            }
            hitboxes.push((
                Rect {
                    x: area.x,
                    y: area.y.saturating_add(start as u16),
                    width: area.width,
                    height: (lines.len().saturating_sub(start) as u16).max(1),
                },
                idx,
            ));
        }
        *self.row_hitboxes.borrow_mut() = hitboxes;

        let text = ratatui::text::Text::from(lines);
        Paragraph::new(text).render(area, buf);
    }

    fn entry_summary(&self, entry: &FleetEntry) -> String {
        if entry.legacy {
            return "legacy format — open for details".to_string();
        }
        // Read the file to summarize operator + members. Parsing errors are
        // already shown on the row. Paths stay out of the primary row — they
        // belong in receipts after an explicit write/select/delete.
        let Ok(text) = std::fs::read_to_string(&entry.path) else {
            return "unreadable".to_string();
        };
        match crate::fleet::store::FleetFile::parse(&text) {
            Ok(fleet) => {
                let coordinator = match &fleet.operator {
                    Some(op) => format!("{}/{}", op.provider, op.model),
                    None => "uses the session's model".to_string(),
                };
                format!(
                    "Coordinator: {coordinator} · members: {}",
                    fleet
                        .members
                        .iter()
                        .filter(|member| !member.shortlist)
                        .count()
                )
            }
            Err(err) => format!("unreadable: {err}"),
        }
    }
}

fn outcome_to_action(outcome: Option<FleetListOutcome>) -> ViewAction {
    match outcome {
        Some(FleetListOutcome::Done { message }) => {
            ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message })
        }
        Some(FleetListOutcome::OpenDetail { name, scope }) => {
            ViewAction::Emit(ViewEvent::FleetListOpenDetailRequested { name, scope })
        }
        Some(FleetListOutcome::None) | None => ViewAction::None,
    }
}

/// How many legacy per-role profile files exist across both scopes.
fn legacy_profile_file_count(workspace: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(dir) = crate::fleet::profile::personal_agent_profile_dir()
        && let Ok(read) = std::fs::read_dir(dir)
    {
        count += read
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .count();
    }
    let ws_dir = workspace.join(".codewhale").join("agents");
    if let Ok(read) = std::fs::read_dir(ws_dir) {
        count += read
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .count();
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::store::{FleetFile, save_fleet};
    use crate::tui::app::{App, TuiOptions};
    use codewhale_runtime::config::Config;
    use std::sync::OnceLock;

    fn sealed_home() -> &'static std::path::Path {
        static HOME: OnceLock<PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let dir = tempfile::TempDir::new().expect("temp dir").keep();
            std::fs::create_dir_all(dir.join("fleets")).expect("fleets dir");
            dir
        })
    }

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
        fleet.operator = Some(crate::fleet::store::FleetOperator {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            reasoning: None,
        });
        fleet
    }

    fn save_in(workspace: &std::path::Path, scope: FleetScope, name: &str) {
        let fleet = sample_fleet(name);
        let _ = save_fleet(&fleet, scope, workspace).expect("save");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    /// #5954: roster → `f` → saved teams → `Esc` returns to the roster, and
    /// the footer promises `back` rather than `close` while it will.
    #[test]
    fn saved_teams_opened_from_the_roster_returns_there_on_esc() {
        let ws = tempfile::TempDir::new().expect("ws");
        save_in(ws.path(), FleetScope::Workspace, "alpha");
        let app = app_in(ws.path().to_path_buf());
        let config = Config::default();

        let mut stack = crate::tui::views::ViewStack::new();
        stack.push(crate::tui::views::fleet_roster::FleetRosterView::new(
            &app, &config,
        ));
        let events = stack.handle_key(key(KeyCode::Char('f')));
        assert!(
            matches!(
                events.as_slice(),
                [ViewEvent::FleetRosterOpenFleetsRequested]
            ),
            "{events:?}"
        );
        assert_eq!(
            stack.top_kind(),
            Some(ModalKind::FleetRoster),
            "the roster must survive the forward jump"
        );

        // What ui/handlers.rs does with that event.
        stack.push(FleetListView::new(&app, &config).over_fleet_roster());
        assert_eq!(stack.top_kind(), Some(ModalKind::FleetList));

        stack.handle_key(key(KeyCode::Esc));
        assert_eq!(
            stack.top_kind(),
            Some(ModalKind::FleetRoster),
            "Esc in saved teams must return to the roster, not close the window"
        );
    }

    /// #5954: `/fleet list` is the root of its own stack, so `Esc` closes.
    #[test]
    fn saved_teams_opened_directly_closes_on_esc() {
        let ws = tempfile::TempDir::new().expect("ws");
        let mut stack = crate::tui::views::ViewStack::new();
        stack.push(FleetListView::new(
            &app_in(ws.path().to_path_buf()),
            &Config::default(),
        ));
        stack.handle_key(key(KeyCode::Esc));
        assert!(stack.is_empty(), "direct entry must close on Esc");
    }

    /// #5954: the hint names what `Esc` actually does in each entry path.
    #[test]
    fn saved_teams_esc_hint_matches_the_entry_path() {
        let ws = tempfile::TempDir::new().expect("ws");
        save_in(ws.path(), FleetScope::Workspace, "alpha");
        let app = app_in(ws.path().to_path_buf());
        let config = Config::default();

        // `footer_hints` is the only source the footer paints from, so
        // asserting it is asserting what the user reads.
        let esc_label = |view: &FleetListView| {
            view.footer_hints()
                .into_iter()
                .find(|hint| hint.key == "Esc")
                .map(|hint| hint.label.into_owned())
                .expect("Esc hint")
        };

        assert_eq!(
            esc_label(&FleetListView::new(&app, &config)),
            "close",
            "direct entry must still promise close"
        );
        assert_eq!(
            esc_label(&FleetListView::new(&app, &config).over_fleet_roster()),
            "back",
            "over the roster the hint must promise back"
        );
    }

    #[test]
    fn list_renders_both_scopes_and_marks_selection() {
        let _lock = crate::test_support::lock_test_env();
        let prev = std::env::var_os("CODEWHALE_HOME");
        // SAFETY: serialised by lock_test_env.
        unsafe { std::env::set_var("CODEWHALE_HOME", sealed_home()) };
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Personal, "DeepSeek Flash");
        save_in(ws.path(), FleetScope::Workspace, "OpenAI Codex");
        set_selected("OpenAI Codex", FleetScope::Workspace, ws.path()).unwrap();

        let mut app = app_in(ws.path().to_path_buf());
        let view = FleetListView::new(&app, &Config::default());

        // Both scopes listed under their display names; the workspace one
        // selected. The shared sealed home may hold other fleets, so assert
        // by name+scope, never by total count.
        assert!(
            view.entries
                .iter()
                .any(|e| e.name == "DeepSeek Flash" && e.scope == FleetScope::Personal),
            "{:?}",
            view.entries
        );
        assert!(
            view.entries
                .iter()
                .any(|e| e.name == "OpenAI Codex" && e.scope == FleetScope::Workspace),
            "{:?}",
            view.entries
        );
        let selected = view.selected.as_ref().unwrap();
        assert_eq!(selected.name, "OpenAI Codex");
        assert_eq!(selected.scope, FleetScope::Workspace);

        // Restore env.
        // SAFETY: serialised by lock_test_env.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEWHALE_HOME", v),
                None => std::env::remove_var("CODEWHALE_HOME"),
            }
        }
        let _ = &mut app;
    }

    #[test]
    fn select_user_global_writes_receipt_naming_the_file() {
        let _lock = crate::test_support::lock_test_env();
        let prev = std::env::var_os("CODEWHALE_HOME");
        // SAFETY: serialised by lock_test_env.
        unsafe { std::env::set_var("CODEWHALE_HOME", sealed_home()) };
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Personal, "DeepSeek Flash");
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());

        let action = view.handle_key(key(KeyCode::Char('u')));
        let ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message }) = action else {
            panic!("expected FleetStoreChanged, got {action:?}");
        };
        assert!(message.contains("DeepSeek Flash"), "{message}");
        assert!(message.contains("user-global"), "{message}");
        assert!(
            message.to_ascii_lowercase().contains("selected"),
            "{message}"
        );

        // The selection really persisted, at the personal scope. The
        // SelectedFleet path is the fleet file the marker names.
        let sel = crate::fleet::store::selected_fleet(ws.path()).expect("selection");
        assert_eq!(sel.name, "DeepSeek Flash");
        assert_eq!(sel.scope, FleetScope::Personal);
        assert!(
            sel.path.ends_with("fleets/deepseek-flash.toml"),
            "{:?}",
            sel.path
        );

        // SAFETY: serialised by lock_test_env.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEWHALE_HOME", v),
                None => std::env::remove_var("CODEWHALE_HOME"),
            }
        }
    }

    #[test]
    fn delete_requires_confirmation_and_removes_the_file() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Workspace, "Temp Fleet");
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].name, "Temp Fleet");

        // 'd' arms the confirmation; the file is still there.
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('d'))),
            ViewAction::None
        ));
        assert!(
            list_fleets(ws.path())
                .iter()
                .any(|e| e.name == "Temp Fleet")
        );

        // 'n' cancels.
        view.handle_key(key(KeyCode::Char('n')));
        assert!(
            list_fleets(ws.path())
                .iter()
                .any(|e| e.name == "Temp Fleet")
        );

        // 'd' then 'y' deletes and emits a receipt naming the removed path.
        view.handle_key(key(KeyCode::Char('d')));
        let action = view.handle_key(key(KeyCode::Char('y')));
        let ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message }) = action else {
            panic!("expected FleetStoreChanged, got {action:?}");
        };
        assert!(message.contains("Deleted Team `Temp Fleet`"), "{message}");
        assert!(list_fleets(ws.path()).is_empty());
    }

    #[test]
    fn enter_selects_in_the_row_scope_without_opening_detail() {
        let _lock = crate::test_support::lock_test_env();
        let prev = std::env::var_os("CODEWHALE_HOME");
        // SAFETY: serialised by lock_test_env.
        unsafe { std::env::set_var("CODEWHALE_HOME", sealed_home()) };
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Workspace, "Folder Fleet");
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        let idx = view
            .entries
            .iter()
            .position(|e| e.name == "Folder Fleet")
            .expect("workspace fleet listed");
        view.row = idx;

        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::EmitAndClose(ViewEvent::FleetStoreChanged { message }) = action else {
            panic!("Enter must select and close, not open detail: {action:?}");
        };
        assert!(message.contains("Folder Fleet"), "{message}");
        assert!(message.contains("folder"), "{message}");
        let sel = crate::fleet::store::selected_fleet(ws.path()).expect("selection");
        assert_eq!(sel.name, "Folder Fleet");
        assert_eq!(sel.scope, FleetScope::Workspace);

        // SAFETY: serialised by lock_test_env.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEWHALE_HOME", v),
                None => std::env::remove_var("CODEWHALE_HOME"),
            }
        }
    }

    #[test]
    fn legacy_entry_opens_a_pager_instead_of_editing() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::TempDir::new().expect("personal home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::TempDir::new().unwrap();

        // A legacy exact fleet file (workflow schema) in the personal dir.
        std::fs::create_dir_all(home.path().join("fleets")).unwrap();
        let legacy_path = home.path().join("fleets/stopship.toml");
        let legacy_source = r#"schema = "exact"
schema_revision = 1
name = "stopship"
members = []"#;
        std::fs::write(&legacy_path, legacy_source).unwrap();

        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        let idx = view
            .entries
            .iter()
            .position(|e| e.name == "stopship")
            .expect("legacy entry listed");
        view.row = idx;
        let entry = view.selected_entry().expect("legacy entry listed");
        assert!(entry.legacy);
        assert_eq!(entry.scope, FleetScope::Personal);
        assert_eq!(entry.path, legacy_path);

        let action = view.handle_key(key(KeyCode::Enter));
        let ViewAction::Emit(ViewEvent::OpenTextPager { title, content }) = action else {
            panic!("legacy entry must open a read-only pager: {action:?}");
        };
        assert_eq!(title, "Team `stopship` — legacy format");
        assert!(content.contains("This team file predates the named-team format"));
        assert!(content.contains("create a new team"));

        assert_eq!(
            std::fs::read_to_string(&legacy_path).unwrap(),
            legacy_source
        );
        assert!(crate::fleet::store::selected_fleet(ws.path()).is_none());
    }

    #[test]
    fn migration_banner_appears_only_when_needed_and_migrates() {
        let _lock = crate::test_support::lock_test_env();
        let prev = std::env::var_os("CODEWHALE_HOME");
        // SAFETY: serialised by lock_test_env.
        unsafe { std::env::set_var("CODEWHALE_HOME", sealed_home()) };
        let ws = tempfile::TempDir::new().unwrap();

        // No legacy profiles: no banner.
        let view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        assert!(!view.banner_visible());

        // A workspace legacy role profile.
        let agents = ws.path().join(".codewhale/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("scout.toml"),
            r#"id = "scout"
role_hint = "scout"
model = "deepseek-v4-flash"
provider = "deepseek"
"#,
        )
        .unwrap();

        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        assert!(
            view.banner_visible(),
            "banner must show with legacy profiles"
        );

        let action = view.handle_key(key(KeyCode::Char('m')));
        let ViewAction::Emit(ViewEvent::OpenTextPager { title, content }) = action else {
            panic!("migration must open the receipt pager: {action:?}");
        };
        assert_eq!(title, "Legacy migration receipt");
        assert!(content.contains("Team `Default`"));
        assert!(content.contains("once a team is selected"));

        // The Default team now exists and is the user-global selection.
        let entries = list_fleets(ws.path());
        assert!(
            entries.iter().any(|e| e.name == "Default" && !e.legacy),
            "{entries:?}"
        );
        let sel =
            crate::fleet::store::selected_fleet(ws.path()).expect("selection after migration");
        assert_eq!(sel.name, "Default");

        // SAFETY: serialised by lock_test_env.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEWHALE_HOME", v),
                None => std::env::remove_var("CODEWHALE_HOME"),
            }
        }
    }

    #[test]
    fn pointer_click_selects_the_painted_row() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Workspace, "First Fleet");
        save_in(ws.path(), FleetScope::Workspace, "Second Fleet");
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        assert_eq!(view.entries.len(), 2);

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let second = view
            .row_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, idx)| *idx == 1)
            .expect("second entry hitbox")
            .0;
        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: second.x.saturating_add(1),
            row: second.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(view.row, 1, "click must select the painted row");
    }

    #[test]
    fn hover_tints_list_entry_without_moving_row() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::TempDir::new().unwrap();

        save_in(ws.path(), FleetScope::Workspace, "First Fleet");
        save_in(ws.path(), FleetScope::Workspace, "Second Fleet");
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let second = view
            .row_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, idx)| *idx == 1)
            .expect("second entry hitbox")
            .0;
        assert!(matches!(
            view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: second.x.saturating_add(1),
                row: second.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }),
            ViewAction::None
        ));
        assert_eq!(view.hovered_row.get(), Some(1));
        assert_eq!(view.row, 0);

        let mut hovered_buf = Buffer::empty(area);
        view.render(area, &mut hovered_buf);
        assert_eq!(
            hovered_buf[(second.x, second.y)].bg,
            codewhale_palette::SURFACE_ELEVATED,
            "hovered entry must show the shared hover band"
        );
    }

    /// U09-m5: entries paint two rows (name + summary). Scrolling by entry
    /// count pushed the selected bottom entry below the list; every painted
    /// entry, the selected one included, must fit inside the list area.
    #[test]
    fn selected_entry_stays_on_screen_when_entries_are_two_rows() {
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::TempDir::new().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let ws = tempfile::TempDir::new().unwrap();
        for name in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
            save_in(ws.path(), FleetScope::Workspace, name);
        }
        let mut view = FleetListView::new(&app_in(ws.path().to_path_buf()), &Config::default());
        assert_eq!(view.entries.len(), 5);
        let area = Rect::new(0, 0, 80, 4);
        for row in 0..view.entries.len() {
            view.row = row;
            let mut buf = Buffer::empty(area);
            view.render_rows(area, &mut buf);
            let hitboxes = view.row_hitboxes.borrow();
            assert!(
                hitboxes.iter().any(|(_, idx)| *idx == row),
                "row {row} not painted: {hitboxes:?}"
            );
            assert!(
                hitboxes
                    .iter()
                    .all(|(rect, _)| rect.bottom() <= area.bottom()),
                "row {row}: {hitboxes:?}"
            );
        }
    }
}

//! `/automation` — the scheduled-automation room.
//!
//! One list, one detail pane, one key grammar shared with the other rooms
//! (workflow runs, fleet, extensions): ↑↓ move, Enter opens the detail, Esc
//! backs out, Tab flips list ↔ detail. The actions an automation affords —
//! pause / resume, run now, cancel a live run, delete — are single keys and
//! every one of those actions goes through the same `/automation …` and `/task cancel`
//! commands the transcript already accepts, so a keypress here and a typed
//! command leave identical receipts.
//! Create/edit keep a local draft until Save calls the shared manager; Cancel
//! discards that draft. The room then shows the persisted definition and receipt.
//!
//! Automations are user-global (`~/.codewhale/automations`): they follow the
//! person into every repository, which is why the room says where each one
//! runs (`cwds`) rather than assuming this workspace.
//!
//! The view reads the shared [`AutomationManager`] with `try_lock`; a frame
//! that finds it busy keeps the last snapshot rather than blocking the UI.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget, Wrap},
};

use super::{
    ActionHint, CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent,
    render_modal_footer,
};
use crate::tui::app::App;
use codewhale_engine::automation_manager::{
    AutomationRecord, AutomationRunRecord, AutomationRunStatus, AutomationStatus,
    SharedAutomationManager,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::list_nav::wrap_index;

mod editor;
use editor::{AutomationEditor, EditorAction};

/// Recent runs kept per automation in the detail pane.
const RECENT_RUNS: usize = 5;

/// One automation with the runs the detail pane and the cancel key need.
#[derive(Debug, Clone)]
pub(crate) struct AutomationRow {
    pub(crate) record: AutomationRecord,
    /// Newest first.
    pub(crate) runs: Vec<AutomationRunRecord>,
    /// The run history could not be read. Painted instead of "no recorded
    /// runs", which would claim an empty history nobody observed.
    pub(crate) runs_error: Option<String>,
}

impl AutomationRow {
    /// The run in flight, if any — the one `x` cancels.
    fn live_run(&self) -> Option<&AutomationRunRecord> {
        self.runs.iter().find(|run| {
            matches!(
                run.status,
                AutomationRunStatus::Queued | AutomationRunStatus::Running
            )
        })
    }
}

pub struct AutomationsView {
    rows: Vec<AutomationRow>,
    row: usize,
    detail_open: bool,
    detail_scroll: usize,
    /// Largest useful detail scroll in rendered (wrapped) rows, from the last
    /// paint; ↓ stops there instead of piling up invisible scroll.
    detail_max_scroll: Cell<usize>,
    locale: Locale,
    manager: Option<SharedAutomationManager>,
    /// Refreshes ride the modal tick, not every frame.
    last_refresh_at: Instant,
    /// A snapshot that could not be read: the manager is missing or its
    /// store failed. Painted in place of the list.
    problem: Option<String>,
    /// Screen rect of the list body, recorded at render for mouse parity.
    list_body: Cell<Rect>,
    config: codewhale_runtime::config::Config,
    workspace: std::path::PathBuf,
    editor: Option<AutomationEditor>,
    notice: Option<String>,
    new_button: Cell<Rect>,
    edit_button: Cell<Rect>,
}

impl AutomationsView {
    /// Open the room, optionally focused on one automation id.
    #[must_use]
    pub fn new(app: &App, config: &codewhale_runtime::config::Config, focus: Option<&str>) -> Self {
        let mut view = Self {
            rows: Vec::new(),
            row: 0,
            detail_open: false,
            detail_scroll: 0,
            detail_max_scroll: Cell::new(usize::MAX),
            locale: app.ui_locale,
            manager: app.runtime_services.automations.clone(),
            last_refresh_at: Instant::now(),
            problem: None,
            list_body: Cell::new(Rect::ZERO),
            config: config.clone(),
            workspace: app.workspace.clone(),
            editor: None,
            notice: None,
            new_button: Cell::new(Rect::ZERO),
            edit_button: Cell::new(Rect::ZERO),
        };
        view.refresh();
        if let Some(focus) = focus
            && let Some(index) = view.rows.iter().position(|row| row.record.id == focus)
        {
            view.row = index;
            view.detail_open = true;
        }
        view
    }

    #[cfg(test)]
    pub(crate) fn from_rows(rows: Vec<AutomationRow>, locale: Locale) -> Self {
        Self {
            rows,
            row: 0,
            detail_open: false,
            detail_scroll: 0,
            detail_max_scroll: Cell::new(usize::MAX),
            locale,
            manager: None,
            last_refresh_at: Instant::now(),
            problem: None,
            list_body: Cell::new(Rect::ZERO),
            config: codewhale_runtime::config::Config::default(),
            workspace: std::env::temp_dir(),
            editor: None,
            notice: None,
            new_button: Cell::new(Rect::ZERO),
            edit_button: Cell::new(Rect::ZERO),
        }
    }

    fn open_editor(&mut self, edit: bool) {
        let original = if edit {
            let Some(row) = self.selected() else {
                return;
            };
            Some(row.record.clone())
        } else {
            None
        };
        self.notice = None;
        self.editor = Some(AutomationEditor::new(
            &self.config,
            &self.workspace,
            self.locale,
            original,
        ));
    }

    fn editor_action(&mut self, action: EditorAction) -> ViewAction {
        match action {
            EditorAction::Cancel => self.editor = None,
            EditorAction::Save => {
                let result = self
                    .manager
                    .as_ref()
                    .ok_or_else(|| {
                        tr(self.locale, MessageId::AutomationManagerUnavailable).into_owned()
                    })
                    .and_then(|manager| {
                        manager.try_lock().map_err(|_| {
                            tr(self.locale, MessageId::AutomationEditorBusy).into_owned()
                        })
                    })
                    .and_then(|manager| {
                        self.editor
                            .as_ref()
                            .unwrap()
                            .save(&manager)
                            .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(record) => {
                        self.editor = None;
                        self.refresh();
                        if let Some(index) =
                            self.rows.iter().position(|row| row.record.id == record.id)
                        {
                            self.row = index;
                        }
                        self.detail_open = true;
                        self.detail_scroll = 0;
                        self.notice = Some(
                            tr(self.locale, MessageId::AutomationEditorSaved)
                                .replace("{name}", &display_text(&record.name)),
                        );
                    }
                    Err(error) => {
                        self.editor.as_mut().unwrap().problem = Some(
                            tr(self.locale, MessageId::AutomationEditorSaveFailed)
                                .replace("{error}", &error),
                        )
                    }
                }
            }
            EditorAction::None => {}
        }
        ViewAction::None
    }

    /// Re-read definitions and recent runs, keeping the selected id when the
    /// list reorders. A busy manager keeps the previous snapshot.
    fn refresh(&mut self) {
        self.last_refresh_at = Instant::now();
        let Some(manager) = self.manager.as_ref() else {
            self.problem =
                Some(tr(self.locale, MessageId::AutomationManagerUnavailable).into_owned());
            return;
        };
        let Ok(manager) = manager.try_lock() else {
            return;
        };
        let records = match manager.list_automations() {
            Ok(records) => records,
            Err(error) => {
                self.problem = Some(
                    tr(self.locale, MessageId::AutomationListFailed)
                        .replace("{error}", &error.to_string()),
                );
                return;
            }
        };
        let selected_id = self.selected().map(|row| row.record.id.clone());
        self.rows = records
            .into_iter()
            .map(|record| {
                let (runs, runs_error) = match manager.list_runs(&record.id, Some(RECENT_RUNS)) {
                    Ok(runs) => (runs, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                AutomationRow {
                    record,
                    runs,
                    runs_error,
                }
            })
            .collect();
        self.problem = None;
        self.row = selected_id
            .and_then(|id| self.rows.iter().position(|row| row.record.id == id))
            .unwrap_or_else(|| self.row.min(self.rows.len().saturating_sub(1)));
    }

    fn selected(&self) -> Option<&AutomationRow> {
        self.rows.get(self.row)
    }

    pub(crate) fn show_action_receipt(&mut self, receipt: String) {
        self.notice = Some(receipt);
        self.refresh();
    }

    fn move_row(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.row = wrap_index(self.row, self.rows.len(), delta);
        self.detail_scroll = 0;
    }

    /// Every mutation is the typed command, so the receipt in the transcript
    /// is the same one `/automation …` leaves; the next tick re-reads.
    fn command(command: String) -> ViewAction {
        ViewAction::Emit(ViewEvent::CommandPaletteSelected {
            action: CommandPaletteAction::ExecuteCommand { command },
        })
    }

    fn toggle_pause(&self) -> ViewAction {
        let Some(row) = self.selected() else {
            return ViewAction::None;
        };
        let verb = match row.record.status {
            AutomationStatus::Active => "pause",
            AutomationStatus::Paused => "resume",
        };
        Self::command(format!("/automation {verb} {}", row.record.id))
    }

    fn run_now(&self) -> ViewAction {
        match self.selected() {
            Some(row) => Self::command(format!("/automation run {}", row.record.id)),
            None => ViewAction::None,
        }
    }

    /// Cancel the live run: an automation run is a durable task, so the
    /// task's own cancel is the one path.
    fn cancel_live_run(&self) -> ViewAction {
        match self
            .selected()
            .and_then(AutomationRow::live_run)
            .and_then(|run| run.task_id.as_deref())
        {
            Some(task_id) => Self::command(format!("/task cancel {task_id}")),
            None => ViewAction::None,
        }
    }

    /// Delete opens the shared review control with the exact snapshot token.
    fn delete(&self) -> ViewAction {
        match self.selected() {
            Some(row) => Self::command(format!("/automation delete {}", row.record.id)),
            None => ViewAction::None,
        }
    }

    fn footer_hints(&self) -> Vec<ActionHint> {
        let locale = self.locale;
        let mut hints = vec![ActionHint::new("↑↓", tr(locale, MessageId::LaunchHintMove))];
        if self.detail_open {
            hints.push(ActionHint::new(
                "Tab",
                tr(locale, MessageId::AutomationListHeading),
            ));
        } else {
            hints.push(ActionHint::new(
                "Enter",
                tr(locale, MessageId::AutomationActionInspect),
            ));
        }
        if let Some(row) = self.selected() {
            hints.push(ActionHint::new(
                "p",
                match row.record.status {
                    AutomationStatus::Active => tr(locale, MessageId::AutomationActionPause),
                    AutomationStatus::Paused => tr(locale, MessageId::AutomationActionResume),
                },
            ));
            hints.push(ActionHint::new(
                "r",
                tr(locale, MessageId::AutomationActionRun),
            ));
            if row.live_run().is_some() {
                hints.push(ActionHint::new(
                    "x",
                    tr(locale, MessageId::AutomationActionCancel),
                ));
            }
            hints.push(ActionHint::new(
                "d",
                tr(locale, MessageId::AutomationActionDelete),
            ));
        }
        hints.push(ActionHint::new(
            "Esc",
            tr(locale, MessageId::SessionsActionClose),
        ));
        hints
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        let active = self
            .rows
            .iter()
            .filter(|row| row.record.status == AutomationStatus::Active)
            .count();
        let live = self
            .rows
            .iter()
            .filter(|row| row.live_run().is_some())
            .count();
        vec![
            Line::from(vec![
                Span::styled(
                    format!("─ {} ", tr(self.locale, MessageId::AutomationListHeading)),
                    Style::default().fg(palette::WHALE_ACTION).bold(),
                ),
                Span::styled(
                    format!(
                        "· {active} {} · {live} {}",
                        tr(self.locale, MessageId::AutomationStatusActive),
                        tr(self.locale, MessageId::AutomationRunStatusRunning)
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]),
            Line::from(Span::styled(
                format!("  {}", tr(self.locale, MessageId::AutomationScopeNote)),
                Style::default().fg(palette::TEXT_DIM),
            )),
            Line::from(self.notice.clone().unwrap_or_default()),
        ]
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        self.list_body.set(area);
        if let Some(problem) = self.problem.as_deref() {
            Paragraph::new(Line::from(Span::styled(
                format!("  {problem}"),
                Style::default().fg(palette::STATUS_ERROR),
            )))
            .wrap(Wrap { trim: false })
            .render(area, buf);
            return;
        }
        if self.rows.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", tr(self.locale, MessageId::AutomationEmpty)),
                Style::default().fg(palette::TEXT_MUTED),
            )))
            .wrap(Wrap { trim: false })
            .render(area, buf);
            return;
        }
        let rows_visible = usize::from(area.height).max(1);
        let scroll = self.row.saturating_sub(rows_visible.saturating_sub(1));
        for (idx, row) in self.rows.iter().enumerate().skip(scroll).take(rows_visible) {
            let y = area.y + u16::try_from(idx - scroll).unwrap_or(u16::MAX);
            let selected = idx == self.row;
            let base = if selected {
                Style::default().fg(palette::WHALE_ACTION).bold()
            } else {
                Style::default().fg(palette::TEXT_SECONDARY)
            };
            let (mark, mark_style) = match (row.live_run(), row.record.status) {
                (Some(_), _) => ("●", Style::default().fg(palette::STATUS_WARNING)),
                (None, AutomationStatus::Active) => ("○", Style::default().fg(palette::TEXT_MUTED)),
                (None, AutomationStatus::Paused) => ("‖", Style::default().fg(palette::TEXT_DIM)),
            };
            let state = match (row.live_run(), row.record.status) {
                (Some(_), _) => tr(self.locale, MessageId::AutomationRunStatusRunning),
                (None, AutomationStatus::Active) => {
                    tr(self.locale, MessageId::AutomationStatusActive)
                }
                (None, AutomationStatus::Paused) => {
                    tr(self.locale, MessageId::AutomationStatusPaused)
                }
            };
            let line = Line::from(vec![
                Span::styled(if selected { "▸ " } else { "  " }, base),
                Span::styled(format!("{mark} "), mark_style),
                Span::styled(display_text(&row.record.name), base),
                Span::styled(
                    format!(
                        "  ·  {state}  ·  {}: {}",
                        tr(self.locale, MessageId::AutomationNextLabel),
                        next_run_label(row.record.next_run_at)
                    ),
                    Style::default()
                        .fg(palette::TEXT_DIM)
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]);
            line.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    fn detail_lines(&self, row: &AutomationRow) -> Vec<Line<'static>> {
        let locale = self.locale;
        let label = |id: MessageId| {
            Span::styled(
                format!("  {}  ", tr(locale, id)),
                Style::default().fg(palette::TEXT_PRIMARY).bold(),
            )
        };
        let value = |text: String| Span::styled(text, Style::default().fg(palette::TEXT_SECONDARY));
        let record = &row.record;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("─ ", Style::default().fg(palette::WHALE_ACTION).bold()),
                Span::styled(
                    display_text(&record.name),
                    Style::default().fg(palette::TEXT_PRIMARY).bold(),
                ),
                Span::styled(
                    format!(
                        "  ·  {}",
                        match record.status {
                            AutomationStatus::Active =>
                                tr(locale, MessageId::AutomationStatusActive),
                            AutomationStatus::Paused =>
                                tr(locale, MessageId::AutomationStatusPaused),
                        }
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]),
            Line::from(Span::styled(
                format!("  {}", record.id),
                Style::default().fg(palette::TEXT_DIM),
            )),
            Line::from(""),
            Line::from(vec![
                label(MessageId::AutomationRruleLabel),
                value(record.rrule.clone()),
            ]),
            Line::from(vec![
                label(MessageId::AutomationNextLabel),
                value(next_run_label(record.next_run_at)),
            ]),
        ];
        if !record.cwds.is_empty() {
            lines.push(Line::from(vec![
                label(MessageId::AutomationCwdLabel),
                value(
                    record
                        .cwds
                        .iter()
                        .map(|cwd| cwd.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ]));
        }
        if let Some(model) = record.model.as_deref() {
            lines.push(Line::from(vec![
                label(MessageId::SetupCardModelLabel),
                value(
                    record
                        .model_provider_id
                        .as_ref()
                        .or(record.model_provider.as_ref())
                        .map_or_else(
                            || model.to_string(),
                            |provider| format!("{provider} / {model}"),
                        ),
                ),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", tr(locale, MessageId::AutomationPromptLabel)),
            Style::default().fg(palette::TEXT_PRIMARY).bold(),
        )));
        for line in display_text(&record.prompt).lines().take(12) {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(palette::TEXT_SECONDARY),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", tr(locale, MessageId::AutomationRecentRunsLabel)),
            Style::default().fg(palette::TEXT_PRIMARY).bold(),
        )));
        if let Some(error) = row.runs_error.as_deref() {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {} · {}",
                    tr(locale, MessageId::AutomationRunsUnavailable),
                    display_text(error)
                ),
                Style::default().fg(palette::STATUS_ERROR),
            )));
        } else if row.runs.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", tr(locale, MessageId::AutomationNoRuns)),
                Style::default().fg(palette::TEXT_DIM),
            )));
        }
        for run in &row.runs {
            let (glyph, style) = match run.status {
                AutomationRunStatus::Queued | AutomationRunStatus::Running => {
                    ("●", Style::default().fg(palette::STATUS_WARNING))
                }
                AutomationRunStatus::Completed => {
                    ("✓", Style::default().fg(palette::STATUS_SUCCESS))
                }
                AutomationRunStatus::Failed => ("✗", Style::default().fg(palette::STATUS_ERROR)),
                AutomationRunStatus::Canceled => ("–", Style::default().fg(palette::TEXT_DIM)),
            };
            let mut spans = vec![
                Span::styled(format!("    {glyph} "), style),
                Span::styled(
                    run.scheduled_for.format("%Y-%m-%d %H:%M UTC").to_string(),
                    Style::default().fg(palette::TEXT_SECONDARY),
                ),
            ];
            if let Some(task) = run.task_id.as_deref() {
                spans.push(Span::styled(
                    format!("  ·  {} {task}", tr(locale, MessageId::AutomationTaskLabel)),
                    Style::default().fg(palette::TEXT_DIM),
                ));
            }
            if let Some(error) = run.error.as_deref() {
                spans.push(Span::styled(
                    format!("  ·  {error}"),
                    Style::default().fg(palette::STATUS_ERROR),
                ));
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer) {
        let Some(row) = self.selected() else {
            self.render_list(area, buf);
            return;
        };
        let lines = self.detail_lines(row);
        self.detail_max_scroll.set(super::render_wrapped_detail(
            lines,
            area,
            buf,
            self.detail_scroll,
        ));
    }
}

fn next_run_label(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    value.map_or_else(
        || "-".to_string(),
        |at| at.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

/// Stored names and prompts are untrusted text: one line, no control
/// characters, so a name cannot reflow the room.
fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

impl ModalView for AutomationsView {
    fn kind(&self) -> ModalKind {
        ModalKind::Automations
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        if let Some(editor) = self.editor.as_mut() {
            let action = editor.key(key);
            return self.editor_action(action);
        }
        match key.code {
            KeyCode::Char('n') => {
                self.open_editor(false);
                ViewAction::None
            }
            KeyCode::Char('e') => {
                self.open_editor(true);
                ViewAction::None
            }
            KeyCode::Esc => {
                if self.detail_open {
                    self.detail_open = false;
                    self.detail_scroll = 0;
                    ViewAction::None
                } else {
                    ViewAction::Close
                }
            }
            KeyCode::Char('q') => ViewAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.detail_open {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else {
                    self.move_row(-1);
                }
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.detail_open {
                    self.detail_scroll = self
                        .detail_scroll
                        .saturating_add(1)
                        .min(self.detail_max_scroll.get());
                } else {
                    self.move_row(1);
                }
                ViewAction::None
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if !self.rows.is_empty() {
                    self.detail_open = true;
                }
                ViewAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.detail_open = false;
                self.detail_scroll = 0;
                ViewAction::None
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if !self.rows.is_empty() {
                    self.detail_open = !self.detail_open;
                    self.detail_scroll = 0;
                }
                ViewAction::None
            }
            KeyCode::Char('p') | KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('r') => self.run_now(),
            KeyCode::Char('x') | KeyCode::Char('c') => self.cancel_live_run(),
            KeyCode::Char('d') => self.delete(),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        if let Some(editor) = self.editor.as_mut() {
            let action = editor.mouse(mouse);
            return self.editor_action(action);
        }
        // The wheel moves this list, not the transcript behind it.
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_row(-1);
                return ViewAction::None;
            }
            MouseEventKind::ScrollDown => {
                self.move_row(1);
                return ViewAction::None;
            }
            _ => {}
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let point = (mouse.column, mouse.row).into();
            if self.new_button.get().contains(point) {
                self.open_editor(false);
                return ViewAction::None;
            }
            if self.edit_button.get().contains(point) {
                self.open_editor(true);
                return ViewAction::None;
            }
        }
        if self.detail_open {
            return ViewAction::None;
        }
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let body = self.list_body.get();
            if body.width > 0 && body.contains((mouse.column, mouse.row).into()) {
                let offset = usize::from(mouse.row - body.y);
                let scroll = self
                    .row
                    .saturating_sub(usize::from(body.height).saturating_sub(1));
                let idx = scroll + offset;
                if idx < self.rows.len() {
                    if self.row == idx {
                        self.detail_open = true;
                    }
                    self.row = idx;
                }
            }
        }
        ViewAction::None
    }

    fn handle_paste(&mut self, text: &str) -> bool {
        if let Some(editor) = self.editor.as_mut() {
            editor.paste(text);
            return true;
        }
        false
    }

    fn tick(&mut self) -> ViewAction {
        let interval = if self.rows.iter().any(|row| row.live_run().is_some()) {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(2)
        };
        if self.last_refresh_at.elapsed() >= interval {
            self.refresh();
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        Block::default()
            .style(
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .bg(palette::WHALE_BG),
            )
            .render(area, buf);
        if let Some(editor) = &self.editor {
            editor.render(area, buf);
            return;
        }
        let hints = self.footer_hints();
        let content = render_modal_footer(area, buf, &hints);
        let header = Paragraph::new(self.header_lines()).wrap(Wrap { trim: false });
        let header_height = u16::try_from(header.line_count(content.width))
            .unwrap_or(u16::MAX)
            .saturating_add(1);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_height), Constraint::Min(1)])
            .split(content);
        // Keep action feedback visible while this panel covers the transcript.
        // Reserve wrapped rows before the New/Edit buttons, including for CJK.
        header.render(chunks[0], buf);
        let mut x = chunks[0].x;
        self.new_button.set(Rect::ZERO);
        self.edit_button.set(Rect::ZERO);
        for (key, label, hit) in [
            ("n", MessageId::AutomationEditorNew, &self.new_button),
            ("e", MessageId::AutomationEditorEdit, &self.edit_button),
        ] {
            if key == "e" && self.selected().is_none() {
                continue;
            }
            let text = format!("[{key} {}] ", tr(self.locale, label));
            let width = u16::try_from(codewhale_tui_kit::ui_text::text_display_width(&text))
                .unwrap_or(u16::MAX)
                .min(chunks[0].right().saturating_sub(x));
            let rect = Rect::new(
                x,
                chunks[0].y + chunks[0].height.saturating_sub(1),
                width,
                1,
            );
            Paragraph::new(text)
                .style(Style::default().fg(palette::WHALE_ACTION))
                .render(rect, buf);
            hit.set(rect);
            x += width;
        }
        if self.detail_open {
            self.render_detail(chunks[1], buf);
        } else {
            self.render_list(chunks[1], buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use crossterm::event::KeyModifiers;

    fn record(id: &str, status: AutomationStatus) -> AutomationRecord {
        let at = Utc.with_ymd_and_hms(2026, 9, 4, 17, 0, 0).unwrap();
        AutomationRecord {
            destination_thread_id: None,
            schema_version: 1,
            execution_scope: Some(codewhale_engine::task_manager::test_execution_scope("test")),
            id: id.to_string(),
            name: format!("cwc daily {id}"),
            prompt: "patrol".to_string(),
            rrule: "FREQ=DAILY;BYHOUR=17".to_string(),
            cwds: vec![std::path::PathBuf::from("/tmp/cwc")],
            model: None,
            model_provider: None,
            model_provider_id: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            delivery_mode: None,
            status,
            created_at: at,
            updated_at: at,
            next_run_at: Some(at),
            last_run_at: None,
            revision: 0,
        }
    }

    fn run(automation_id: &str, status: AutomationRunStatus) -> AutomationRunRecord {
        let at = Utc.with_ymd_and_hms(2026, 9, 3, 17, 0, 0).unwrap();
        AutomationRunRecord {
            schema_version: 1,
            id: format!("run-{automation_id}"),
            automation_id: automation_id.to_string(),
            scheduled_for: at,
            status,
            created_at: at,
            started_at: Some(at),
            ended_at: None,
            task_id: Some(format!("task-{automation_id}")),
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        }
    }

    fn view() -> AutomationsView {
        AutomationsView::from_rows(
            vec![
                AutomationRow {
                    record: record("vision", AutomationStatus::Active),
                    runs: vec![run("vision", AutomationRunStatus::Running)],
                    runs_error: None,
                },
                AutomationRow {
                    record: record("infra", AutomationStatus::Paused),
                    runs: Vec::new(),
                    runs_error: None,
                },
            ],
            Locale::En,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn command_of(action: ViewAction) -> String {
        match action {
            ViewAction::Emit(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => command,
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn every_affordance_is_the_typed_command() {
        let mut view = view();
        assert_eq!(
            command_of(view.handle_key(key(KeyCode::Char('p')))),
            "/automation pause vision"
        );
        assert_eq!(
            command_of(view.handle_key(key(KeyCode::Char('r')))),
            "/automation run vision"
        );
        assert_eq!(
            command_of(view.handle_key(key(KeyCode::Char('x')))),
            "/task cancel task-vision"
        );
        assert_eq!(
            command_of(view.handle_key(key(KeyCode::Char('d')))),
            "/automation delete vision"
        );

        view.handle_key(key(KeyCode::Down));
        assert_eq!(
            command_of(view.handle_key(key(KeyCode::Char('p')))),
            "/automation resume infra"
        );
        // Nothing live to cancel on the paused row.
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('x'))),
            ViewAction::None
        ));
    }

    #[test]
    fn tab_flips_list_and_detail_and_esc_backs_out() {
        let mut view = view();
        assert!(!view.detail_open);
        view.handle_key(key(KeyCode::Tab));
        assert!(view.detail_open);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::None
        ));
        assert!(!view.detail_open);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::Close
        ));
    }

    #[test]
    fn editor_save_and_cancel_require_explicit_room_actions() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(tokio::sync::Mutex::new(
            codewhale_engine::automation_manager::AutomationManager::open_for_test(
                root.path().join("store"),
            )
            .unwrap(),
        ));
        let mut view = AutomationsView::from_rows(Vec::new(), Locale::En);
        view.workspace = root.path().to_path_buf();
        view.manager = Some(manager.clone());
        view.handle_key(key(KeyCode::Char('n')));
        assert!(view.handle_paste("draft"));
        view.handle_key(key(KeyCode::Tab));
        view.handle_paste("line one\nline two");
        assert!(
            manager
                .try_lock()
                .unwrap()
                .list_automations()
                .unwrap()
                .is_empty()
        );
        let area = Rect::new(0, 0, 40, 12);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        let cancel = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 17,
            row: 11,
            modifiers: KeyModifiers::NONE,
        };
        view.handle_mouse(cancel);
        assert!(view.editor.is_none());
        assert!(
            manager
                .try_lock()
                .unwrap()
                .list_automations()
                .unwrap()
                .is_empty()
        );

        view.handle_key(key(KeyCode::Char('n')));
        view.handle_paste("saved");
        view.handle_key(key(KeyCode::Tab));
        view.handle_paste("prompt");
        view.render(area, &mut buffer);
        view.handle_mouse(MouseEvent {
            column: 2,
            ..cancel
        });
        assert!(view.editor.is_none());
        assert_eq!(
            manager
                .try_lock()
                .unwrap()
                .list_automations()
                .unwrap()
                .len(),
            1
        );
        view.render(area, &mut buffer);
        let receipt = rendered_text(area, &buffer);
        assert!(
            receipt.contains("Saved saved"),
            "compact receipt: {receipt}"
        );
    }

    fn rendered_text(area: Rect, buffer: &Buffer) -> String {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn refused_action_stays_visible_in_list_and_detail_after_refresh() {
        for (locale, receipt) in [
            (
                Locale::En,
                "Could not run automation: Automation belongs to another Runtime execution scope",
            ),
            (
                Locale::ZhHans,
                "无法运行此自动化：它属于另一会话，请返回原会话后重试。",
            ),
        ] {
            for width in [40, 80, 120] {
                for detail in [false, true] {
                    let mut view = view();
                    view.locale = locale;
                    view.detail_open = detail;
                    view.show_action_receipt(receipt.to_string());
                    view.refresh();
                    let area = Rect::new(0, 0, width, 20);
                    let mut buffer = Buffer::empty(area);
                    view.render(area, &mut buffer);
                    let text = rendered_text(area, &buffer);
                    let compact =
                        |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                    assert!(
                        compact(&text).contains(&compact(receipt)),
                        "{locale:?}/{width}/{detail}: {text}"
                    );
                    assert!(!view.new_button.get().is_empty(), "New remains accessible");
                    assert_eq!(view.rows.len(), 2, "feedback preserves definitions");
                }
            }
        }
    }

    #[test]
    fn renders_state_next_run_and_the_live_cancel_hint() {
        let view = view();
        let area = Rect::new(0, 0, 100, 16);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("cwc daily vision"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("2026-09-04 17:00 UTC"), "{text}");
        assert!(text.contains("x cancel"), "{text}");
        assert!(text.contains("p pause"), "{text}");
        assert!(text.contains("follow you into every repository"), "{text}");
    }

    /// U09-m3: a run history that could not be read is reported as such,
    /// never painted as "no recorded runs".
    #[test]
    fn unreadable_run_history_is_not_shown_as_empty() {
        let view = AutomationsView::from_rows(
            vec![AutomationRow {
                record: record("vision", AutomationStatus::Active),
                runs: Vec::new(),
                runs_error: Some("run store unreadable".to_string()),
            }],
            Locale::En,
        );
        let text = view
            .detail_lines(&view.rows[0])
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("runs unavailable"), "{text}");
        assert!(text.contains("run store unreadable"), "{text}");
        assert!(!text.contains("no recorded runs"), "{text}");
    }

    /// U09-m6: detail scroll counts wrapped rows, so a long prompt's tail is
    /// reachable and ↓ stops at the real end instead of piling up scroll.
    #[test]
    fn detail_scroll_reaches_the_wrapped_tail() {
        let mut long = record("long", AutomationStatus::Active);
        long.prompt = format!("{}TAILMARK", "word ".repeat(80));
        let mut view = AutomationsView::from_rows(
            vec![AutomationRow {
                record: long,
                runs: Vec::new(),
                runs_error: None,
            }],
            Locale::En,
        );
        view.detail_open = true;
        let area = Rect::new(0, 0, 30, 8);
        let mut buf = Buffer::empty(area);
        view.render_detail(area, &mut buf);
        for _ in 0..200 {
            view.handle_key(key(KeyCode::Down));
        }
        assert_eq!(view.detail_scroll, view.detail_max_scroll.get());
        let mut buf = Buffer::empty(area);
        view.render_detail(area, &mut buf);
        let text = rendered_text(area, &buf);
        assert!(text.contains("TAILMARK"), "{text}");
    }
}

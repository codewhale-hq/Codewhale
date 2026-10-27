//! Draft-only automation form. Save is the only path to the canonical manager.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDate, NaiveTime, Timelike, Utc, Weekday};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::automation_manager::{
    AutomationManager, AutomationRecord, AutomationSchedule, AutomationStatus,
    CreateAutomationRequest, UpdateAutomationRequest,
};
use crate::tui::ui_text::{grapheme_display_width, text_display_width};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;

const DAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];
const DAY_CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
const DAY_LABELS: [MessageId; 7] = [
    MessageId::AutomationEditorMonday,
    MessageId::AutomationEditorTuesday,
    MessageId::AutomationEditorWednesday,
    MessageId::AutomationEditorThursday,
    MessageId::AutomationEditorFriday,
    MessageId::AutomationEditorSaturday,
    MessageId::AutomationEditorSunday,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preset {
    Daily,
    Weekly,
    Hourly,
    Once,
    Custom,
}

impl Preset {
    const ALL: [Self; 5] = [
        Self::Daily,
        Self::Weekly,
        Self::Hourly,
        Self::Once,
        Self::Custom,
    ];
    fn label(self) -> MessageId {
        match self {
            Self::Daily => MessageId::AutomationEditorDaily,
            Self::Weekly => MessageId::AutomationEditorWeekly,
            Self::Hourly => MessageId::AutomationEditorHourly,
            Self::Once => MessageId::AutomationEditorOnce,
            Self::Custom => MessageId::AutomationEditorCustom,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Prompt,
    Schedule,
    Time,
    Days,
    Date,
    Rrule,
    Model,
    Workspace,
    Enabled,
    Save,
    Cancel,
}

#[derive(Clone, Copy)]
enum Hit {
    Field(Field),
    Day(usize),
    Time(i32),
    Model(usize),
}

pub(super) enum EditorAction {
    None,
    Cancel,
    Save,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ModelChoice {
    model: Option<String>,
    provider: Option<String>,
    provider_id: Option<String>,
}

impl ModelChoice {
    fn from_record(record: &AutomationRecord) -> Self {
        Self {
            model: record.model.clone(),
            provider: record.model_provider.clone(),
            provider_id: record.model_provider_id.clone(),
        }
    }
}

/// A small grapheme-aware form buffer; the composer owns its own history and
/// tool-input semantics, which must not run while a schedule draft is open.
#[derive(Clone, Debug)]
struct TextField {
    value: String,
    cursor: usize,
    selected: bool,
}

impl TextField {
    fn new(value: String) -> Self {
        Self {
            cursor: value.len(),
            value,
            selected: false,
        }
    }

    fn insert(&mut self, text: &str, multiline: bool) {
        let text: String = text
            .replace("\r\n", "\n")
            .chars()
            .filter(|ch| !ch.is_control() || (multiline && *ch == '\n'))
            .collect();
        let retained = if self.selected { 0 } else { self.value.len() };
        if retained.saturating_add(text.len()) > 64 * 1024 {
            return;
        }
        self.erase_selection();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn erase_selection(&mut self) -> bool {
        if !self.selected {
            return false;
        }
        self.value.clear();
        self.cursor = 0;
        self.selected = false;
        true
    }

    fn key(&mut self, key: KeyEvent, multiline: bool) {
        let control = key.modifiers == KeyModifiers::CONTROL;
        match key.code {
            KeyCode::Char('a') if control => self.selected = true,
            KeyCode::Char('u') if control => {
                self.selected = true;
                self.erase_selection();
            }
            KeyCode::Char(ch)
                if crate::tui::widgets::key_hint::is_altgr(key.modifiers)
                    || !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&ch.to_string(), multiline)
            }
            KeyCode::Enter if multiline => self.insert("\n", true),
            KeyCode::Backspace => {
                if !self.erase_selection() {
                    let previous = self.value[..self.cursor]
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    self.value.replace_range(previous..self.cursor, "");
                    self.cursor = previous;
                }
            }
            KeyCode::Delete => {
                if !self.erase_selection() {
                    let next = self.cursor
                        + self.value[self.cursor..]
                            .graphemes(true)
                            .next()
                            .map_or(0, str::len);
                    self.value.replace_range(self.cursor..next, "");
                }
            }
            KeyCode::Left => {
                self.cursor = self.value[..self.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.selected = false;
            }
            KeyCode::Right => {
                self.cursor += self.value[self.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len);
                self.selected = false;
            }
            KeyCode::Home => {
                self.cursor = if control {
                    0
                } else {
                    self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
                };
                self.selected = false;
            }
            KeyCode::End => {
                self.cursor = if control {
                    self.value.len()
                } else {
                    self.cursor
                        + self.value[self.cursor..]
                            .find('\n')
                            .unwrap_or(self.value.len() - self.cursor)
                };
                self.selected = false;
            }
            KeyCode::Up | KeyCode::Down if multiline => {
                let start = self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = self.value[start..self.cursor].graphemes(true).count();
                let target = if key.code == KeyCode::Up {
                    if start == 0 {
                        return;
                    }
                    let previous = self.value[..start - 1].rfind('\n').map_or(0, |i| i + 1);
                    previous..start - 1
                } else {
                    let Some(end) = self.value[self.cursor..]
                        .find('\n')
                        .map(|i| self.cursor + i + 1)
                    else {
                        return;
                    };
                    end..end
                        + self.value[end..]
                            .find('\n')
                            .unwrap_or(self.value.len() - end)
                };
                self.cursor = target.start
                    + self.value[target.clone()]
                        .graphemes(true)
                        .take(column)
                        .map(str::len)
                        .sum::<usize>();
                self.selected = false;
            }
            _ => {}
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut lines = vec![Line::default()];
        let mut width = 0;
        let mut cursor_line = 0;
        for (offset, grapheme) in self
            .value
            .grapheme_indices(true)
            .chain(std::iter::once((self.value.len(), " ")))
        {
            let caret = focused && offset == self.cursor;
            let shown = if grapheme == "\n" || grapheme.chars().any(char::is_control) {
                " "
            } else {
                grapheme
            };
            let columns = grapheme_display_width(shown);
            if width + columns > usize::from(area.width) {
                lines.push(Line::default());
                width = 0;
            }
            if caret {
                cursor_line = lines.len() - 1;
            }
            let style = if focused && (caret || self.selected) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(shown.to_string(), style));
            width += columns;
            if grapheme == "\n" {
                lines.push(Line::default());
                width = 0;
            }
        }
        let scroll = if focused {
            cursor_line.saturating_sub(usize::from(area.height) - 1)
        } else {
            0
        };
        Paragraph::new(lines.into_iter().skip(scroll).collect::<Vec<_>>()).render(area, buf);
    }
}

pub(super) struct AutomationEditor {
    original: Option<AutomationRecord>,
    locale: Locale,
    workspace_root: PathBuf,
    provider: String,
    name: TextField,
    prompt: TextField,
    time: TextField,
    date: TextField,
    rrule: TextField,
    workspace: TextField,
    preset: Preset,
    days: [bool; 7],
    day_cursor: usize,
    schedule_changed: bool,
    enabled: bool,
    models: Vec<ModelChoice>,
    model: usize,
    picking_model: bool,
    model_query: TextField,
    model_row: usize,
    focus: Field,
    scroll: Cell<usize>,
    hits: RefCell<Vec<(Rect, Hit)>>,
    preview_cache: RefCell<Option<(String, bool, Instant, String)>>,
    pub(super) problem: Option<String>,
}

impl AutomationEditor {
    pub(super) fn new(
        config: &Config,
        workspace: &Path,
        locale: Locale,
        original: Option<AutomationRecord>,
    ) -> Self {
        let active = config.active_provider_identity().ok();
        // Invalid configuration remains visible, but does not admit any route.
        let identity = active
            .as_ref()
            .map(|identity| identity.key.to_string())
            .unwrap_or_else(|| {
                config
                    .provider
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_string())
            });
        let mut models = vec![ModelChoice::default()];
        for (route, model, _) in super::super::fleet_setup::cross_provider_model_routes(
            config,
            active.as_ref(),
            &crate::provider_readiness::ProviderReadinessSnapshot::default(),
        ) {
            // `auto` may use configured cross-provider routing. It is not a
            // concrete provider/model pin; legacy records remain selectable.
            if model.trim().eq_ignore_ascii_case("auto") {
                continue;
            }
            if let Ok(identity) = config.resolve_provider_identity(&route) {
                let choice = ModelChoice {
                    model: Some(model),
                    provider: Some(identity.persisted_kind().to_string()),
                    provider_id: identity.persisted_id().map(str::to_string),
                };
                if !models.contains(&choice) {
                    models.push(choice);
                }
            }
        }
        if let Some(choice) = original.as_ref().map(ModelChoice::from_record)
            && !models.contains(&choice)
        {
            models.push(choice);
        }
        let model = original
            .as_ref()
            .and_then(|row| {
                models
                    .iter()
                    .position(|model| *model == ModelChoice::from_record(row))
            })
            .unwrap_or(0);
        let tomorrow = Local::now().date_naive() + chrono::Duration::days(1);
        let mut editor = Self {
            name: TextField::new(
                original
                    .as_ref()
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
            ),
            prompt: TextField::new(
                original
                    .as_ref()
                    .map(|r| r.prompt.clone())
                    .unwrap_or_default(),
            ),
            rrule: TextField::new(
                original
                    .as_ref()
                    .map(|r| r.rrule.clone())
                    .unwrap_or_default(),
            ),
            workspace: TextField::new(original.as_ref().and_then(|r| r.cwds.first()).map_or_else(
                || workspace.display().to_string(),
                |p| p.display().to_string(),
            )),
            enabled: original
                .as_ref()
                .is_none_or(|r| r.status == AutomationStatus::Active),
            original,
            locale,
            workspace_root: workspace.to_path_buf(),
            provider: identity,
            time: TextField::new("09:00".to_string()),
            date: TextField::new(tomorrow.to_string()),
            preset: Preset::Daily,
            days: [true, false, false, false, false, false, false],
            day_cursor: 0,
            schedule_changed: false,
            models,
            model,
            picking_model: false,
            model_query: TextField::new(String::new()),
            model_row: 0,
            focus: Field::Name,
            scroll: Cell::new(0),
            hits: RefCell::new(Vec::new()),
            preview_cache: RefCell::new(None),
            problem: None,
        };
        if editor.original.is_some() {
            editor.load_schedule();
        }
        editor
    }

    fn load_schedule(&mut self) {
        self.preset = Preset::Custom;
        match AutomationSchedule::parse_rrule(&self.rrule.value) {
            Ok(AutomationSchedule::Weekly {
                byday,
                byhour,
                byminute,
            }) => {
                self.days = DAYS.map(|day| byday.contains(&day));
                self.preset = if self.days.iter().all(|day| *day) {
                    Preset::Daily
                } else {
                    Preset::Weekly
                };
                self.time = TextField::new(format!("{byhour:02}:{byminute:02}"));
            }
            Ok(AutomationSchedule::Hourly {
                interval_hours,
                byday: None,
                anchor_hour: Some(hour),
                anchor_minute: Some(minute),
            }) if interval_hours == 1 || interval_hours == 24 => {
                self.preset = if interval_hours == 1 {
                    Preset::Hourly
                } else {
                    Preset::Daily
                };
                self.time = TextField::new(format!("{hour:02}:{minute:02}"));
            }
            Ok(AutomationSchedule::Once { at }) => {
                self.preset = Preset::Once;
                let local = at.with_timezone(&Local);
                self.time = TextField::new(local.format("%H:%M").to_string());
                self.date = TextField::new(local.format("%Y-%m-%d").to_string());
            }
            _ => {}
        }
    }

    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name, Field::Prompt, Field::Schedule];
        if self.preset == Preset::Custom {
            fields.push(Field::Rrule);
        } else {
            if self.preset == Preset::Once {
                fields.push(Field::Date);
            }
            fields.push(Field::Time);
            if self.preset == Preset::Weekly {
                fields.push(Field::Days);
            }
        }
        fields.extend([
            Field::Model,
            Field::Workspace,
            Field::Enabled,
            Field::Save,
            Field::Cancel,
        ]);
        fields
    }

    fn move_focus(&mut self, delta: isize) {
        let fields = self.fields();
        let row = fields
            .iter()
            .position(|field| *field == self.focus)
            .unwrap_or(0);
        self.focus = fields[crate::tui::list_nav::wrap_index(row, fields.len(), delta)];
    }

    fn text(&mut self) -> Option<&mut TextField> {
        match self.focus {
            Field::Name => Some(&mut self.name),
            Field::Prompt => Some(&mut self.prompt),
            Field::Time => Some(&mut self.time),
            Field::Date => Some(&mut self.date),
            Field::Rrule => Some(&mut self.rrule),
            Field::Workspace => Some(&mut self.workspace),
            _ => None,
        }
    }

    fn rule(&self) -> anyhow::Result<String> {
        if !self.schedule_changed
            && let Some(original) = &self.original
        {
            return Ok(original.rrule.clone());
        }
        if self.preset == Preset::Custom {
            return Ok(self.rrule.value.trim().to_string());
        }
        let time = NaiveTime::parse_from_str(self.time.value.trim(), "%H:%M")?;
        let (hour, minute) = (time.hour(), time.minute());
        Ok(match self.preset {
            Preset::Once => {
                let date = NaiveDate::parse_from_str(self.date.value.trim(), "%Y-%m-%d")?;
                format!("FREQ=ONCE;AT={date}T{hour:02}:{minute:02}")
            }
            Preset::Hourly => format!("FREQ=HOURLY;INTERVAL=1;BYHOUR={hour};BYMINUTE={minute}"),
            Preset::Daily | Preset::Weekly => {
                let days = DAY_CODES
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| self.preset == Preset::Daily || self.days[*i])
                    .map(|(_, day)| *day)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("FREQ=WEEKLY;BYDAY={days};BYHOUR={hour};BYMINUTE={minute}")
            }
            Preset::Custom => unreachable!(),
        })
    }

    fn preview(&self) -> String {
        // Cron search can span years. Paints and typing in unrelated fields
        // reuse the canonical result rather than running that search per frame.
        let rule = self.rule().unwrap_or_else(|_| {
            format!("{:?}:{}:{}", self.preset, self.time.value, self.date.value)
        });
        if let Some((cached_rule, enabled, at, value)) = self.preview_cache.borrow().as_ref()
            && *cached_rule == rule
            && *enabled == self.enabled
            && at.elapsed() < Duration::from_secs(60)
        {
            return value.clone();
        }
        let value = self.compute_preview();
        *self.preview_cache.borrow_mut() =
            Some((rule, self.enabled, Instant::now(), value.clone()));
        value
    }

    fn compute_preview(&self) -> String {
        if !self.enabled {
            return tr(self.locale, MessageId::AutomationEditorPausedPreview).into_owned();
        }
        if !self.schedule_changed
            && let Some(original) = &self.original
            && original.status == AutomationStatus::Active
            && let Some(next) = original.next_run_at
            && next > Utc::now()
        {
            return local_time(next);
        }
        let now = Utc::now();
        match self
            .rule()
            .and_then(|rule| AutomationSchedule::parse_rrule(&rule))
            .and_then(|schedule| {
                schedule.next_after_with_anchor(
                    now,
                    self.original.as_ref().map_or(now, |r| r.created_at),
                )
            }) {
            Ok(next) => local_time(next),
            Err(_) if self.original.is_some() && !self.schedule_changed => {
                tr(self.locale, MessageId::AutomationEditorPreviewUnavailable).into_owned()
            }
            Err(error) => tr(self.locale, MessageId::AutomationEditorInvalidSchedule)
                .replace("{error}", &error.to_string()),
        }
    }

    pub(super) fn save(&self, manager: &AutomationManager) -> anyhow::Result<AutomationRecord> {
        let rrule = self.rule()?;
        let status = if self.enabled {
            AutomationStatus::Active
        } else {
            AutomationStatus::Paused
        };
        let choice = self.models[self.model].clone();
        let old_workspace = self.original.as_ref().map(|r| {
            r.cwds
                .first()
                .unwrap_or(&self.workspace_root)
                .display()
                .to_string()
        });
        let workspace_changed = old_workspace.as_deref() != Some(self.workspace.value.as_str());
        let mut cwds = self
            .original
            .as_ref()
            .map(|r| r.cwds.clone())
            .unwrap_or_default();
        // Leave old multi-workspace definitions intact; the existing scheduler
        // executes their first workspace. Editing that field keeps the tail.
        if workspace_changed {
            let path = PathBuf::from(self.workspace.value.trim());
            let path = if path.is_absolute() {
                path
            } else {
                self.workspace_root.join(path)
            };
            if self.workspace.value.trim().is_empty() || !path.is_dir() {
                anyhow::bail!(
                    "{}",
                    tr(self.locale, MessageId::AutomationEditorInvalidWorkspace)
                );
            }
            let path = path.canonicalize()?;
            if cwds.is_empty() {
                cwds.push(path);
            } else {
                cwds[0] = path;
            }
        }
        if let Some(original) = &self.original {
            let latest = manager.get_automation(&original.id)?;
            let request = UpdateAutomationRequest {
                name: (self.name.value != original.name).then(|| self.name.value.clone()),
                prompt: (self.prompt.value != original.prompt).then(|| self.prompt.value.clone()),
                rrule: self.schedule_changed.then_some(rrule),
                cwds: workspace_changed.then_some(cwds),
                model: (choice != ModelChoice::from_record(original))
                    .then(|| choice.model.clone().unwrap_or_default()),
                model_provider: (choice != ModelChoice::from_record(original))
                    .then(|| choice.provider.clone().unwrap_or_default()),
                model_provider_id: (choice != ModelChoice::from_record(original))
                    .then(|| choice.provider_id.clone().unwrap_or_default()),
                status: (status != original.status).then_some(status),
                ..Default::default()
            };
            if (request.name.is_some() && latest.name != original.name)
                || (request.prompt.is_some() && latest.prompt != original.prompt)
                || (request.rrule.is_some() && latest.rrule != original.rrule)
                || (request.cwds.is_some() && latest.cwds != original.cwds)
                || (request.model.is_some()
                    && ModelChoice::from_record(&latest) != ModelChoice::from_record(original))
                || (request.status.is_some() && latest.status != original.status)
            {
                anyhow::bail!("{}", tr(self.locale, MessageId::AutomationEditorConflict));
            }
            manager.update_automation(&original.id, request)
        } else {
            manager.create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: self.name.value.clone(),
                prompt: self.prompt.value.clone(),
                rrule,
                cwds,
                model: choice.model,
                model_provider: choice.provider,
                model_provider_id: choice.provider_id,
                status: Some(status),
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                delivery_mode: None,
            })
        }
    }

    fn cycle_preset(&mut self, delta: isize) {
        // Keep a custom expression available if the user cycles away and back.
        if self.preset != Preset::Custom
            && let Ok(rule) = self.rule()
        {
            self.rrule = TextField::new(rule);
        }
        let index = Preset::ALL
            .iter()
            .position(|preset| *preset == self.preset)
            .unwrap();
        self.preset =
            Preset::ALL[crate::tui::list_nav::wrap_index(index, Preset::ALL.len(), delta)];
        self.schedule_changed = true;
    }

    fn adjust_time(&mut self, delta: i32) {
        if let Ok(time) = NaiveTime::parse_from_str(self.time.value.trim(), "%H:%M") {
            self.time = TextField::new(
                (time + chrono::Duration::minutes(i64::from(delta)))
                    .format("%H:%M")
                    .to_string(),
            );
            self.schedule_changed = true;
        }
    }

    fn model_label(&self, index: usize) -> String {
        let choice = &self.models[index];
        let model = choice.model.clone().unwrap_or_else(|| {
            tr(self.locale, MessageId::AutomationEditorDefaultModel).into_owned()
        });
        match choice.provider_id.as_ref().or(choice.provider.as_ref()) {
            Some(provider) => format!("{provider} / {model}"),
            None => format!(
                "{model} · {}",
                tr(self.locale, MessageId::AutomationEditorInheritedProvider)
            ),
        }
    }

    fn filtered_models(&self) -> Vec<usize> {
        let query = self.model_query.value.to_lowercase();
        (0..self.models.len())
            .filter(|index| self.model_label(*index).to_lowercase().contains(&query))
            .collect()
    }

    fn open_models(&mut self) {
        self.picking_model = true;
        self.model_query = TextField::new(String::new());
        self.model_row = self.model;
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> EditorAction {
        // AltGr emits printable Ctrl+Alt chords on Windows. It can type into
        // a field, but can never save, cancel, select all, or toggle a control.
        if crate::tui::widgets::key_hint::is_altgr(key.modifiers) {
            if self.picking_model {
                self.model_query.key(key, false);
                self.model_row = 0;
            } else {
                let multiline = self.focus == Field::Prompt;
                let schedule = matches!(self.focus, Field::Time | Field::Date | Field::Rrule);
                if let Some(text) = self.text() {
                    let before = text.value.clone();
                    text.key(key, multiline);
                    if schedule && before != text.value {
                        self.schedule_changed = true;
                    }
                }
            }
            return EditorAction::None;
        }
        if key.modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        ) {
            return EditorAction::None;
        }
        if self.picking_model {
            let models = self.filtered_models();
            match key.code {
                KeyCode::Esc => self.picking_model = false,
                KeyCode::Enter => {
                    if let Some(index) = models.get(self.model_row) {
                        self.model = *index;
                        self.picking_model = false;
                    }
                }
                KeyCode::Up | KeyCode::Down if !models.is_empty() => {
                    self.model_row = crate::tui::list_nav::wrap_index(
                        self.model_row,
                        models.len(),
                        if key.code == KeyCode::Up { -1 } else { 1 },
                    )
                }
                _ => {
                    self.model_query.key(key, false);
                    self.model_row = 0;
                }
            }
            return EditorAction::None;
        }
        if key.code == KeyCode::Esc {
            return EditorAction::Cancel;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('s') {
            return EditorAction::Save;
        }
        self.problem = None;
        match key.code {
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Enter if self.focus == Field::Save => return EditorAction::Save,
            KeyCode::Enter if self.focus == Field::Cancel => return EditorAction::Cancel,
            KeyCode::Left | KeyCode::Right | KeyCode::Enter if self.focus == Field::Schedule => {
                self.cycle_preset(if key.code == KeyCode::Left { -1 } else { 1 })
            }
            KeyCode::Up | KeyCode::Down if self.focus == Field::Time => {
                self.adjust_time(if key.code == KeyCode::Up { 15 } else { -15 })
            }
            KeyCode::Up | KeyCode::Down if self.focus == Field::Date => {
                if let Ok(date) = NaiveDate::parse_from_str(&self.date.value, "%Y-%m-%d")
                    && let Some(next) = date.checked_add_signed(chrono::Duration::days(
                        if key.code == KeyCode::Up { 1 } else { -1 },
                    ))
                {
                    self.date = TextField::new(next.to_string());
                    self.schedule_changed = true;
                }
            }
            KeyCode::Left | KeyCode::Right if self.focus == Field::Days => {
                self.day_cursor = crate::tui::list_nav::wrap_index(
                    self.day_cursor,
                    7,
                    if key.code == KeyCode::Left { -1 } else { 1 },
                )
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == Field::Days => {
                self.days[self.day_cursor] = !self.days[self.day_cursor];
                self.schedule_changed = true;
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                if self.focus == Field::Enabled =>
            {
                self.enabled = !self.enabled
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == Field::Model => self.open_models(),
            KeyCode::Enter if self.focus != Field::Prompt => self.move_focus(1),
            _ => {
                let schedule = matches!(self.focus, Field::Time | Field::Date | Field::Rrule);
                let multiline = self.focus == Field::Prompt;
                if let Some(text) = self.text() {
                    let before = text.value.clone();
                    text.key(key, multiline);
                    if schedule && before != text.value {
                        self.schedule_changed = true;
                    }
                }
            }
        }
        EditorAction::None
    }

    pub(super) fn paste(&mut self, value: &str) {
        if self.picking_model {
            self.model_query.insert(value, false);
            self.model_row = 0;
            return;
        }
        let schedule = matches!(self.focus, Field::Time | Field::Date | Field::Rrule);
        let multiline = self.focus == Field::Prompt;
        if let Some(text) = self.text() {
            let before = text.value.clone();
            text.insert(value, multiline);
            if schedule && before != text.value {
                self.schedule_changed = true;
            }
        }
    }

    pub(super) fn mouse(&mut self, mouse: MouseEvent) -> EditorAction {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                if self.picking_model {
                    let models = self.filtered_models();
                    if !models.is_empty() {
                        self.model_row =
                            crate::tui::list_nav::wrap_index(self.model_row, models.len(), delta);
                    }
                } else {
                    self.move_focus(delta);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .hits
                    .borrow()
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
                    .map(|(_, hit)| *hit);
                match hit {
                    Some(Hit::Field(Field::Save)) => return EditorAction::Save,
                    Some(Hit::Field(Field::Cancel)) if self.picking_model => {
                        self.picking_model = false
                    }
                    Some(Hit::Field(Field::Cancel)) => return EditorAction::Cancel,
                    Some(Hit::Field(field)) => {
                        self.focus = field;
                        match field {
                            Field::Model => self.open_models(),
                            Field::Schedule => self.cycle_preset(1),
                            Field::Enabled => self.enabled = !self.enabled,
                            _ => {}
                        }
                    }
                    Some(Hit::Day(day)) => {
                        self.focus = Field::Days;
                        self.day_cursor = day;
                        self.days[day] = !self.days[day];
                        self.schedule_changed = true;
                    }
                    Some(Hit::Time(delta)) => {
                        self.focus = Field::Time;
                        self.adjust_time(delta);
                    }
                    Some(Hit::Model(index)) => {
                        self.model = index;
                        self.picking_model = false;
                    }
                    None => {}
                }
            }
            _ => {}
        }
        EditorAction::None
    }

    pub(super) fn render(&self, area: Rect, buf: &mut Buffer) {
        self.hits.borrow_mut().clear();
        if area.height < 6 || area.width < 8 {
            return;
        }
        let title = if self.original.is_some() {
            MessageId::AutomationEditorEdit
        } else {
            MessageId::AutomationEditorNew
        };
        Paragraph::new(tr(self.locale, title).into_owned())
            .style(Style::default().fg(palette::WHALE_ACTION).bold())
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
        let subtitle = if self.picking_model {
            tr(self.locale, MessageId::AutomationEditorProvider)
                .replace("{provider}", &self.provider)
        } else {
            tr(self.locale, MessageId::AutomationEditorLocalTime)
                .replace("{zone}", &Local::now().format("%:z").to_string())
        };
        Paragraph::new(subtitle)
            .style(Style::default().fg(palette::TEXT_MUTED))
            .render(Rect::new(area.x, area.y + 1, area.width, 1), buf);
        let content = Rect::new(area.x, area.y + 2, area.width, area.height - 5);
        if self.picking_model {
            self.render_models(content, buf);
        } else {
            self.render_fields(content, buf);
        }
        let next = self.problem.clone().unwrap_or_else(|| {
            format!(
                "{} {}",
                tr(self.locale, MessageId::AutomationNextLabel),
                self.preview()
            )
        });
        Paragraph::new(super::display_text(&next))
            .style(Style::default().fg(if self.problem.is_some() {
                palette::STATUS_ERROR
            } else {
                palette::TEXT_MUTED
            }))
            .render(Rect::new(area.x, area.bottom() - 3, area.width, 1), buf);
        let controls = if self.picking_model {
            MessageId::AutomationEditorModelControls
        } else if self.focus == Field::Prompt {
            MessageId::AutomationEditorPromptControls
        } else {
            MessageId::AutomationEditorControls
        };
        Paragraph::new(tr(self.locale, controls).into_owned())
            .style(Style::default().fg(palette::TEXT_DIM))
            .render(Rect::new(area.x, area.bottom() - 2, area.width, 1), buf);
        {
            let mut x = area.x;
            for (field, key, label) in [
                (Field::Save, "Ctrl+S", MessageId::StatusPickerActionSave),
                (Field::Cancel, "Esc", MessageId::StatusPickerActionCancel),
            ] {
                if self.picking_model && field == Field::Save {
                    continue;
                }
                let text = format!("[{} {key}] ", tr(self.locale, label).trim());
                let width = u16::try_from(text_display_width(&text))
                    .unwrap_or(u16::MAX)
                    .min(area.right().saturating_sub(x));
                let rect = Rect::new(x, area.bottom() - 1, width, 1);
                Paragraph::new(text)
                    .style(self.style(self.focus == field))
                    .render(rect, buf);
                self.hits.borrow_mut().push((rect, Hit::Field(field)));
                x += width;
            }
        }
    }

    fn style(&self, focused: bool) -> Style {
        if focused {
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette::TEXT_SECONDARY)
        }
    }

    fn render_fields(&self, area: Rect, buf: &mut Buffer) {
        let fields: Vec<_> = self
            .fields()
            .into_iter()
            .filter(|f| !matches!(f, Field::Save | Field::Cancel))
            .collect();
        let height = |field| {
            if field == Field::Prompt {
                5
            } else if field == Field::Days {
                let columns = (area.width.saturating_sub(2) / self.day_width()).max(1);
                2 + 7u16.div_ceil(columns)
            } else {
                3
            }
        };
        let focus = fields
            .iter()
            .position(|field| *field == self.focus)
            .unwrap_or(fields.len() - 1);
        let mut start = self.scroll.get().min(focus);
        while start < focus
            && fields[start..=focus]
                .iter()
                .map(|field| height(*field))
                .sum::<u16>()
                > area.height
        {
            start += 1;
        }
        self.scroll.set(start);
        let mut y = area.y;
        for field in fields.into_iter().skip(start) {
            if y >= area.bottom() {
                break;
            }
            let rect = Rect::new(area.x, y, area.width, height(field).min(area.bottom() - y));
            self.render_field(field, rect, buf);
            y += rect.height;
        }
    }

    fn render_field(&self, field: Field, area: Rect, buf: &mut Buffer) {
        let label = match field {
            Field::Name => MessageId::AutomationNameLabel,
            Field::Prompt => MessageId::AutomationPromptLabel,
            Field::Schedule => MessageId::AutomationEditorSchedule,
            Field::Time => MessageId::AutomationEditorTime,
            Field::Days => MessageId::AutomationEditorDays,
            Field::Date => MessageId::AutomationEditorDate,
            Field::Rrule => MessageId::AutomationRruleLabel,
            Field::Model => MessageId::SetupCardModelLabel,
            Field::Workspace => MessageId::AutomationCwdLabel,
            Field::Enabled => MessageId::AutomationEditorEnabled,
            _ => return,
        };
        let focused = field == self.focus;
        let title = tr(self.locale, label).into_owned();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(self.style(focused));
        let inner = block.inner(area);
        block.render(area, buf);
        self.hits.borrow_mut().push((area, Hit::Field(field)));
        let text = match field {
            Field::Name => Some(&self.name),
            Field::Prompt => Some(&self.prompt),
            Field::Time => Some(&self.time),
            Field::Date => Some(&self.date),
            Field::Rrule => Some(&self.rrule),
            Field::Workspace => Some(&self.workspace),
            _ => None,
        };
        if let Some(text) = text {
            let input = if field == Field::Time && inner.width >= 16 {
                Rect::new(inner.x, inner.y, inner.width - 10, inner.height)
            } else {
                inner
            };
            text.render(input, buf, focused);
            if field == Field::Time && inner.width >= 16 {
                for (offset, value, delta) in [(10, "[−]", -15), (5, "[+]", 15)] {
                    let rect = Rect::new(inner.right() - offset, inner.y, 3, inner.height);
                    Paragraph::new(value)
                        .style(self.style(focused))
                        .render(rect, buf);
                    self.hits.borrow_mut().push((rect, Hit::Time(delta)));
                }
            }
            return;
        }
        match field {
            Field::Schedule => {
                Paragraph::new(format!("‹ {} ›", tr(self.locale, self.preset.label())))
                    .style(self.style(focused))
                    .render(inner, buf);
            }
            Field::Model => {
                Paragraph::new(super::display_text(&self.model_label(self.model)))
                    .style(self.style(focused))
                    .render(inner, buf);
            }
            Field::Enabled => {
                Paragraph::new(format!(
                    "{} {}",
                    if self.enabled { "[x]" } else { "[ ]" },
                    tr(
                        self.locale,
                        if self.enabled {
                            MessageId::AutomationStatusActive
                        } else {
                            MessageId::AutomationStatusPaused
                        }
                    )
                ))
                .style(self.style(focused))
                .render(inner, buf);
            }
            Field::Days => {
                let cell_width = self.day_width().min(inner.width).max(1);
                let columns = (inner.width / cell_width).max(1);
                for (day, label) in DAY_LABELS.iter().enumerate() {
                    let text = format!(
                        "{}{} ",
                        if self.days[day] { "✓" } else { "·" },
                        tr(self.locale, *label)
                    );
                    let column = day as u16 % columns;
                    let row = day as u16 / columns;
                    if row >= inner.height {
                        break;
                    }
                    let rect =
                        Rect::new(inner.x + column * cell_width, inner.y + row, cell_width, 1);
                    let style = self.style(focused && self.day_cursor == day);
                    Paragraph::new(text)
                        .style(if focused && self.day_cursor == day {
                            style.add_modifier(Modifier::REVERSED)
                        } else {
                            style
                        })
                        .render(rect, buf);
                    self.hits.borrow_mut().push((rect, Hit::Day(day)));
                }
            }
            _ => {}
        }
    }

    fn day_width(&self) -> u16 {
        DAY_LABELS
            .iter()
            .map(|label| text_display_width(&format!("✓{} ", tr(self.locale, *label))) as u16)
            .max()
            .unwrap_or(1)
            .max(1)
    }

    fn render_models(&self, area: Rect, buf: &mut Buffer) {
        let search = Rect::new(area.x, area.y, area.width, area.height.min(3));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(tr(self.locale, MessageId::ConfigSearchLabel).into_owned())
            .border_style(self.style(true));
        let inner = block.inner(search);
        block.render(search, buf);
        self.model_query.render(inner, buf, true);
        let rows = usize::from(area.height.saturating_sub(3));
        let models = self.filtered_models();
        if models.is_empty() {
            Paragraph::new(tr(self.locale, MessageId::HistoryNoMatches).into_owned()).render(
                Rect::new(
                    area.x,
                    search.bottom(),
                    area.width,
                    area.height.saturating_sub(3),
                ),
                buf,
            );
        }
        let start = self.model_row.saturating_sub(rows.saturating_sub(1));
        for (row, index) in models.into_iter().enumerate().skip(start).take(rows) {
            let rect = Rect::new(
                area.x,
                search.bottom() + u16::try_from(row - start).unwrap_or(0),
                area.width,
                1,
            );
            Paragraph::new(format!(
                "{} {}",
                if row == self.model_row { "▸" } else { " " },
                super::display_text(&self.model_label(index))
            ))
            .style(self.style(row == self.model_row))
            .render(rect, buf);
            self.hits.borrow_mut().push((rect, Hit::Model(index)));
        }
    }
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation_manager::AutomationDeliveryMode;

    fn editor(root: &Path) -> AutomationEditor {
        AutomationEditor::new(&Config::default(), root, Locale::En, None)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn editor_preserves_unknown_schedule_permissions_models_and_unedited_workspace() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let manager = AutomationManager::open_for_test(root.path().join("store")).unwrap();
        let mut draft = editor(root.path());
        draft.name.insert("original", false);
        draft.prompt.insert("line one\nline two", true);
        draft.enabled = false;
        let mut original = draft.save(&manager).unwrap();
        original.rrule = "FREQ=FUTURE;PRESERVE=EXACT".to_string();
        original.cwds.clear();
        original.allow_shell = Some(true);
        original.trust_mode = Some(true);
        original.auto_approve = Some(false);
        original.mode = Some("plan".to_string());
        original.delivery_mode = Some(AutomationDeliveryMode::Task);
        original.model = Some("private/retired-model".to_string());
        original.model_provider = Some("custom".to_string());
        original.model_provider_id = Some("retired-route".to_string());
        manager.save_automation(&original).unwrap();
        let mut edit = AutomationEditor::new(
            &Config::default(),
            root.path(),
            Locale::En,
            Some(original.clone()),
        );
        edit.name = TextField::new("renamed".to_string());
        let saved = edit.save(&manager).unwrap();
        assert_eq!(saved.name, "renamed");
        let mut before = serde_json::to_value(original).unwrap();
        let mut after = serde_json::to_value(saved).unwrap();
        for key in ["name", "updated_at", "revision"] {
            before.as_object_mut().unwrap().remove(key);
            after.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(before, after, "only explicitly edited fields may change");
    }

    #[test]
    fn editor_rejects_invalid_and_conflicting_saves_without_mutation() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let manager = AutomationManager::open_for_test(root.path().join("store")).unwrap();
        let mut draft = editor(root.path());
        assert!(draft.save(&manager).is_err());
        assert!(manager.list_automations().unwrap().is_empty());
        draft.name.insert("name", false);
        draft.prompt.insert("prompt", true);
        let saved = draft.save(&manager).unwrap();
        let mut edit = AutomationEditor::new(
            &Config::default(),
            root.path(),
            Locale::En,
            Some(saved.clone()),
        );
        edit.time = TextField::new("25:90".to_string());
        edit.schedule_changed = true;
        assert!(edit.save(&manager).is_err());
        assert_eq!(
            manager.get_automation(&saved.id).unwrap().updated_at,
            saved.updated_at
        );
        edit.schedule_changed = false;
        edit.name = TextField::new("my edit".to_string());
        manager
            .update_automation(
                &saved.id,
                UpdateAutomationRequest {
                    name: Some("other editor".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            edit.save(&manager)
                .unwrap_err()
                .to_string()
                .contains("changed since opening")
        );
        assert_eq!(
            manager.get_automation(&saved.id).unwrap().name,
            "other editor"
        );
    }

    #[test]
    fn editor_presets_use_canonical_schedules_and_atomic_pause() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let manager = AutomationManager::open_for_test(root.path().join("store")).unwrap();
        let mut draft = editor(root.path());
        draft.name.insert("scheduled", false);
        draft.prompt.insert("prompt", true);
        for preset in Preset::ALL {
            draft.preset = preset;
            draft.schedule_changed = true;
            draft.rrule = TextField::new("FREQ=CRON;EXPR=30 9 * * 1-5".to_string());
            let rule = draft.rule().unwrap();
            AutomationSchedule::parse_rrule(&rule).unwrap();
            let saved = draft.save(&manager).unwrap();
            assert!(saved.next_run_at.unwrap() > Utc::now());
        }
        let saved = draft.save(&manager).unwrap();
        let mut edit =
            AutomationEditor::new(&Config::default(), root.path(), Locale::En, Some(saved));
        edit.preset = Preset::Once;
        edit.date = TextField::new("2020-01-01".to_string());
        edit.schedule_changed = true;
        edit.enabled = false;
        let saved = edit
            .save(&manager)
            .expect("paused one-shot need not have a future run");
        assert_eq!(saved.status, AutomationStatus::Paused);
        assert_eq!(saved.next_run_at, None);
    }

    #[test]
    fn editor_model_choices_pin_exact_custom_routes_and_clear_atomically() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(
            r#"
provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/first/v1"
api_key = "fixture-first"
model = "same-model"
[providers.second]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/second/v1"
api_key = "fixture-second"
model = "same-model"
"#,
        )
        .unwrap();
        let manager = AutomationManager::open_for_test(root.path().join("store")).unwrap();
        let mut draft = AutomationEditor::new(&config, root.path(), Locale::En, None);
        draft.name.insert("name", false);
        draft.prompt.insert("prompt", true);
        draft.enabled = false;
        draft.model = draft
            .models
            .iter()
            .position(|m| {
                m.provider_id.as_deref() == Some("second")
                    && m.model.as_deref() == Some("same-model")
            })
            .unwrap();
        let saved = draft.save(&manager).unwrap();
        assert_eq!(saved.model.as_deref(), Some("same-model"));
        assert_eq!(saved.model_provider.as_deref(), Some("custom"));
        assert_eq!(saved.model_provider_id.as_deref(), Some("second"));
        let mut edit = AutomationEditor::new(&config, root.path(), Locale::En, Some(saved));
        edit.model = 0;
        let saved = edit.save(&manager).unwrap();
        assert_eq!(
            (saved.model, saved.model_provider, saved.model_provider_id),
            (None, None, None)
        );
    }

    #[test]
    fn editor_multiline_unicode_input_does_not_execute_commands() {
        let mut input = TextField::new(String::new());
        input.insert("hello\r\n👨‍👩‍👧‍👦\u{1b}", true);
        assert_eq!(input.value, "hello\n👨‍👩‍👧‍👦");
        input.key(key(KeyCode::Backspace), true);
        assert_eq!(input.value, "hello\n");
        input.key(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            true,
        );
        input.insert("replacement\n/automation delete foo", true);
        assert_eq!(input.value, "replacement\n/automation delete foo");
    }

    #[test]
    fn editor_altgr_cannot_save_cancel_or_erase_the_draft() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let mut draft = editor(root.path());
        draft.name.insert("keep-", false);
        for ch in ['s', 'a', 'u', 'q'] {
            assert!(matches!(
                draft.key(KeyEvent::new(
                    KeyCode::Char(ch),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                )),
                EditorAction::None
            ));
            assert!(draft.name.value.starts_with("keep-"));
            assert!(!draft.name.selected);
        }
        #[cfg(windows)]
        assert_eq!(draft.name.value, "keep-sauq");
        #[cfg(not(windows))]
        assert_eq!(draft.name.value, "keep-");
        assert!(matches!(
            draft.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorAction::Save
        ));
    }
}

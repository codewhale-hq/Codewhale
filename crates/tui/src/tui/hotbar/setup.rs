use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::tui::app::App;
use crate::tui::views::{
    ActionHint, EmptyState, ListDetailLayout, ModalKind, ModalView, ViewAction, ViewEvent,
    centered_modal_area, render_modal_footer, render_modal_surface,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;

#[cfg(test)]
use super::actions::HotbarRecommendation;
use super::actions::{
    HotbarActionCategory, HotbarActionMetadata, HotbarArgsBehavior, HotbarRecommendationOptions,
    HotbarSafetyClass, recommend_hotbar_actions,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarSetupActionRow {
    pub metadata: HotbarActionMetadata,
    pub disabled_reason: Option<String>,
}

impl HotbarSetupActionRow {
    fn status_label(&self, locale: Locale) -> String {
        tr(
            locale,
            if self.disabled_reason.is_some() {
                MessageId::HotbarSetupStatusDisabled
            } else if matches!(self.metadata.args, HotbarArgsBehavior::Required) {
                MessageId::HotbarSetupStatusPrefill
            } else {
                MessageId::HotbarSetupStatusReady
            },
        )
        .into_owned()
    }
}

fn hotbar_setup_source_label(locale: Locale, source: HotbarActionCategory) -> String {
    let id = match source {
        HotbarActionCategory::App => MessageId::HotbarSetupSourceApp,
        HotbarActionCategory::Slash => MessageId::HotbarSetupSourceSlash,
        HotbarActionCategory::Mcp => MessageId::HotbarSetupSourceMcp,
        HotbarActionCategory::Skill => MessageId::HotbarSetupSourceSkill,
        HotbarActionCategory::Plugin => MessageId::HotbarSetupSourcePlugin,
        // `Route` is a source category introduced after PR #3785; it has no
        // dedicated localization key, so fall back to its canonical English label.
        HotbarActionCategory::Route => return source.as_str().to_string(),
    };
    tr(locale, id).into_owned()
}

fn tr_hotbar_setup(locale: Locale, id: MessageId, replacements: &[(&str, String)]) -> String {
    let mut message = tr(locale, id).into_owned();
    for (placeholder, value) in replacements {
        message = message.replace(placeholder, value);
    }
    message
}

fn hotbar_setup_dirty_label(locale: Locale, is_dirty: bool) -> String {
    tr(
        locale,
        if is_dirty {
            MessageId::HotbarSetupDirtyModified
        } else {
            MessageId::HotbarSetupDirtyClean
        },
    )
    .into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarSetupView {
    locale: Locale,
    sources: Vec<HotbarActionCategory>,
    actions: Vec<HotbarSetupActionRow>,
    selected_source_idx: usize,
    selected_action_idx_by_source: BTreeMap<HotbarActionCategory, usize>,
    selected_slot: u8,
    original_bindings: BTreeMap<u8, codewhale_config::HotbarBindingToml>,
    draft_bindings: BTreeMap<u8, codewhale_config::HotbarBindingToml>,
    recommended_action_ids: BTreeSet<String>,
    validation_errors: Vec<String>,
    query: String,
    filter_focused: bool,
    help_visible: bool,
    /// `d` arms this instead of persisting `hotbar = []` straight away.
    /// Disabling rewrites every slot binding on disk and the setup view invites
    /// bare typing as its filter, so a stray keystroke must not be able to
    /// destroy the bindings. Mirrors `SessionPickerView::confirm_delete`.
    confirm_disable: bool,
}

impl HotbarSetupView {
    #[must_use]
    pub fn new(app: &App, config: &Config) -> Self {
        let mut actions = app
            .hotbar_actions
            .iter()
            .map(|action| {
                let metadata = action.metadata(app.ui_locale);
                let disabled_reason = action.disabled_reason(app);
                HotbarSetupActionRow {
                    metadata,
                    disabled_reason,
                }
            })
            .collect::<Vec<_>>();
        actions.sort_by(|a, b| {
            a.metadata
                .category
                .cmp(&b.metadata.category)
                .then_with(|| {
                    a.metadata
                        .display_name
                        .to_ascii_lowercase()
                        .cmp(&b.metadata.display_name.to_ascii_lowercase())
                })
                .then_with(|| a.metadata.id.cmp(&b.metadata.id))
        });

        let sources = actions
            .iter()
            .map(|row| row.metadata.category)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let recommended_action_ids =
            recommend_hotbar_actions(app, HotbarRecommendationOptions::for_setup_wizard())
                .into_iter()
                .map(|entry| entry.metadata.id)
                .collect::<BTreeSet<_>>();

        let known_action_ids = app
            .hotbar_actions
            .iter()
            .map(|action| action.id())
            .collect::<Vec<_>>();
        let original_bindings = config
            .resolve_hotbar_bindings(&known_action_ids)
            .bindings
            .into_iter()
            .map(|binding| {
                (
                    binding.slot,
                    codewhale_config::HotbarBindingToml {
                        slot: binding.slot,
                        action: binding.action,
                        label: binding.label,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        Self {
            locale: app.ui_locale,
            sources,
            actions,
            selected_source_idx: 0,
            selected_action_idx_by_source: BTreeMap::new(),
            selected_slot: 1,
            draft_bindings: original_bindings.clone(),
            original_bindings,
            recommended_action_ids,
            validation_errors: Vec::new(),
            confirm_disable: false,
            query: String::new(),
            filter_focused: false,
            help_visible: false,
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn source_categories(&self) -> &[HotbarActionCategory] {
        &self.sources
    }

    #[must_use]
    pub fn selected_source(&self) -> Option<HotbarActionCategory> {
        self.sources.get(self.selected_source_idx).copied()
    }

    #[must_use]
    #[cfg(test)]
    pub fn selected_slot(&self) -> u8 {
        self.selected_slot
    }

    #[must_use]
    pub fn selected_action(&self) -> Option<&HotbarSetupActionRow> {
        let source = self.selected_source()?;
        self.actions_for_source(source)
            .get(self.selected_action_idx(source))
            .copied()
    }

    #[must_use]
    #[cfg(test)]
    pub fn binding_for_slot(&self, slot: u8) -> Option<&codewhale_config::HotbarBindingToml> {
        self.draft_bindings.get(&slot)
    }

    #[must_use]
    #[cfg(test)]
    pub fn checked_action_ids(&self) -> BTreeSet<String> {
        self.draft_bindings
            .values()
            .map(|binding| binding.action.clone())
            .collect()
    }

    #[must_use]
    #[cfg(test)]
    pub fn recommended_action_ids(&self) -> &BTreeSet<String> {
        &self.recommended_action_ids
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.draft_bindings != self.original_bindings
    }

    #[must_use]
    #[cfg(test)]
    pub fn validation_errors(&self) -> &[String] {
        &self.validation_errors
    }

    #[must_use]
    #[cfg(test)]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The status row, styled. An armed disable confirmation outranks every
    /// other status because it is the only one asking for an answer.
    fn status_line(&self) -> Line<'static> {
        if self.confirm_disable {
            return Line::from(Span::styled(
                tr(self.locale, MessageId::HotbarSetupConfirmDisable).into_owned(),
                Style::default()
                    .fg(palette::STATUS_WARNING)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(self.status_text())
    }

    #[must_use]
    pub fn status_text(&self) -> String {
        if self.confirm_disable {
            return tr(self.locale, MessageId::HotbarSetupConfirmDisable).into_owned();
        }
        if let Some(error) = self.validation_errors.last() {
            return error.clone();
        }
        let dirty = hotbar_setup_dirty_label(self.locale, self.is_dirty());
        let action = self
            .selected_action()
            .map(|row| {
                format!(
                    "{} ({})",
                    row.metadata.display_name,
                    row.status_label(self.locale)
                )
            })
            .unwrap_or_else(|| tr(self.locale, MessageId::HotbarSetupNoAction).into_owned());
        tr_hotbar_setup(
            self.locale,
            MessageId::HotbarSetupStatusLine,
            &[
                ("{slot}", self.selected_slot.to_string()),
                ("{action}", action),
                ("{dirty}", dirty),
            ],
        )
    }

    #[cfg(test)]
    pub fn select_action_by_id(&mut self, action_id: &str) -> bool {
        self.query.clear();
        self.filter_focused = false;
        let Some(row) = self
            .actions
            .iter()
            .find(|row| row.metadata.id == action_id)
            .cloned()
        else {
            return false;
        };
        let Some(source_idx) = self
            .sources
            .iter()
            .position(|source| *source == row.metadata.category)
        else {
            return false;
        };
        self.selected_source_idx = source_idx;
        let index = self
            .actions_for_source(row.metadata.category)
            .iter()
            .position(|candidate| candidate.metadata.id == action_id)
            .unwrap_or(0);
        self.selected_action_idx_by_source
            .insert(row.metadata.category, index);
        self.validation_errors.clear();
        true
    }

    pub fn select_slot(&mut self, slot: u8) -> bool {
        if !(1..=codewhale_config::HOTBAR_SLOT_COUNT).contains(&slot) {
            self.validation_errors = vec![tr_hotbar_setup(
                self.locale,
                MessageId::HotbarSetupSlotOutOfRange,
                &[
                    ("{slot}", slot.to_string()),
                    ("{max}", codewhale_config::HOTBAR_SLOT_COUNT.to_string()),
                ],
            )];
            return false;
        }
        self.selected_slot = slot;
        self.validation_errors.clear();
        true
    }

    pub fn assign_selected_action(&mut self) -> bool {
        let Some(row) = self.selected_action().cloned() else {
            self.validation_errors =
                vec![tr(self.locale, MessageId::HotbarSetupNoActionSelected).into_owned()];
            return false;
        };
        if let Some(reason) = row.disabled_reason {
            self.validation_errors = vec![tr_hotbar_setup(
                self.locale,
                MessageId::HotbarSetupCannotAssign,
                &[
                    ("{action}", row.metadata.display_name),
                    ("{reason}", reason),
                ],
            )];
            return false;
        }
        self.draft_bindings.insert(
            self.selected_slot,
            codewhale_config::HotbarBindingToml {
                slot: self.selected_slot,
                action: row.metadata.id,
                label: None,
            },
        );
        self.validation_errors.clear();
        true
    }

    pub fn toggle_selected_action(&mut self) -> bool {
        let selected_id = self
            .selected_action()
            .map(|row| row.metadata.id.clone())
            .unwrap_or_default();
        if self
            .draft_bindings
            .get(&self.selected_slot)
            .is_some_and(|binding| binding.action == selected_id)
        {
            self.clear_selected_slot();
            true
        } else {
            self.assign_selected_action()
        }
    }

    pub fn clear_selected_slot(&mut self) {
        self.draft_bindings.remove(&self.selected_slot);
        self.validation_errors.clear();
    }

    #[must_use]
    pub fn save_bindings(&self) -> Vec<codewhale_config::HotbarBindingToml> {
        self.draft_bindings.values().cloned().collect()
    }

    fn actions_for_source(&self, source: HotbarActionCategory) -> Vec<&HotbarSetupActionRow> {
        let query = self.query.trim().to_ascii_lowercase();
        self.actions
            .iter()
            .filter(|row| {
                row.metadata.category == source
                    && (query.is_empty() || action_matches_query(row, self.locale, &query))
            })
            .collect()
    }

    fn unfiltered_actions_for_source(
        &self,
        source: HotbarActionCategory,
    ) -> Vec<&HotbarSetupActionRow> {
        self.actions
            .iter()
            .filter(|row| row.metadata.category == source)
            .collect()
    }

    fn selected_action_idx(&self, source: HotbarActionCategory) -> usize {
        let len = self.actions_for_source(source).len();
        if len == 0 {
            return 0;
        }
        self.selected_action_idx_by_source
            .get(&source)
            .copied()
            .unwrap_or(0)
            .min(len.saturating_sub(1))
    }

    fn set_selected_action_idx(&mut self, source: HotbarActionCategory, idx: usize) {
        let len = self.actions_for_source(source).len();
        if len == 0 {
            self.selected_action_idx_by_source.insert(source, 0);
        } else {
            self.selected_action_idx_by_source
                .insert(source, idx.min(len.saturating_sub(1)));
        }
    }

    fn move_source(&mut self, delta: isize) {
        if self.sources.is_empty() {
            return;
        }
        self.selected_source_idx = wrap_index(self.selected_source_idx, self.sources.len(), delta);
        self.validation_errors.clear();
    }

    fn move_action(&mut self, delta: isize) {
        let Some(source) = self.selected_source() else {
            return;
        };
        let len = self.actions_for_source(source).len();
        if len == 0 {
            return;
        }
        let next = wrap_index(self.selected_action_idx(source), len, delta);
        self.set_selected_action_idx(source, next);
        self.validation_errors.clear();
    }

    fn move_slot(&mut self, delta: isize) {
        let len = usize::from(codewhale_config::HOTBAR_SLOT_COUNT);
        let next = wrap_index(usize::from(self.selected_slot - 1), len, delta) + 1;
        self.selected_slot = u8::try_from(next).expect("hotbar slot fits in u8");
        self.validation_errors.clear();
    }

    fn save_action(&self) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::HotbarSetupSaved {
            bindings: self.save_bindings(),
        })
    }

    #[cfg(test)]
    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.extend(self.header_lines());

        let Some(source) = self.selected_source() else {
            lines.push(Line::from(
                tr(self.locale, MessageId::HotbarSetupNoActions).into_owned(),
            ));
            return lines;
        };

        for (idx, row) in self.actions_for_source(source).iter().enumerate() {
            lines.push(self.action_row_line(source, idx, row, 80, true));
        }

        lines.push(Line::from(""));
        lines.push(self.slots_line());
        lines.push(self.status_line());
        lines
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        // The header is painted into `content.height.min(5)` rows with
        // `Wrap { trim: true }`, and the intro wraps to two rows at ordinary
        // widths — which is the whole budget once the slots, tabs and filter
        // rows follow. An armed confirmation must never be the line that falls
        // off the bottom, so it takes the intro's place instead of queueing
        // behind it. Chrome yields to the question; the question is the content.
        if self.confirm_disable {
            return vec![
                self.status_line(),
                self.slots_line(),
                self.source_tabs_line(),
                self.filter_line(),
            ];
        }
        let alt_prefix = crate::tui::widgets::key_hint::alt_prefix();
        vec![
            Line::from(Span::styled(
                format!(
                    "Hotbar gives you {alt_prefix}1-8 shortcuts. Assign actions below; \
                     press 'd' or run `/hotbar off` to hide it."
                ),
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::DIM),
            )),
            self.slots_line(),
            self.source_tabs_line(),
            self.filter_line(),
            self.status_line(),
        ]
    }

    fn source_tabs_line(&self) -> Line<'static> {
        let mut spans = Vec::new();
        for (idx, source) in self.sources.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw("  "));
            }
            let count = self.unfiltered_actions_for_source(*source).len();
            let name = hotbar_setup_source_label(self.locale, *source);
            let label = if Some(*source) == self.selected_source() {
                format!("[{name} {count}]")
            } else {
                format!("{name} {count}")
            };
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(if Some(*source) == self.selected_source() {
                        Color::Cyan
                    } else {
                        palette::TEXT_MUTED
                    })
                    .add_modifier(if Some(*source) == self.selected_source() {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ));
        }
        Line::from(spans)
    }

    fn filter_line(&self) -> Line<'static> {
        let value = if self.query.is_empty() {
            if self.filter_focused {
                "type to filter".to_string()
            } else {
                "press / or type to filter".to_string()
            }
        } else {
            self.query.clone()
        };
        Line::from(vec![
            Span::styled("Filter ", Style::default().fg(palette::TEXT_MUTED)),
            Span::styled(
                value,
                Style::default().fg(if self.filter_focused {
                    palette::WHALE_ACTION
                } else {
                    palette::TEXT_PRIMARY
                }),
            ),
        ])
    }

    fn slots_line(&self) -> Line<'static> {
        let slots = (1..=codewhale_config::HOTBAR_SLOT_COUNT)
            .map(|slot| {
                let label = self
                    .draft_bindings
                    .get(&slot)
                    .map(|binding| compact_action_id(&binding.action))
                    .unwrap_or_else(|| {
                        tr(self.locale, MessageId::HotbarSetupEmptySlot).into_owned()
                    });
                if slot == self.selected_slot {
                    format!("[{slot}:{label}]")
                } else {
                    format!("{slot}:{label}")
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        Line::from(slots)
    }

    fn action_row_line(
        &self,
        source: HotbarActionCategory,
        idx: usize,
        row: &HotbarSetupActionRow,
        max_width: u16,
        show_description: bool,
    ) -> Line<'static> {
        let selected = idx == self.selected_action_idx(source);
        let marker = crate::tui::glyphs::selection_marker(selected);
        let checked = if self
            .draft_bindings
            .values()
            .any(|binding| binding.action == row.metadata.id)
        {
            "*"
        } else {
            " "
        };
        let recommended = if self.recommended_action_ids.contains(&row.metadata.id) {
            tr(self.locale, MessageId::HotbarSetupRecommended).into_owned()
        } else {
            String::new()
        };
        let prefix = format!(
            "{marker}{checked} {:<3} {:<22} {:<8} ",
            recommended,
            row.metadata.display_name,
            row.status_label(self.locale)
        );
        let suffix = if let Some(reason) = row.disabled_reason.as_deref() {
            format!(" ({reason})")
        } else {
            String::new()
        };
        let (prefix, description) = if show_description {
            (prefix.as_str(), row.metadata.description.as_str())
        } else {
            (prefix.trim_end(), "")
        };
        let text = crate::tui::ui_text::semantic_truncate_with_affixes(
            prefix,
            description,
            &suffix,
            usize::from(max_width),
        );
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(if selected {
                    palette::WHALE_ACTION
                } else {
                    palette::TEXT_PRIMARY
                })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ))
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.header_lines())
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }

    fn render_action_list(&self, area: Rect, buf: &mut Buffer, show_descriptions: bool) {
        let Some(source) = self.selected_source() else {
            EmptyState::new("No actions", "No hotbar action sources are available.")
                .render(area, buf);
            return;
        };
        let rows = self.actions_for_source(source);
        if rows.is_empty() {
            EmptyState::new(
                "No matching actions",
                "Clear the filter or switch categories to find another bindable action.",
            )
            .primary_action("/", "filter")
            .secondary_action("Esc", "clear filter")
            .render(area, buf);
            return;
        }
        let mut lines = vec![Line::from(Span::styled(
            format!("{} actions", source.as_str()),
            Style::default()
                .fg(palette::TEXT_MUTED)
                .add_modifier(Modifier::BOLD),
        ))];
        // Keep the focused row inside the rendered viewport. The list used to
        // render only its first rows, so keyboard selection could advance past
        // `/export` while the highlight stayed behind (#4418).
        let visible_rows = usize::from(area.height.saturating_sub(1));
        let visible_range =
            action_list_visible_range(self.selected_action_idx(source), rows.len(), visible_rows);
        for idx in visible_range {
            lines.push(self.action_row_line(source, idx, rows[idx], area.width, show_descriptions));
        }
        Paragraph::new(lines)
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .render(area, buf);
    }

    fn render_action_detail(&self, area: Rect, buf: &mut Buffer) {
        let Some(row) = self.selected_action() else {
            EmptyState::new(
                "Select an action",
                "Move through the catalog to preview the selected slot binding.",
            )
            .primary_action("Tab", "category")
            .secondary_action("/", "filter")
            .render(area, buf);
            return;
        };
        Paragraph::new(self.detail_lines(row))
            .style(Style::default().fg(palette::TEXT_PRIMARY))
            .wrap(Wrap { trim: true })
            .render(area, buf);
    }

    fn detail_lines(&self, row: &HotbarSetupActionRow) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(
                row.metadata.display_name.clone(),
                Style::default()
                    .fg(palette::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                row.metadata.id.clone(),
                Style::default().fg(palette::TEXT_MUTED),
            )),
            Line::from(""),
            Line::from(format!("Category: {}", row.metadata.category.as_str())),
            Line::from(format!("Status: {}", row.status_label(self.locale))),
            Line::from(format!("Safety: {}", safety_label(row.metadata.safety))),
            Line::from(format!("Arguments: {}", args_label(row.metadata.args))),
            Line::from(format!(
                "Slot {}: {}",
                self.selected_slot,
                self.selected_slot_binding_label()
            )),
            Line::from(""),
            Line::from(row.metadata.description.clone()),
            Line::from(""),
            Line::from(preview_line(row)),
        ];
        if let Some(reason) = row.disabled_reason.as_deref() {
            lines.push(Line::from(Span::styled(
                format!("Unavailable: {reason}"),
                Style::default().fg(palette::STATUS_WARNING),
            )));
        }
        if self.help_visible {
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Save writes staged slots; Esc cancels staged changes unless a filter is active.",
            ));
            lines.push(Line::from(
                "After save: Alt+1 through Alt+8 dispatch Hotbar slots. Bare 1-8 stay composer text outside setup.",
            ));
        }
        lines
    }

    fn selected_slot_binding_label(&self) -> String {
        let Some(binding) = self.draft_bindings.get(&self.selected_slot) else {
            return tr(self.locale, MessageId::HotbarSetupEmptySlot).into_owned();
        };
        self.actions
            .iter()
            .find(|row| row.metadata.id == binding.action)
            .map(|row| row.metadata.display_name.clone())
            .unwrap_or_else(|| binding.action.clone())
    }
}

impl ModalView for HotbarSetupView {
    fn kind(&self) -> ModalKind {
        ModalKind::HotbarSetup
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // The disable confirmation owns every key while it is armed, so the
        // filter cannot swallow the answer and no other action can fire under it.
        if self.confirm_disable {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm_disable = false;
                    ViewAction::EmitAndClose(ViewEvent::HotbarDisableRequested)
                }
                _ => {
                    self.confirm_disable = false;
                    ViewAction::None
                }
            };
        }

        match key.code {
            KeyCode::Esc if self.filter_focused || !self.query.is_empty() => {
                self.query.clear();
                self.filter_focused = false;
                self.validation_errors.clear();
                ViewAction::None
            }
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Char('q') | KeyCode::Char('Q')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                ViewAction::Close
            }
            KeyCode::Tab => {
                self.move_source(1);
                ViewAction::None
            }
            KeyCode::BackTab => {
                self.move_source(-1);
                ViewAction::None
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                self.move_source(-1);
                ViewAction::None
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                self.move_source(1);
                ViewAction::None
            }
            KeyCode::Left => {
                self.move_slot(-1);
                ViewAction::None
            }
            KeyCode::Right => {
                self.move_slot(1);
                ViewAction::None
            }
            KeyCode::Up => {
                self.move_action(-1);
                ViewAction::None
            }
            KeyCode::Char('k') | KeyCode::Char('K')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                self.move_action(-1);
                ViewAction::None
            }
            KeyCode::Down => {
                self.move_action(1);
                ViewAction::None
            }
            KeyCode::Char('j') | KeyCode::Char('J')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                self.move_action(1);
                ViewAction::None
            }
            KeyCode::Enter => {
                self.assign_selected_action();
                ViewAction::None
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                self.assign_selected_action();
                ViewAction::None
            }
            KeyCode::Char(' ') => {
                self.toggle_selected_action();
                ViewAction::None
            }
            KeyCode::Backspace if self.filter_focused || !self.query.is_empty() => {
                self.query.pop();
                if self.query.is_empty() {
                    self.filter_focused = false;
                }
                self.validation_errors.clear();
                ViewAction::None
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.clear_selected_slot();
                ViewAction::None
            }
            KeyCode::Char('c') | KeyCode::Char('C')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                self.clear_selected_slot();
                ViewAction::None
            }
            KeyCode::Char(ch) if ('1'..='8').contains(&ch) => {
                let slot = ch.to_digit(10).expect("digit") as u8;
                self.select_slot(slot);
                ViewAction::None
            }
            KeyCode::Char('s') | KeyCode::Char('S')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                self.save_action()
            }
            KeyCode::Char('d') | KeyCode::Char('D')
                if key.modifiers.is_empty() && !self.filter_focused =>
            {
                // "Disable Hotbar" from inside the setup flow: hide it and
                // persist `hotbar = []`. Mirrors `/hotbar off`. Arm the
                // confirmation rather than writing; `y` commits.
                self.confirm_disable = true;
                ViewAction::None
            }
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.filter_focused = true;
                self.validation_errors.clear();
                ViewAction::None
            }
            KeyCode::Char('?') => {
                self.help_visible = !self.help_visible;
                ViewAction::None
            }
            KeyCode::Char(ch) if key.modifiers.is_empty() => {
                self.filter_focused = true;
                self.query.push(ch);
                self.validation_errors.clear();
                if let Some(source) = self.selected_source() {
                    self.set_selected_action_idx(source, 0);
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup_area = centered_modal_area(area, 118, 28, 72, 12);
        render_modal_surface(area, popup_area, buf);
        let block = Block::default()
            .title(Line::from(Span::styled(
                tr(self.locale, MessageId::HotbarSetupTitle),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG));
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("Tab/Shift+Tab", "source"),
                ActionHint::new("↑/↓", "action"),
                ActionHint::new("1-8", "slot"),
                ActionHint::new("/", "filter"),
                ActionHint::new("Enter/A", "assign"),
                ActionHint::new("Space", "toggle"),
                ActionHint::new("C/Delete", "clear"),
                ActionHint::new("s", "save"),
                ActionHint::new("d", "disable"),
                ActionHint::new("Esc", "cancel"),
            ],
        );
        let header_height = content.height.min(5);
        let header = Rect {
            x: content.x,
            y: content.y,
            width: content.width,
            height: header_height,
        };
        self.render_header(header, buf);
        let body = Rect {
            x: content.x,
            y: content.y + header_height,
            width: content.width,
            height: content.height.saturating_sub(header_height),
        };
        let layout = ListDetailLayout::split(body, 34);
        self.render_action_list(layout.list, buf, layout.stacked);
        self.render_action_detail(layout.detail, buf);
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = isize::try_from(len).expect("len fits in isize");
    let current = isize::try_from(current).expect("current fits in isize");
    usize::try_from((current + delta).rem_euclid(len)).expect("wrapped index fits")
}

fn action_list_visible_range(
    selected_idx: usize,
    row_count: usize,
    visible_rows: usize,
) -> std::ops::Range<usize> {
    if row_count == 0 || visible_rows == 0 {
        return 0..0;
    }
    let selected_idx = selected_idx.min(row_count.saturating_sub(1));
    let start = selected_idx.saturating_add(1).saturating_sub(visible_rows);
    let end = start.saturating_add(visible_rows).min(row_count);
    start..end
}

fn action_matches_query(row: &HotbarSetupActionRow, locale: Locale, query: &str) -> bool {
    let status = row.status_label(locale);
    [
        row.metadata.id.as_str(),
        row.metadata.display_name.as_str(),
        row.metadata.description.as_str(),
        row.metadata.category.as_str(),
        status.as_str(),
        row.disabled_reason.as_deref().unwrap_or_default(),
    ]
    .into_iter()
    .any(|value| value.to_ascii_lowercase().contains(query))
}

fn safety_label(safety: HotbarSafetyClass) -> &'static str {
    match safety {
        HotbarSafetyClass::LocalUi => "safe UI",
        HotbarSafetyClass::LocalState => "local state",
        HotbarSafetyClass::ConfigChange => "config change",
        HotbarSafetyClass::ExternalInput => "external input",
        HotbarSafetyClass::ExistingCommand => "existing command",
        HotbarSafetyClass::RequiresApproval => "approval gated",
    }
}

fn args_label(args: HotbarArgsBehavior) -> &'static str {
    match args {
        HotbarArgsBehavior::None => "none",
        HotbarArgsBehavior::Optional => "optional",
        HotbarArgsBehavior::Required => "prefill required arguments",
    }
}

fn preview_line(row: &HotbarSetupActionRow) -> String {
    match (row.metadata.category, row.metadata.args) {
        (HotbarActionCategory::Route, _) => {
            "Preview: switches provider/model through /model route logic.".to_string()
        }
        (_, HotbarArgsBehavior::Required) => {
            "Preview: pre-fills the composer instead of running blindly.".to_string()
        }
        _ => "Preview: dispatches through the existing Hotbar action path.".to_string(),
    }
}

fn compact_action_id(action_id: &str) -> String {
    let suffix = action_id.rsplit('.').next().unwrap_or(action_id);
    crate::tui::ui_text::truncate_line_to_width(suffix, 7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::hotbar::HotbarActionRegistry;
    use codewhale_config::ProviderKind;
    use codewhale_localization::{Locale, MessageId, tr};
    use codewhale_runtime::config::Config;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn test_app_with_config(config: &Config) -> App {
        let options = TuiOptions {
            start_in_agent_mode: true,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, config);
        app.ui_locale = Locale::En;
        app
    }

    fn test_app_with_locale(locale: Locale) -> App {
        let mut app = test_app();
        app.ui_locale = locale;
        app
    }

    fn test_app() -> App {
        test_app_with_config(&Config::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn rendered_text_at(view: &HotbarSetupView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn rendered_text(view: &HotbarSetupView) -> String {
        rendered_text_at(view, 140, 36)
    }

    #[test]
    fn wizard_sources_follow_registered_action_categories() {
        let app = test_app();
        let view = HotbarSetupView::new(&app, &Config::default());

        // Skills are registered from whatever the startup skill cache
        // discovered, so only the always-present categories are asserted
        // in order here (see wizard_lists_skill_and_mcp_sources_when_registered
        // for the injected-source coverage).
        assert!(
            view.source_categories().starts_with(&[
                HotbarActionCategory::App,
                HotbarActionCategory::Route,
                HotbarActionCategory::Slash,
            ]),
            "unexpected wizard sources: {:?}",
            view.source_categories()
        );
        // MCP tools only appear after a live discovery snapshot lands, and
        // plugins stay a deferred source.
        assert!(
            !view
                .source_categories()
                .contains(&HotbarActionCategory::Mcp)
        );
        assert!(
            !view
                .source_categories()
                .contains(&HotbarActionCategory::Plugin)
        );
        assert_eq!(view.selected_source(), Some(HotbarActionCategory::App));
        assert!(view.recommended_action_ids().contains("mode.agent"));
        // #3807: a fresh config seeds no bindings, so the wizard opens with
        // nothing checked until the user opts in.
        assert!(view.checked_action_ids().is_empty());
    }

    #[test]
    fn wizard_lists_skill_and_mcp_sources_when_registered() {
        let mut app = test_app();
        let mut registry = HotbarActionRegistry::with_builtins();
        registry.register_skills(&[("demo".to_string(), "Demo skill".to_string())]);
        registry.replace_mcp_tools(Some(&crate::mcp::McpManagerSnapshot {
            config_path: PathBuf::from("mcp.json"),
            config_exists: true,
            reload_required: false,
            servers: vec![crate::mcp::McpServerSnapshot {
                name: "search".to_string(),
                enabled: true,
                required: false,
                transport: "stdio".to_string(),
                command_or_url: "search-server".to_string(),
                connect_timeout: 5,
                execute_timeout: 5,
                read_timeout: 5,
                connected: true,
                error: None,
                auth_required: false,
                capability_metadata: crate::mcp::McpServerCapabilityMetadata::LegacyFallback,
                tools: vec![crate::mcp::McpDiscoveredItem {
                    name: "web_search".to_string(),
                    model_name: "mcp_search_web_search".to_string(),
                    description: Some("Search the web".to_string()),
                }],
                resources: Vec::new(),
                prompts: Vec::new(),
            }],
        }));
        app.hotbar_actions = registry;
        let mut view = HotbarSetupView::new(&app, &Config::default());

        assert!(
            view.source_categories()
                .contains(&HotbarActionCategory::Skill)
        );
        assert!(
            view.source_categories()
                .contains(&HotbarActionCategory::Mcp)
        );

        // Skills assign like any direct action; MCP tools stay assignable as
        // composer-prefill actions.
        assert!(view.select_slot(4));
        assert!(view.select_action_by_id("skill.demo"));
        assert!(view.assign_selected_action());
        assert_eq!(
            view.binding_for_slot(4)
                .map(|binding| binding.action.as_str()),
            Some("skill.demo")
        );

        assert!(view.select_action_by_id("mcp.search.web_search"));
        assert!(
            view.status_text().contains("prefill"),
            "MCP tools must be labeled as prefill actions: {}",
            view.status_text()
        );
        assert!(view.select_slot(5));
        assert!(view.assign_selected_action());
        assert_eq!(
            view.binding_for_slot(5)
                .map(|binding| binding.action.as_str()),
            Some("mcp.search.web_search")
        );
    }

    #[test]
    fn wizard_chrome_uses_non_english_locale() {
        let app = test_app_with_locale(Locale::ZhHant);
        let mut view = HotbarSetupView::new(&app, &Config::default());
        view.clear_selected_slot();
        view.handle_key(key(KeyCode::Char('?')));

        let status = view.status_text();
        assert!(status.contains("槽位 1"), "status was {status:?}");
        // `Config::default()` ships no default bindings, so a freshly-cleared slot
        // is clean; assert the localized clean label (dirty localization is covered
        // by the wider render checks below) and that no English chrome leaks.
        assert!(
            status.contains(tr(Locale::ZhHant, MessageId::HotbarSetupDirtyClean).as_ref()),
            "status was {status:?}"
        );
        assert!(!status.contains("slot 1 |"), "status was {status:?}");
        assert!(!status.contains("clean"), "status was {status:?}");

        let rendered = rendered_text(&view);
        let compact_rendered = rendered.replace(' ', "");
        // Localized chrome the PR routes through message IDs: title, source tabs
        // (the selected tab is bracketed and now carries a count from PR #3987),
        // status line, and localized built-in action names.
        for expected in [
            "Hotbar設定",
            "[應用",
            "斜線指令",
            "就緒",
            "槽位",
            "Work模式",
            "指令面板",
            "切換側邊欄",
        ] {
            assert!(
                compact_rendered.contains(expected),
                "missing {expected:?} in render:\n{rendered}"
            );
        }
        assert!(
            compact_rendered.contains(":空"),
            "missing localized empty slot:\n{rendered}"
        );

        // English must not leak on the surfaces the PR localizes. The keybinding
        // footer, filter row, and detail labels are English scaffolding added by
        // PR #3987 after this contribution and are intentionally out of scope.
        for leaked in [
            "Hotbar setup",
            "slot 1 |",
            "ready",
            "modified",
            "empty",
            "Work mode",
            "Command palette",
            "Toggle workbar",
            "Switch the conversation",
        ] {
            assert!(
                !rendered.contains(leaked),
                "leaked {leaked:?} in render:\n{rendered}"
            );
        }
    }

    #[test]
    fn wizard_assigns_replaces_toggles_and_clears_slots() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());

        assert!(view.select_slot(1));
        assert!(view.select_action_by_id("mode.plan"));
        assert!(view.assign_selected_action());
        assert_eq!(
            view.binding_for_slot(1)
                .map(|binding| binding.action.as_str()),
            Some("mode.plan")
        );

        assert!(view.select_action_by_id("mode.agent"));
        assert!(view.assign_selected_action());
        assert_eq!(
            view.binding_for_slot(1)
                .map(|binding| binding.action.as_str()),
            Some("mode.agent")
        );
        assert!(view.is_dirty());

        assert!(view.toggle_selected_action());
        assert!(view.binding_for_slot(1).is_none());
        view.clear_selected_slot();
        assert!(view.binding_for_slot(1).is_none());
    }

    #[test]
    fn wizard_save_emits_bindings_but_escape_only_closes() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());
        assert!(view.select_slot(8));
        assert!(view.select_action_by_id("sidebar.toggle"));
        assert!(view.assign_selected_action());

        match view.handle_key(key(KeyCode::Char('s'))) {
            ViewAction::EmitAndClose(ViewEvent::HotbarSetupSaved { bindings }) => {
                assert!(
                    bindings
                        .iter()
                        .any(|binding| { binding.slot == 8 && binding.action == "sidebar.toggle" })
                );
            }
            other => panic!("expected HotbarSetupSaved, got {other:?}"),
        }

        let mut view = HotbarSetupView::new(&app, &Config::default());
        assert!(view.select_slot(1));
        assert!(view.select_action_by_id("mode.agent"));
        assert!(view.assign_selected_action());
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::Close
        ));
    }

    #[test]
    fn wizard_disable_key_emits_disable_request_and_intro_mentions_it() {
        let app = test_app();

        // 'd' and 'D' hide the Hotbar from inside the setup flow (mirrors /hotbar
        // off), but only after the confirmation is answered. Disabling rewrites
        // every slot binding on disk, and this view takes bare letters as its
        // filter, so the first keystroke must never be the destructive one.
        for ch in ['d', 'D'] {
            let mut view = HotbarSetupView::new(&app, &Config::default());
            assert!(
                matches!(view.handle_key(key(KeyCode::Char(ch))), ViewAction::None),
                "{ch} must arm the confirmation, not disable the Hotbar"
            );
            assert!(
                view.status_text().contains("(y/n)"),
                "the armed confirmation must be visible, got {:?}",
                view.status_text()
            );
            assert!(matches!(
                view.handle_key(key(KeyCode::Char('y'))),
                ViewAction::EmitAndClose(ViewEvent::HotbarDisableRequested)
            ));
        }

        // The armed confirmation must survive the header's five-row budget at a
        // real terminal width. `status_text()` returning it is not enough: the
        // intro wraps to two rows, and the confirmation used to be the sixth
        // line into a five-line region, so `d` armed a question the user never
        // saw. Assert the painted buffer, not the string.
        {
            use ratatui::{Terminal, backend::TestBackend};
            let mut view = HotbarSetupView::new(&app, &Config::default());
            assert!(matches!(
                view.handle_key(key(KeyCode::Char('d'))),
                ViewAction::None
            ));
            for (w, h) in [(100u16, 32u16), (80, 24), (140, 40)] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
                terminal
                    .draw(|frame| {
                        let area = frame.area();
                        view.render(area, frame.buffer_mut());
                    })
                    .expect("draw");
                let painted = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(
                    painted.contains("(y/n)"),
                    "{w}x{h}: the armed disable confirmation is not on screen"
                );
            }
        }

        // Anything other than y dismisses it, and the keystroke is spent on the
        // dismissal rather than falling through to whatever it normally does.
        let mut view = HotbarSetupView::new(&app, &Config::default());
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('d'))),
            ViewAction::None
        ));
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('n'))),
            ViewAction::None
        ));
        assert!(!view.status_text().contains("(y/n)"));
        assert!(
            matches!(view.handle_key(key(KeyCode::Esc)), ViewAction::Close),
            "a dismissed confirmation returns the view to its ordinary key table"
        );

        let view = HotbarSetupView::new(&app, &Config::default());

        // The always-visible intro explains what Hotbar is and the disable path.
        let joined: String = view
            .render_lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            joined.contains("shortcuts"),
            "intro should explain what Hotbar is: {joined:?}"
        );
        assert!(
            joined.contains("/hotbar off"),
            "intro should mention the disable path: {joined:?}"
        );
    }

    #[test]
    fn disabled_actions_are_visible_but_not_assignable() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());
        let reasoning = view
            .actions
            .iter_mut()
            .find(|row| row.metadata.id == "reasoning.cycle")
            .expect("reasoning action");
        reasoning.disabled_reason = Some("disabled by test policy".to_string());

        assert!(view.select_slot(2));
        assert!(view.select_action_by_id("reasoning.cycle"));
        assert!(!view.assign_selected_action());

        assert_ne!(
            view.binding_for_slot(2)
                .map(|binding| binding.action.as_str()),
            Some("reasoning.cycle")
        );
        assert!(
            view.validation_errors()
                .last()
                .is_some_and(|error| error.contains("cannot be assigned"))
        );
        assert!(view.status_text().contains("cannot be assigned"));
    }

    #[test]
    fn args_required_slash_actions_are_visible_and_assignable_as_prefill() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());

        assert!(view.select_action_by_id("slash.rename"));
        assert!(
            view.status_text().contains("prefill"),
            "required-arg commands must be labeled as prefill actions"
        );
        assert!(view.select_slot(3));
        assert!(view.assign_selected_action());

        assert_eq!(
            view.binding_for_slot(3)
                .map(|binding| binding.action.as_str()),
            Some("slash.rename")
        );
    }

    #[test]
    fn wizard_help_documents_runtime_hotbar_shortcut() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());

        assert!(matches!(
            view.handle_key(key(KeyCode::Char('?'))),
            ViewAction::None
        ));
        let rendered = view
            .selected_action()
            .map(|row| view.detail_lines(row))
            .expect("selected action")
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("After save: Alt+1 through Alt+8 dispatch Hotbar slots"));
        assert!(rendered.contains("Bare 1-8 stay composer text outside setup"));
    }

    #[test]
    fn action_rows_semantically_truncate_descriptions_at_narrow_width() {
        let app = test_app();
        let view = HotbarSetupView::new(&app, &Config::default());
        let row = HotbarSetupActionRow {
            metadata: HotbarActionMetadata {
                id: "test.long-description".to_string(),
                source_id: "test".to_string(),
                display_name: "Open settings row".to_string(),
                compact_label: "test".to_string(),
                description: "Open a detailed settings panel without clipping".to_string(),
                category: HotbarActionCategory::App,
                args: HotbarArgsBehavior::None,
                safety: HotbarSafetyClass::LocalUi,
                recommendation: HotbarRecommendation::Eligible,
            },
            disabled_reason: None,
        };

        let text = view
            .action_row_line(HotbarActionCategory::App, 0, &row, 58, true)
            .to_string();
        assert!(crate::tui::ui_text::text_display_width(&text) <= 58);
        assert!(text.contains("Open a detailed…"), "{text:?}");
        assert!(!text.contains("Open a detailed s"), "{text:?}");
    }

    #[test]
    fn keyboard_controls_navigate_source_action_and_slot() {
        let mut config = {
            let __cw_config_init_0_0 = Some(ProviderKind::Deepseek.as_str().to_string());
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
            .unwrap()
            .model = Some("anthropic/claude-sonnet-4".to_string());
        let app = test_app_with_config(&config);
        let mut view = HotbarSetupView::new(&app, &config);

        assert_eq!(view.selected_source(), Some(HotbarActionCategory::App));
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.selected_source(), Some(HotbarActionCategory::Route));
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.selected_source(), Some(HotbarActionCategory::Slash));
        view.handle_key(key(KeyCode::BackTab));
        assert_eq!(view.selected_source(), Some(HotbarActionCategory::Route));

        let first = view
            .selected_action()
            .map(|row| row.metadata.id.clone())
            .expect("first action");
        view.handle_key(key(KeyCode::Down));
        let second = view
            .selected_action()
            .map(|row| row.metadata.id.clone())
            .expect("second action");
        assert_ne!(first, second);

        view.handle_key(key(KeyCode::Char('8')));
        assert_eq!(view.selected_slot(), 8);
        view.handle_key(key(KeyCode::Left));
        assert_eq!(view.selected_slot(), 7);
    }

    #[test]
    fn down_past_export_keeps_the_selected_action_visible() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());
        assert!(view.select_action_by_id("slash.export"));

        view.handle_key(key(KeyCode::Down));
        let selected = view.selected_action().expect("action after /export");
        assert_ne!(selected.metadata.id, "slash.export");

        let rendered = rendered_text_at(&view, 80, 24);
        let marker = crate::tui::glyphs::selection_marker(true);
        assert!(
            rendered.lines().any(|line| {
                line.contains(marker) && line.contains(&selected.metadata.display_name)
            }),
            "focused action {} must remain visible after moving past /export:\n{rendered}",
            selected.metadata.id
        );
    }

    #[test]
    fn keyboard_filter_searches_catalog_and_escape_clears_it() {
        let app = test_app();
        let mut view = HotbarSetupView::new(&app, &Config::default());

        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.selected_source(), Some(HotbarActionCategory::Route));
        let route_label = view
            .selected_action()
            .map(|row| row.metadata.display_name.clone())
            .expect("route action");
        let route_query = route_label
            .chars()
            .take(4)
            .collect::<String>()
            .to_ascii_lowercase();
        view.handle_key(key(KeyCode::Char('/')));
        for ch in route_query.chars() {
            view.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(view.query(), route_query);
        assert!(view.status_text().contains(&route_label));

        view.handle_key(key(KeyCode::Esc));
        assert_eq!(view.query(), "");
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc)),
            ViewAction::Close
        ));
    }

    #[test]
    fn hotbar_setup_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        use unicode_width::UnicodeWidthStr;

        const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];
        let app = test_app();
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(HotbarSetupView::new(&app, &Config::default()));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let text = rows.join("\n");

            // Footer keeps every action.
            for label in [
                "source", "action", "slot", "filter", "assign", "toggle", "clear", "save",
                "disable", "cancel",
            ] {
                assert!(text.contains(label), "{w}x{h}: footer missing '{label}'");
            }

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
}

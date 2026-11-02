//! `/theme` picker with live preview.
//!
//! Built on [`crate::tui::settings_picker`]: navigation, filtering ownership,
//! and transactional preview/commit/rollback live in the shared controller.
//! Theme-specific chrome (swatches, underwater surface) stays here so the
//! framework contract does not flatten visual character.
//!
//! Semantics preserved from the pre-framework picker:
//! - Up/Down emit a `ThemeSelectionUpdated{persist:false}` so the host swaps
//!   `app.ui_theme` immediately and the whole TUI re-paints under the modal.
//! - Enter persists (`persist:true`); Esc emits one more
//!   `ThemeSelectionUpdated{persist:false}` to restore the exact theme that
//!   was active when the picker opened.
//!
//! The option list contains compiled themes followed by valid user overlays.
//! `underwater` is an ordinary row: the painted ocean field is the theme, not
//! a treatment beside it.

use std::borrow::Cow;
use std::cell::RefCell;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::settings_picker::{
    PickerNavResult, SettingAvailability, SettingOption, SettingValues, SettingsPickerController,
    SettingsPickerLayout, handle_nav_key,
};
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, render_modal_footer,
    render_panel_scroll_rail, render_underwater_surface,
};
use codewhale_config::settings_schema::DEFAULT_TUI_THEME;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette::{SELECTABLE_THEMES, ThemeId, UiTheme};
use codewhale_tui_kit::menu_style;

pub struct ThemePickerView {
    controller: SettingsPickerController,
    /// Exact opening state for Esc rollback.
    original_theme_name: String,
    /// Cursor index the controller settled on at open time (row 0 when the
    /// persisted selector is not a compiled theme row). Enter without any
    /// navigation commits the original name, never this fallback row.
    opening_cursor: Option<usize>,
    /// Cached UiTheme for `ThemeId::System`, captured once at construction
    /// so the per-frame render doesn't re-invoke `UiTheme::detect()` (which
    /// reads `COLORFGBG`) on every keystroke.
    system_ui_theme: UiTheme,
    /// User-configured background applied on top of every named-theme preview.
    background_override: Option<Color>,
    row_hitboxes: RefCell<Vec<(Rect, usize)>>,
    last_mouse_selected: Option<usize>,
    /// UI locale captured from the app at construction (#4057 wave 2).
    locale: Locale,
    /// Valid user overlays loaded once when the picker opens.
    custom_themes: Vec<codewhale_palette::UserThemeOption>,
}

impl ThemePickerView {
    #[cfg(test)]
    #[must_use]
    pub fn new(original_name: String) -> Self {
        Self::new_with_background(original_name, Locale::En, None)
    }

    fn new_with_background(
        original_name: String,
        locale: Locale,
        background_override: Option<Color>,
    ) -> Self {
        let normalized = original_name.trim().to_ascii_lowercase();
        let (options, custom_themes) = theme_options(&normalized);
        let controller = SettingsPickerController::new(options, normalized.clone());
        let opening_cursor = controller.selected_source_index();
        Self {
            controller,
            original_theme_name: normalized,
            opening_cursor,
            system_ui_theme: UiTheme::detect(),
            background_override,
            row_hitboxes: RefCell::new(Vec::new()),
            last_mouse_selected: None,
            locale,
            custom_themes,
        }
    }

    /// Construct behind type erasure before returning to the async event loop.
    /// Keeping the concrete picker out of that already-large future prevents
    /// transient modal values from inflating the main-thread stack frame.
    #[must_use]
    pub fn boxed(
        original_name: String,
        locale: Locale,
        background_override: Option<Color>,
    ) -> Box<dyn ModalView> {
        Box::new(Self::new_with_background(
            original_name,
            locale,
            background_override,
        ))
    }

    fn selected_theme_name(&self) -> &str {
        self.controller
            .selected_id()
            .unwrap_or(ThemeId::System.name())
    }

    fn custom_theme_for(&self, selector: &str) -> Option<UiTheme> {
        self.custom_themes
            .iter()
            .find(|option| option.selector == selector)
            .map(|option| option.theme)
    }

    #[cfg(test)]
    fn current(&self) -> ThemeId {
        let selected = self.selected_theme_name();
        self.custom_themes
            .iter()
            .find(|option| option.selector == selected)
            .map(|option| option.base)
            .or_else(|| ThemeId::from_name(selected))
            .unwrap_or(ThemeId::System)
    }

    #[cfg(test)]
    fn selected(&self) -> usize {
        self.controller.selected_source_index().unwrap_or(0)
    }

    /// Resolve a theme to a `UiTheme`, returning the cached `System`
    /// resolution to avoid repeated env-var reads inside `render`.
    fn ui_theme_for_selection(&self, selection: &str) -> UiTheme {
        let theme = if let Some(custom) = self.custom_theme_for(selection) {
            custom
        } else if let Some(id) = ThemeId::from_name(selection) {
            if matches!(id, ThemeId::System) {
                self.system_ui_theme
            } else {
                id.ui_theme()
            }
        } else {
            self.system_ui_theme
        };
        self.background_override
            .map_or(theme, |background| theme.with_background_color(background))
    }

    fn preview_event(&self) -> ViewAction {
        ViewAction::Emit(ViewEvent::ThemeSelectionUpdated {
            theme: self.selected_theme_name().to_string(),
            persist: false,
        })
    }

    fn commit_event(&self) -> ViewAction {
        // A commit that never moved the cursor must preserve the exact
        // opening selector. This also protects a custom:<name> selector if
        // its file disappears or becomes invalid while the picker is open.
        if self.controller.selected_source_index() == self.opening_cursor {
            return ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated {
                theme: self.original_theme_name.clone(),
                persist: true,
            });
        }
        ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated {
            theme: self.selected_theme_name().to_string(),
            persist: true,
        })
    }

    fn revert_event(&self) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated {
            theme: self.original_theme_name.clone(),
            persist: false,
        })
    }

    fn action_from_nav(&self, result: PickerNavResult) -> ViewAction {
        match result {
            PickerNavResult::Preview => self.preview_event(),
            PickerNavResult::Commit => self.commit_event(),
            PickerNavResult::Cancel => self.revert_event(),
            PickerNavResult::ItemAction | PickerNavResult::None => ViewAction::None,
        }
    }

    fn move_up(&mut self) -> ViewAction {
        let result = self.controller.move_up();
        self.action_from_nav(result)
    }

    fn move_down(&mut self) -> ViewAction {
        let result = self.controller.move_down();
        self.action_from_nav(result)
    }
}

fn theme_options(
    current_name: &str,
) -> (Vec<SettingOption>, Vec<codewhale_palette::UserThemeOption>) {
    theme_options_with_custom(current_name, codewhale_palette::list_user_theme_options())
}

fn theme_options_with_custom(
    current_name: &str,
    custom_themes: Vec<codewhale_palette::UserThemeOption>,
) -> (Vec<SettingOption>, Vec<codewhale_palette::UserThemeOption>) {
    let current = current_name.trim().to_ascii_lowercase();
    let mut options = SELECTABLE_THEMES
        .iter()
        .copied()
        .map(|id| {
            let name = id.name();
            SettingOption::builder(name, id.display_name())
                .summary(id.tagline())
                .detail(id.tagline())
                .help("Pick a theme with live preview")
                .values(SettingValues::new(
                    Cow::Owned(current.clone()),
                    // Reset uses the same default as fresh terminal settings.
                    Cow::Borrowed(DEFAULT_TUI_THEME),
                    Cow::Borrowed(name),
                ))
                .availability(SettingAvailability::Available)
                .tab("themes")
                .prefer_list_when_narrow(true)
                .build()
        })
        .collect::<Vec<_>>();
    for custom in &custom_themes {
        let label = custom
            .selector
            .strip_prefix(codewhale_palette::USER_THEME_PREFIX)
            .map_or_else(|| custom.selector.clone(), |slug| format!("Custom: {slug}"));
        options.push(
            SettingOption::builder(custom.selector.clone(), label)
                .summary(format!(
                    "User overlay · based on {}",
                    custom.base.display_name()
                ))
                .detail(format!(
                    "User-authored overlay based on {}",
                    custom.base.display_name()
                ))
                .help("Pick a user-authored theme overlay")
                .values(SettingValues::new(
                    Cow::Owned(current.clone()),
                    Cow::Borrowed(DEFAULT_TUI_THEME),
                    Cow::Owned(custom.selector.clone()),
                ))
                .availability(SettingAvailability::Available)
                .tab("themes")
                .prefer_list_when_narrow(true)
                .build(),
        );
    }
    (options, custom_themes)
}

impl ModalView for ThemePickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::ThemePicker
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::Moved => {
                // Hover-follow with live preview: the pointer highlights a
                // row exactly like ↑/↓ does, so the surface behind the modal
                // repaints on hover and a later Enter persists the hovered
                // theme. Returning the preview event (not None) is what makes
                // the highlight repaint immediately.
                let hovered = self.row_hitboxes.borrow().iter().find_map(|(rect, idx)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*idx)
                });
                match hovered {
                    Some(idx) if self.controller.selected_source_index() != Some(idx) => {
                        let nav = self.controller.select_source_index(idx);
                        self.action_from_nav(nav)
                    }
                    _ => ViewAction::None,
                }
            }
            MouseEventKind::ScrollUp => {
                self.last_mouse_selected = None;
                self.move_up()
            }
            MouseEventKind::ScrollDown => {
                self.last_mouse_selected = None;
                self.move_down()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().find_map(|(rect, idx)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*idx)
                });
                if let Some(idx) = clicked {
                    let commit = self.last_mouse_selected == Some(idx)
                        && self.controller.selected_source_index() == Some(idx);
                    let nav = self.controller.select_source_index(idx);
                    self.last_mouse_selected = Some(idx);
                    if commit {
                        self.commit_event()
                    } else {
                        self.action_from_nav(nav)
                    }
                } else {
                    ViewAction::None
                }
            }
            _ => ViewAction::None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Theme picker keeps digit-jump / vim keys; search typing stays off so
        // `j`/`k` and `1`..=`9` retain their navigation meaning.
        let result = handle_nav_key(&mut self.controller, key, false);
        self.action_from_nav(result)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.row_hitboxes.borrow_mut().clear();
        // The live selection has already been swapped under us via
        // ThemeSelectionUpdated.
        // so we pull the *current* preview's UiTheme from the cursor row to
        // skin the modal chrome. That way the popup itself shifts color as
        // the cursor moves, matching what the background will look like
        // after Enter. We keep the live `surface_bg` (not the shared ink) and
        // the bare `Clear` so the preview backdrop reads as intended.
        let live = self.ui_theme_for_selection(self.selected_theme_name());
        let inner =
            render_underwater_surface(area, buf, tr(self.locale, MessageId::ThemeSurfaceTitle));

        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("↑/↓", "preview"),
                ActionHint::new("Enter", "save"),
                ActionHint::new("Esc", "revert"),
            ],
        );

        // Theme rows prefer list-when-narrow; layout still drives scroll math.
        let _layout = SettingsPickerLayout::resolve(content, 34, self.controller.selected_option());

        let mut lines: Vec<Line> = Vec::with_capacity(self.controller.visible().len() + 2);
        lines.push(Line::from(""));

        let header_rows = lines.len();
        let visible_rows = usize::from(content.height)
            .saturating_sub(header_rows)
            .max(1);
        let source_count = self.controller.visible().len();
        let number_width = source_count.to_string().len();
        let selected_visible = self.controller.selected_visible();
        let max_start = source_count.saturating_sub(visible_rows);
        let start = selected_visible
            .saturating_sub(visible_rows.saturating_sub(1))
            .min(max_start);
        let content = render_panel_scroll_rail(
            content,
            buf,
            source_count.saturating_add(header_rows),
            start,
            visible_rows,
            true,
        );

        for (visible_idx, &source_idx) in self
            .controller
            .visible()
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_rows)
        {
            let row_y = content.y.saturating_add(lines.len() as u16);
            self.row_hitboxes
                .borrow_mut()
                .push((Rect::new(content.x, row_y, content.width, 1), source_idx));
            let option = self
                .controller
                .options()
                .get(source_idx)
                .expect("visible source index must reference an option");
            let is_selected = visible_idx == selected_visible;
            let row_style = if is_selected {
                menu_style::theme_selected_row_style(&live)
            } else {
                Style::default().fg(live.text_body)
            };
            let tagline_style = if is_selected {
                Style::default().fg(live.text_muted).bg(live.selection_bg)
            } else {
                Style::default().fg(live.text_dim)
            };
            let number_style = if is_selected {
                Style::default()
                    .fg(live.status_working)
                    .bg(live.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(live.text_hint)
            };
            let pointer = codewhale_tui_kit::glyphs::selection_marker(is_selected);

            // 3-cell color swatch per row using the candidate theme's own
            // accent + panel + border colors so the picker doubles as a
            // legend. The underwater row shows its water column; use the
            // cached resolver so `System` doesn't repeat `UiTheme::detect()`.
            let row_theme = self.ui_theme_for_selection(option.id.as_ref());
            let swatch_colors = match crate::tui::ocean::OceanRamp::for_theme(&row_theme) {
                Some(ramp) => [
                    ramp.surface,
                    ramp.middle,
                    ramp.deep,
                    ramp.ambient,
                    row_theme.status_working,
                ],
                None => [
                    row_theme.surface_bg,
                    row_theme.panel_bg,
                    row_theme.status_working,
                    row_theme.mode_yolo,
                    row_theme.mode_plan,
                ],
            };
            let swatch = swatch_colors
                .into_iter()
                .map(|color| Span::styled("  ", Style::default().bg(color)));

            let mut spans: Vec<Span> = Vec::with_capacity(8);
            spans.push(Span::styled(format!(" {pointer} "), row_style));
            spans.push(Span::styled(
                format!("{:>number_width$}. ", visible_idx + 1),
                number_style,
            ));
            spans.push(Span::styled(format!("{:<22}", option.label), row_style));
            spans.extend(swatch);
            spans.push(Span::raw("  "));

            let prefix_width = Line::from(spans.clone()).width();
            let tagline = codewhale_tui_kit::ui_text::semantic_truncate(
                option.summary.as_ref(),
                usize::from(content.width).saturating_sub(prefix_width),
            );
            spans.push(Span::styled(tagline, tagline_style));

            lines.push(Line::from(spans));
        }

        Paragraph::new(lines).render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn selected_values(action: &ViewAction) -> Option<(&str, bool)> {
        match action {
            ViewAction::Emit(ViewEvent::ThemeSelectionUpdated { theme, persist })
            | ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated { theme, persist }) => {
                Some((theme.as_str(), *persist))
            }
            _ => None,
        }
    }

    fn selected_name(action: &ViewAction) -> Option<&str> {
        selected_values(action).map(|(theme, _)| theme)
    }

    #[test]
    fn opens_at_persisted_theme() {
        let v = ThemePickerView::new("tokyo-night".to_string());
        assert_eq!(v.current(), ThemeId::TokyoNight);
    }

    #[test]
    fn unknown_persisted_name_falls_back_to_first_row() {
        let v = ThemePickerView::new("not-a-real-theme".to_string());
        assert_eq!(v.selected(), 0);
        assert_eq!(v.current(), ThemeId::System);
    }

    #[test]
    fn arrow_down_previews_next_theme() {
        let mut v = ThemePickerView::new("system".to_string());
        let action = v.handle_key(key(KeyCode::Down));
        assert!(matches!(action, ViewAction::Emit(_)));
        assert_eq!(selected_name(&action), Some(ThemeId::Terminal.name()));
        assert_eq!(selected_values(&action), Some(("terminal", false)));
    }

    #[test]
    fn mouse_wheel_previews_and_second_underwater_click_commits() {
        let mut v = ThemePickerView::new("system".to_string());
        let wheel = v.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(wheel, ViewAction::Emit(_)));
        assert_eq!(selected_name(&wheel), Some(ThemeId::Terminal.name()));

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        v.render(area, &mut buf);
        let underwater_source = v
            .controller
            .options()
            .iter()
            .position(|option| option.id.as_ref() == ThemeId::Underwater.name())
            .expect("Underwater row");
        let (rect, idx) = v
            .row_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, source)| *source == underwater_source)
            .expect("rendered Underwater hitbox");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        let preview = v.handle_mouse(click);
        assert!(matches!(preview, ViewAction::Emit(_)));
        assert_eq!(v.selected(), idx);
        assert_eq!(selected_values(&preview), Some(("underwater", false)));
        let commit = v.handle_mouse(click);
        assert!(matches!(commit, ViewAction::EmitAndClose(_)));
        assert_eq!(selected_values(&commit), Some(("underwater", true)));
    }

    #[test]
    fn hover_moves_highlight_and_previews_without_persisting() {
        let mut v = ThemePickerView::new("system".to_string());
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        v.render(area, &mut buf);
        let underwater_source = v
            .controller
            .options()
            .iter()
            .position(|option| option.id.as_ref() == ThemeId::Underwater.name())
            .expect("Underwater row");
        let (rect, idx) = v
            .row_hitboxes
            .borrow()
            .iter()
            .copied()
            .find(|(_, source)| *source == underwater_source)
            .expect("rendered Underwater hitbox");
        let hover = MouseEvent {
            kind: MouseEventKind::Moved,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        };
        // Hovering a new row highlights it and previews (persist:false),
        // exactly like keyboard navigation.
        let action = v.handle_mouse(hover);
        assert!(matches!(action, ViewAction::Emit(_)));
        assert_eq!(selected_values(&action), Some(("underwater", false)));
        assert_eq!(v.selected(), idx);
        // Hovering the already-highlighted row is a no-op.
        assert!(matches!(v.handle_mouse(hover), ViewAction::None));
        // Hovering outside every row is a no-op.
        let outside = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 99,
            row: 29,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(v.handle_mouse(outside), ViewAction::None));
    }

    #[test]
    fn arrow_navigation_wraps_at_picker_edges() {
        let mut v = ThemePickerView::new("system".to_string());
        let last = SELECTABLE_THEMES.last().unwrap();

        let action = v.handle_key(key(KeyCode::Up));
        assert_eq!(selected_name(&action), Some(last.name()));

        let action = v.handle_key(key(KeyCode::Down));
        assert_eq!(selected_name(&action), Some(SELECTABLE_THEMES[0].name()));
    }

    #[test]
    fn enter_commits_with_persist_true() {
        let mut v = ThemePickerView::new("system".to_string());
        v.handle_key(key(KeyCode::Char('9'))); // -> Grayscale
        let action = v.handle_key(key(KeyCode::Enter));
        match action {
            ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated { theme, persist }) => {
                assert_eq!(theme, ThemeId::Grayscale.name());
                assert!(persist);
            }
            other => panic!("expected commit, got {other:?}"),
        }
    }

    #[test]
    fn enter_without_navigating_preserves_a_custom_theme_selector() {
        // The picker's rows are compiled themes only; a persisted
        // custom:<name> selector opens on no row, and Enter without
        // navigation must not replace it with a compiled row.
        let mut v = ThemePickerView::new("custom:midnight".to_string());
        let action = v.handle_key(key(KeyCode::Enter));
        assert_eq!(
            selected_values(&action),
            Some(("custom:midnight", true)),
            "committing without moving the cursor must not replace the persisted selector"
        );
    }

    #[test]
    fn custom_theme_rows_preview_and_commit_their_selector() {
        let mut custom_theme = ThemeId::Whale.ui_theme();
        custom_theme.accent_primary = Color::Rgb(0x12, 0x34, 0x56);
        let custom = codewhale_palette::UserThemeOption {
            selector: "custom:midnight".to_string(),
            base: ThemeId::Whale,
            theme: custom_theme,
        };
        let (options, custom_themes) = theme_options_with_custom("custom:midnight", vec![custom]);
        let controller = SettingsPickerController::new(options, "custom:midnight");
        let view = ThemePickerView {
            opening_cursor: controller.selected_source_index(),
            controller,
            original_theme_name: "custom:midnight".to_string(),
            system_ui_theme: UiTheme::detect(),
            background_override: None,
            row_hitboxes: RefCell::new(Vec::new()),
            last_mouse_selected: None,
            locale: Locale::En,
            custom_themes,
        };

        assert_eq!(view.controller.selected_id(), Some("custom:midnight"));
        assert_eq!(view.current(), ThemeId::Whale);
        assert_eq!(
            view.ui_theme_for_selection("custom:midnight")
                .accent_primary,
            Color::Rgb(0x12, 0x34, 0x56)
        );
        assert_eq!(
            selected_values(&view.preview_event()),
            Some(("custom:midnight", false))
        );
        assert_eq!(
            selected_values(&view.commit_event()),
            Some(("custom:midnight", true))
        );
    }

    #[test]
    fn enter_after_navigating_away_still_commits_the_chosen_option() {
        let mut v = ThemePickerView::new("dracula".to_string());
        v.handle_key(key(KeyCode::Down));
        let action = v.handle_key(key(KeyCode::Enter));
        let (theme, persist) = selected_values(&action).expect("expected a commit");
        assert_ne!(
            theme, "dracula",
            "navigation should have moved off the opening row"
        );
        assert!(persist);
    }

    #[test]
    fn esc_reverts_to_exact_original_theme() {
        let mut v = ThemePickerView::new("dracula".to_string());
        v.handle_key(key(KeyCode::Up));
        v.handle_key(key(KeyCode::Up));
        let action = v.handle_key(key(KeyCode::Esc));
        match action {
            ViewAction::EmitAndClose(ViewEvent::ThemeSelectionUpdated { theme, persist }) => {
                assert_eq!(theme, "dracula");
                assert!(!persist);
            }
            other => panic!("expected revert, got {other:?}"),
        }
    }

    #[test]
    fn digit_jumps_to_shoreline_and_previews() {
        let mut v = ThemePickerView::new("system".to_string());
        let action = v.handle_key(key(KeyCode::Char('3')));
        // Shoreline follows System and Terminal.
        assert_eq!(selected_values(&action), Some(("shoreline", false)));
    }

    #[test]
    fn digit_zero_is_rejected_not_remapped_to_row_zero() {
        let mut v = ThemePickerView::new("dracula".to_string());
        let before = v.selected();
        let action = v.handle_key(key(KeyCode::Char('0')));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(v.selected(), before, "'0' should not move the cursor");
    }

    #[test]
    fn render_does_not_panic_on_zero_sized_area() {
        // The picker historically panicked here via .max(W).max(H) floors
        // that produced dimensions larger than the available area, then
        // underflowed the centering arithmetic.
        let v = ThemePickerView::new("system".to_string());
        let outer = ratatui::layout::Rect::new(0, 0, 10, 10);
        let area = ratatui::layout::Rect::new(0, 0, 0, 0);
        let mut buf = ratatui::buffer::Buffer::empty(outer);
        v.render(area, &mut buf);
    }

    #[test]
    fn render_does_not_panic_on_tiny_area() {
        // 20×6 is smaller than every soft floor the picker prefers.
        let v = ThemePickerView::new("system".to_string());
        let area = ratatui::layout::Rect::new(0, 0, 20, 6);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        v.render(area, &mut buf);
    }

    #[test]
    fn every_selectable_theme_previews_and_renders_through_the_same_surface() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 32);
        let mut view = ThemePickerView::new("system".to_string());

        for expected in SELECTABLE_THEMES.iter().copied() {
            let index = view
                .controller
                .options()
                .iter()
                .position(|option| option.id.as_ref() == expected.name())
                .expect("selectable theme option");
            let _ = view.controller.select_source_index(index);
            assert_eq!(view.current(), expected);
            assert_eq!(
                selected_values(&view.preview_event()),
                Some((expected.name(), false))
            );

            let mut buf = ratatui::buffer::Buffer::empty(area);
            view.render(area, &mut buf);
            let text = buf
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                text.contains(expected.display_name()),
                "{} was not represented in its live preview surface",
                expected.name()
            );
            assert!(text.contains("Enter save"));
        }
    }

    #[test]
    fn render_semantically_truncates_taglines_at_narrow_width() {
        let v = ThemePickerView::new("system".to_string());
        let area = ratatui::layout::Rect::new(0, 0, 56, 12);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        v.render(area, &mut buf);
        let rows = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let text = rows.join("\n");

        assert!(text.contains('…'), "{text}");
        for (idx, row) in rows.iter().enumerate() {
            assert!(
                codewhale_tui_kit::ui_text::text_display_width(row) <= usize::from(area.width),
                "line {idx} overflows: {row:?}"
            );
        }
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    #[test]
    fn theme_picker_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        use ratatui::{buffer::Buffer, layout::Rect};
        use unicode_width::UnicodeWidthStr;

        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(ThemePickerView::new("system".to_string()));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            for label in ["preview", "save", "revert"] {
                assert!(text.contains(label), "{w}x{h}: missing footer '{label}'");
            }
            assert!(
                !text.contains('X'),
                "{w}x{h}: background bleed-through into modal surface"
            );
            // The theme picker paints the *live* theme surface (not the shared
            // ink), so assert the center cell is painted (no surviving
            // sentinel) rather than checking a fixed background color.
            assert_ne!(
                buf[(w / 2, h / 2)].symbol(),
                "X",
                "{w}x{h}: modal interior must be painted"
            );
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn theme_picker_uses_shared_settings_controller() {
        let v = ThemePickerView::new("dracula".to_string());
        assert_eq!(v.controller.original_id(), "dracula");
        assert_eq!(v.controller.selected_id(), Some("dracula"));
        // One row per selectable theme: no modifier rows beside them.
        assert_eq!(v.controller.visible().len(), SELECTABLE_THEMES.len());
    }

    /// Goldens are stored without cell padding: every row is right-trimmed
    /// and trailing empty rows are dropped, so `git diff --check` stays
    /// clean.
    fn trim_golden_rows(text: &str) -> String {
        let mut rows: Vec<&str> = text.lines().map(str::trim_end).collect();
        while rows.last().is_some_and(|row| row.is_empty()) {
            rows.pop();
        }
        let mut out = rows.join("\n");
        out.push('\n');
        out
    }

    /// Slice C: cell-exact goldens for the picker surface with the default
    /// theme selected — 14 rows plus the preview footer. A visual change
    /// that cannot show as a golden diff did not happen. Re-bless with
    /// `CODEWHALE_BLESS_GOLDENS=1`.
    #[test]
    fn theme_picker_matches_goldens_at_blocker_sizes() {
        use crate::tui::golden_harness::{assert_matches_golden, render_golden_text};
        for (w, h) in [(80u16, 24u16), (120u16, 32u16)] {
            let rendered = render_golden_text(w, h, |buf| {
                ThemePickerView::new("underwater".to_string()).render(Rect::new(0, 0, w, h), buf);
            });
            assert_matches_golden(
                &format!("theme_picker_{w}x{h}"),
                &trim_golden_rows(&rendered),
            );
        }
    }
}

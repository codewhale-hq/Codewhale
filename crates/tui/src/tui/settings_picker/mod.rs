//! Shared settings-picker framework.
//!
//! # Contract
//!
//! Concrete pickers (theme, model, provider, config) sit *on top* of this
//! module. The framework owns:
//!
//! - option catalog + tab/search filtering with stable visible indices
//! - keyboard navigation (↑/↓/Home/End/digits), disabled rows with reasons
//! - optional per-item actions
//! - nav-level preview / commit / cancel lifecycle via [`PickerNavResult`]
//! - responsive list↔detail layout (side-by-side when wide; stacked or
//!   list-only narrow fallback per option)
//!
//! Ocean chrome (swatches, underwater surface paint, locale strings) stays in
//! the concrete picker so shared *contracts* do not flatten visual character.
//!
//! # Integration hooks (model / provider / Fleet)
//!
//! - **Theme**: nav/layout migrated — [`crate::tui::theme_picker`] builds
//!   options and drives [`SettingsPickerController`] for navigation. Theme
//!   preview/revert flows through its existing `ViewAction` path; hosts map
//!   [`PickerNavResult`] into their own actions.
//! - **Model / provider**: leave full migration to the TUI-DOG-009 sibling.
//!   Call `SettingsPickerController::new(options, original_id)` and map
//!   [`PickerNavResult`] into existing `ViewAction`s; reuse
//!   [`SettingsPickerLayout::resolve`] instead of ad-hoc splits.
//! - **Fleet setup**: framework only — billing/Fleet UX sibling owns flow
//!   rewrites; plug drafts into the controller when ready.
//!
//! See `docs/SETTINGS_PICKER_FRAMEWORK.md` for the short integration note.

pub mod controller;
pub mod layout;
pub mod option;

pub use controller::{PickerNavResult, SettingsPickerController};
pub use layout::SettingsPickerLayout;
#[allow(unused_imports)] // public API surface for host pickers
pub use option::{
    SettingAvailability, SettingItemAction, SettingOption, SettingOptionBuilder, SettingValues,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Map a key event onto the shared picker navigation contract.
///
/// Search typing (`Char` that is not a digit shortcut / vim key) is left to
/// the host when `allow_search_typing` is true so theme-style digit jumps stay
/// intact for non-search pickers.
pub fn handle_nav_key(
    controller: &mut SettingsPickerController,
    key: KeyEvent,
    allow_search_typing: bool,
) -> PickerNavResult {
    match key.code {
        KeyCode::Esc => controller.request_cancel(),
        KeyCode::Enter => controller.request_commit(),
        // A tab switch lands on whatever row the new tab keeps selected; it
        // previews that row only when it is available, like ↑/↓ do.
        KeyCode::BackTab => controller.prev_tab(),
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => controller.prev_tab(),
        KeyCode::Tab => controller.next_tab(),
        KeyCode::Up
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            controller.move_up()
        }
        KeyCode::Char('k')
            if !allow_search_typing
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            controller.move_up()
        }
        KeyCode::Down
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            controller.move_down()
        }
        KeyCode::Char('j')
            if !allow_search_typing
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            controller.move_down()
        }
        KeyCode::Home => controller.jump_home(),
        KeyCode::End => controller.jump_end(),
        KeyCode::Backspace if allow_search_typing => {
            controller.pop_query_char();
            PickerNavResult::None
        }
        KeyCode::Char('u')
            if key.modifiers.contains(KeyModifiers::CONTROL) && allow_search_typing =>
        {
            controller.clear_query();
            PickerNavResult::None
        }
        KeyCode::Char(c)
            if allow_search_typing
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && !c.is_control() =>
        {
            controller.push_query_char(c);
            PickerNavResult::None
        }
        KeyCode::Char(c)
            if matches!(c, '1'..='9')
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            controller.jump_digit(c as u8 - b'0')
        }
        KeyCode::Char(' ') => controller.request_item_action(),
        _ => PickerNavResult::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use std::borrow::Cow;

    fn sample_options() -> Vec<SettingOption> {
        vec![
            SettingOption::builder("system", "System")
                .summary("Follow the terminal")
                .detail("System resolves from COLORFGBG at session start.")
                .help("Default theme selection")
                .values(SettingValues::new(
                    Cow::Borrowed("system"),
                    Cow::Borrowed("system"),
                    Cow::Borrowed("system"),
                ))
                .tab("core")
                .build(),
            SettingOption::builder("terminal", "Terminal")
                .summary("Terminal-owned background")
                .detail("Terminal owns the background; Deepsea is unavailable.")
                .help("No painted ocean field")
                .values(SettingValues::new(
                    Cow::Borrowed("terminal"),
                    Cow::Borrowed("system"),
                    Cow::Borrowed("terminal"),
                ))
                .tab("core")
                .build(),
            SettingOption::builder("locked", "Locked Theme")
                .summary("Unavailable in this build")
                .detail("Disabled for matrix coverage.")
                .help("Shows disabled reason in detail")
                .values(SettingValues::new(
                    Cow::Borrowed("locked"),
                    Cow::Borrowed("system"),
                    Cow::Borrowed("system"),
                ))
                .availability(SettingAvailability::Disabled {
                    reason: Cow::Borrowed("requires fancy_animations"),
                })
                .tab("extra")
                .prefer_list_when_narrow(true)
                .build(),
            SettingOption::builder("dracula", "Dracula")
                .summary("Purple night")
                .detail("Classic Dracula palette.")
                .help("Popular dark theme")
                .values(SettingValues::new(
                    Cow::Borrowed("dracula"),
                    Cow::Borrowed("system"),
                    Cow::Borrowed("dracula"),
                ))
                .tab("extra")
                .action(SettingItemAction {
                    id: Cow::Borrowed("swatch"),
                    label: Cow::Borrowed("Show swatch"),
                })
                .build(),
        ]
    }

    fn matrix_snapshot(controller: &SettingsPickerController, area: Rect) -> String {
        let focused = controller.selected_option();
        let layout = SettingsPickerLayout::resolve(area, 34, focused);
        let mut lines = Vec::new();
        lines.push(format!(
            "tab={} query={:?} selected={:?} visible={} narrow={} stacked={} detail={}",
            controller.active_tab_name(),
            controller.query(),
            controller.selected_id(),
            controller.visible().len(),
            layout.narrow,
            layout.stacked,
            layout.detail.is_some(),
        ));
        for (visible_idx, &source) in controller.visible().iter().enumerate() {
            let option = &controller.options()[source];
            let marker = if visible_idx == controller.selected_visible() {
                ">"
            } else {
                " "
            };
            let disabled = option
                .availability
                .disabled_reason()
                .map(|reason| format!(" [disabled: {reason}]"))
                .unwrap_or_default();
            lines.push(format!(
                "{marker}{}. {} ({}){}",
                visible_idx + 1,
                option.label,
                option.id,
                disabled
            ));
        }
        if let Some(option) = focused {
            lines.push(format!(
                "detail: current={} default={} effective={}",
                option.values.current, option.values.default, option.values.effective
            ));
            lines.push(format!("help: {}", option.help));
            if let Some(reason) = option.availability.disabled_reason() {
                lines.push(format!("reason: {reason}"));
            }
        }
        lines.join("\n")
    }

    #[test]
    fn matrix_normal_layout_is_side_by_side() {
        let controller = SettingsPickerController::new(sample_options(), "system");
        let snap = matrix_snapshot(&controller, Rect::new(0, 0, 120, 30));
        assert!(snap.contains("narrow=false"));
        assert!(snap.contains("detail=true"));
        assert!(snap.contains(">1. System (system)"));
        assert!(snap.contains("detail: current=system"));
    }

    #[test]
    fn matrix_narrow_falls_back_to_list_only_when_preferred() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        // Move to locked which prefers list-when-narrow (on the extra tab).
        controller.set_active_tab(
            controller
                .tabs()
                .iter()
                .position(|tab| tab == "extra")
                .expect("extra tab"),
        );
        let _ = controller.jump_home();
        let snap = matrix_snapshot(&controller, Rect::new(0, 0, 60, 16));
        assert!(snap.contains("narrow=true"));
        assert!(
            snap.contains("detail=false"),
            "narrow + prefer_list should drop detail: {snap}"
        );
    }

    #[test]
    fn matrix_disabled_row_blocks_preview_and_commit() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        controller.set_query("locked");
        assert_eq!(controller.visible().len(), 1);
        assert_eq!(controller.move_down(), PickerNavResult::None);
        assert_eq!(controller.request_commit(), PickerNavResult::None);
        let snap = matrix_snapshot(&controller, Rect::new(0, 0, 100, 24));
        assert!(snap.contains("[disabled: requires fancy_animations]"));
        assert!(snap.contains("reason: requires fancy_animations"));
    }

    #[test]
    fn matrix_filtered_preserves_selection_identity() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        assert_eq!(controller.move_down(), PickerNavResult::Preview);
        assert_eq!(controller.selected_id(), Some("terminal"));
        // Specific enough that the System row's "terminal" summary does not
        // also match — we want identity preservation on a single hit.
        controller.set_query("owns the background");
        assert_eq!(controller.selected_id(), Some("terminal"));
        assert_eq!(controller.visible().len(), 1);
        let snap = matrix_snapshot(&controller, Rect::new(0, 0, 100, 24));
        assert!(snap.contains("query=\"owns the background\""));
        assert!(snap.contains(">1. Terminal (terminal)"));
    }

    #[test]
    fn matrix_preview_commit_and_revert_sequence() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");

        let preview = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            false,
        );
        assert_eq!(preview, PickerNavResult::Preview);
        assert_eq!(controller.selected_id(), Some("terminal"));

        let commit = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            false,
        );
        assert_eq!(commit, PickerNavResult::Commit);
        assert_eq!(controller.selected_id(), Some("terminal"));

        // Re-open semantics: cancel restores the original id via rollback.
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        let _ = controller.move_down();
        let cancel = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            false,
        );
        assert_eq!(cancel, PickerNavResult::Cancel);
        assert_eq!(controller.original_id(), "system");
    }

    #[test]
    fn digit_zero_does_not_jump() {
        let mut controller = SettingsPickerController::new(sample_options(), "dracula");
        let before = controller.selected_id().map(str::to_string);
        let result = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
            false,
        );
        assert_eq!(result, PickerNavResult::None);
        assert_eq!(controller.selected_id().map(str::to_string), before);
    }

    #[test]
    fn backtab_uses_the_same_previous_tab_path_as_shift_tab() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        assert_eq!(controller.active_tab(), 0);

        let result = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            false,
        );

        // U09-m2: the last ("extra") tab lands on its disabled first row,
        // which must not preview.
        assert_eq!(controller.active_tab(), controller.tabs().len() - 1);
        assert_eq!(controller.selected_id(), Some("locked"));
        assert_eq!(result, PickerNavResult::None);

        assert_eq!(controller.move_down(), PickerNavResult::Preview);
        assert_eq!(controller.selected_id(), Some("dracula"));
        let result = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
            false,
        );
        assert_eq!(controller.active_tab_name(), "core");
        assert_eq!(controller.selected_id(), Some("terminal"));
        assert_eq!(
            result,
            PickerNavResult::Preview,
            "an available row previews"
        );
    }

    #[test]
    fn item_action_fires_on_space() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        controller.set_query("dracula");
        let result = handle_nav_key(
            &mut controller,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            false,
        );
        assert_eq!(result, PickerNavResult::ItemAction);
        let option = controller.selected_option().expect("dracula selected");
        assert_eq!(option.id, "dracula");
        assert_eq!(
            option.action.as_ref().map(|action| action.id.as_ref()),
            Some("swatch")
        );
    }

    #[test]
    fn allow_search_typing_accepts_j_k_and_digits_into_query() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        for ch in ['j', 'k', '1', '9'] {
            let result = handle_nav_key(
                &mut controller,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                true,
            );
            assert_eq!(result, PickerNavResult::None);
        }
        assert_eq!(controller.query(), "jk19");
    }

    #[test]
    fn tab_match_counts_tracks_matches_per_tab() {
        let mut controller = SettingsPickerController::new(sample_options(), "system");
        assert_eq!(
            controller.tab_match_counts(),
            vec![
                ("all".to_string(), 4),
                ("core".to_string(), 2),
                ("extra".to_string(), 2),
            ]
        );
        controller.set_query("dracula");
        assert_eq!(
            controller.tab_match_counts(),
            vec![
                ("all".to_string(), 1),
                ("core".to_string(), 0),
                ("extra".to_string(), 1),
            ]
        );
    }
}

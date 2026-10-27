//! Typed work-surface interaction ownership (TUI-DOG-004 / 005 / 006).
//!
//! Selection, focus, and detail-open are distinct axes. Destructive lifecycle
//! actions live inside the inspector pager, not in compact rows.

use crate::tui::app::{App, SidebarRowAction};
use crate::tui::views::ModalKind;

use super::model::{RailPanel, WorkRowId};

pub(crate) fn select_dock_panel(app: &mut App, panel: RailPanel) {
    if panel == RailPanel::Terminal {
        app.launch.dismiss();
    }
    app.work_surface.panel = panel;
    app.work_surface.explicit_view = true;
    app.work_surface.dismissed = false;
    app.work_surface.focused = true;
    app.work_surface.scroll_offset = 0;
    app.work_surface.selected = None;
    app.needs_redraw = true;
}

/// Esc / the `×` tab: close the view. An explicitly opened view goes back to
/// the auto rule; the dock stays down until new work arrives.
pub(crate) fn dismiss_dock(app: &mut App) {
    app.work_surface.explicit_view = false;
    app.work_surface.dismissed = true;
    // Measure the view the auto rule will show next, not the one just
    // closed: the dock re-opens when *that* view grows.
    super::model::resolve_view(app);
    app.work_surface.dismissed_view = app.work_surface.panel;
    app.work_surface.dismissed_at_rows = super::model::auto_work_rows(app);
    app.work_surface.focused = false;
    app.work_surface.selected = None;
    app.needs_redraw = true;
}

/// Claim work-surface focus and clear competing selection owners.
pub fn claim_focus(app: &mut App) {
    let was_focused = app.work_surface.focused;
    app.work_surface.focused = true;
    crate::tui::mouse_ui::clear_transcript_selection(app);
    if !was_focused {
        app.needs_redraw = true;
    }
}

/// Release work-surface focus without clearing the remembered selection.
pub fn release_focus(app: &mut App) {
    if !app.work_surface.focused && app.work_surface.hovered.is_none() {
        return;
    }
    app.work_surface.focused = false;
    app.work_surface.hovered = None;
    app.needs_redraw = true;
}

/// Open or toggle-close the primary detail for a row.
///
/// Enter/click on an already-opened selected row closes it. Opening a different
/// row updates the inspector owner.
pub fn activate_primary(
    app: &mut App,
    row_id: &WorkRowId,
    primary: Option<SidebarRowAction>,
) -> Option<SidebarRowAction> {
    if app.work_surface.opened.as_ref() == Some(row_id) {
        // Toggle-close only while the detail is actually on screen. When the
        // pager closed itself (q/Esc inside it), `opened` is a stale owner —
        // swallowing the click here would make the row look dead, so fall
        // through and reopen instead.
        let detail_on_screen = app.view_stack.top_kind() == Some(ModalKind::Pager);
        close_opened(app);
        if detail_on_screen {
            return None;
        }
    }
    app.work_surface.selected = Some(row_id.clone());
    let action = primary?;
    // A group heading changes the visible panel but does not open a modal.
    // Keep `opened` reserved for a real detail owner, otherwise a later
    // activation treats the still-visible heading as a stale pager toggle.
    if !matches!(action, SidebarRowAction::ShowSubagentsPanel) {
        app.work_surface.opened = Some(row_id.clone());
    }
    Some(action)
}

/// Whether the work-surface-owned detail is actually on screen. A row action
/// that runs a command without pushing a view (the Context panel's `/compact`
/// row) still records `opened`; with nothing stacked above the dock that owner
/// is stale, and Esc belongs to the dock again (#6502).
pub(crate) fn opened_detail_on_screen(app: &App) -> bool {
    app.work_surface.opened.is_some() && !app.view_stack.is_empty()
}

/// Close the work-surface-owned detail (pager when we opened it).
pub fn close_opened(app: &mut App) {
    if app.work_surface.opened.take().is_none() {
        return;
    }
    if app.view_stack.top_kind() == Some(ModalKind::Pager) {
        app.view_stack.pop();
    }
    app.needs_redraw = true;
}

/// Release a closed Agent Details owner without disturbing Work selection.
/// The modal has already popped itself before this event is handled.
pub(crate) fn agent_details_closed(app: &mut App, agent_id: &str) {
    let owner = WorkRowId(format!("worker:{agent_id}"));
    if app.work_surface.opened.as_ref() == Some(&owner) {
        app.work_surface.opened = None;
        app.needs_redraw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn app() -> App {
        let options = TuiOptions {
            use_mouse_capture: true,
            max_subagents: 4,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn primary_toggles_opened_closed() {
        let mut app = app();
        let row = WorkRowId("worker:a1".into());
        let open = SidebarRowAction::OpenAgentDetail {
            agent_id: "a1".into(),
        };
        assert!(activate_primary(&mut app, &row, Some(open.clone())).is_some());
        assert_eq!(app.work_surface.opened.as_ref(), Some(&row));
        // With the detail pager on screen, the second activation toggles it
        // closed; with no pager on screen (it closed itself), the activation
        // reopens instead of going dead.
        app.view_stack.push(crate::tui::pager::PagerView::from_text(
            "Agent".to_string(),
            "body",
            40,
        ));
        assert!(activate_primary(&mut app, &row, Some(open.clone())).is_none());
        assert!(app.work_surface.opened.is_none());
        assert!(activate_primary(&mut app, &row, Some(open.clone())).is_some());
        assert_eq!(app.work_surface.opened.as_ref(), Some(&row));
        // Pager already gone (closed from inside): reopen, don't swallow.
        assert!(activate_primary(&mut app, &row, Some(open)).is_some());
        assert_eq!(app.work_surface.opened.as_ref(), Some(&row));
    }

    #[test]
    fn claim_focus_clears_transcript_selection() {
        use crate::tui::selection::TranscriptSelectionPoint;
        let mut app = app();
        app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        });
        app.viewport.transcript_selection.head = app.viewport.transcript_selection.anchor;
        claim_focus(&mut app);
        assert!(app.work_surface.focused);
        assert!(!app.viewport.transcript_selection.is_active());
        assert!(app.needs_redraw);
    }
}

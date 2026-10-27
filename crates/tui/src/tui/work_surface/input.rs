use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::tui::app::{App, SidebarRowAction};

use super::interaction::{activate_primary, claim_focus, close_opened, release_focus};
use super::model::{
    DockTabTarget, RailPanel, SIDE_WIDTH_MAX, SIDE_WIDTH_MIN, TOP_HEIGHT_MAX, TOP_HEIGHT_MIN,
    WorkRow, WorkRowId, WorkSurfacePlacement, visible_rows_for_panel,
};

#[derive(Debug, Default)]
pub struct MouseOutcome {
    pub consumed: bool,
    pub action: Option<SidebarRowAction>,
}

/// Cycle the bottom view: `Ctrl+Tab` / `Ctrl+]` forward, `Ctrl+Shift+Tab`
/// back. Every view is reachable this way, content or not — an empty view
/// paints its one "nothing here" row rather than skipping, so the order the
/// user learns is the order they get. The choice is explicit until Esc.
pub fn cycle_view(app: &mut App, forward: bool) {
    let next = if forward {
        app.work_surface.panel.next()
    } else {
        app.work_surface.panel.prev()
    };
    super::interaction::select_dock_panel(app, next);
}

/// `← for agents`: switch the rail to the Agents panel and give it keyboard
/// ownership so ↑/↓ + Enter select and focus a worker. Returns `false` when
/// the rail cannot show agents right now (rail off, or nothing to list); the
/// caller then opens the `/agents` register instead so the key still lands.
pub fn enter_agents(app: &mut App) -> bool {
    let previous_panel = app.work_surface.panel;
    app.work_surface.panel = RailPanel::Agents;
    if app.work_surface.placement == WorkSurfacePlacement::Off
        || app.work_surface.effective_placement == WorkSurfacePlacement::Off
        || app.work_surface.last_area.is_none()
    {
        app.work_surface.panel = previous_panel;
        release_focus(app);
        return false;
    }
    let rows = visible_rows_for_panel(app);
    // A live strip is proven by the hitboxes it painted this frame. When the
    // agents view itself was on screen the worker row must be among them;
    // when another view was up (the dock opens on TODO), any painted row
    // proves the strip is live and the switch paints the workers next frame.
    let strip_is_live = !app.work_surface.hitboxes.is_empty();
    let agents_were_painted = previous_panel == RailPanel::Agents;
    let first_agent = rows
        .iter()
        .find(|row| {
            row.selectable
                && row.id.0.starts_with("worker:")
                && if agents_were_painted {
                    app.work_surface
                        .hitboxes
                        .iter()
                        .any(|hitbox| hitbox.id == row.id)
                } else {
                    strip_is_live
                }
        })
        .map(|row| row.id.clone());
    let Some(first_agent) = first_agent else {
        app.work_surface.panel = previous_panel;
        return false;
    };
    super::interaction::select_dock_panel(app, RailPanel::Agents);
    claim_focus(app);
    let selected_agent_is_visible = app.work_surface.selected.as_ref().is_some_and(|selected| {
        rows.iter()
            .any(|row| row.selectable && row.id == *selected && row.id.0.starts_with("worker:"))
            && app
                .work_surface
                .hitboxes
                .iter()
                .any(|hitbox| hitbox.id == *selected)
    });
    if !selected_agent_is_visible {
        app.work_surface.selected = Some(first_agent);
    }
    app.work_surface.clamp_selection(&rows);
    app.needs_redraw = true;
    true
}

/// Handle the work surface's focused keyboard contract. `Alt+W` enters the
/// surface from the composer; Esc returns ownership to the composer (or clears
/// a local stop arm / open detail first). Plain printable input always returns
/// ownership to the composer instead of becoming a hidden panel shortcut.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Option<Option<SidebarRowAction>> {
    // A starved, mini-window-hidden, or explicitly disabled rail owns no
    // cells, so it cannot own keyboard focus. `collapse_strip` normally
    // clears this at layout time; this guard also closes the pre-redraw race.
    if app.work_surface.last_area.is_none() {
        if app.work_surface.focused {
            release_focus(app);
        }
        return None;
    }
    if app.focus() == crate::tui::shell_key_routing::Focus::TerminalPanel {
        return super::terminal::handle_key(app, key).then_some(None);
    }
    if app.work_surface.focused {
        let tabs = app
            .work_surface
            .dock_tabs
            .iter()
            .filter_map(|hitbox| match hitbox.target {
                DockTabTarget::Panel(panel) => Some(panel),
                DockTabTarget::Close => None,
            })
            .collect::<Vec<_>>();
        if !tabs.is_empty() {
            let current = tabs
                .iter()
                .position(|panel| *panel == app.work_surface.panel)
                .unwrap_or(0);
            // Only a bare ←/→ walks the dock tabs. Ctrl/Alt/Super+←/→ are the
            // composer's word-navigation chords; they fall through below and
            // return ownership to the composer like printable input.
            let bare = !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
            let next = match key.code {
                KeyCode::Left if bare => Some((current + tabs.len() - 1) % tabs.len()),
                KeyCode::Right if bare => Some((current + 1) % tabs.len()),
                _ => None,
            };
            if let Some(next) = next {
                super::interaction::select_dock_panel(app, tabs[next]);
                return Some(None);
            }
        }
    }

    let navigation_chord = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
    if (matches!(key.code, KeyCode::Char(_)) && !navigation_chord)
        || (app.work_surface.focused
            && navigation_chord
            && matches!(key.code, KeyCode::Left | KeyCode::Right))
    {
        release_focus(app);
        return None;
    }

    // Keyboard and mouse share one row source per panel: Enter on the
    // selected row must open the same world a click would. An explicitly
    // opened empty view still owns Esc (close) so cycling into "no files
    // touched" is never a trap.
    let rows = visible_rows_for_panel(app);
    if rows.is_empty()
        && !(app.work_surface.explicit_view
            && (app.work_surface.focused || app.work_surface.panel == RailPanel::Terminal))
    {
        return None;
    }
    if !app.work_surface.focused {
        if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::ALT) {
            claim_focus(app);
            app.work_surface.clamp_selection(&rows);
            app.needs_redraw = true;
            return Some(None);
        }
        return None;
    }

    // The details chord opens the selected row's own world; the transcript
    // pager owns ⌥V only when no work row is selected.
    if crate::tui::shell_key_routing::is_tool_details_shortcut(&key) {
        let action = selected_row(app, &rows)
            .and_then(|row| activate_primary(app, &row.id, row.primary_action.clone()));
        if action.is_some() {
            app.work_surface.clamp_selection(&rows);
            app.needs_redraw = true;
            return Some(action);
        }
        return None;
    }

    let action = match key.code {
        KeyCode::Esc => {
            if super::interaction::opened_detail_on_screen(app) {
                close_opened(app);
            } else {
                // A stale owner (the row's command opened no view) must not
                // swallow the Esc the close control advertises.
                app.work_surface.opened = None;
                super::interaction::dismiss_dock(app);
            }
            return Some(None);
        }
        KeyCode::Up => {
            move_selection(app, &rows, -1);
            None
        }
        KeyCode::Down => {
            move_selection(app, &rows, 1);
            None
        }
        KeyCode::Home => {
            select_edge(app, &rows, false);
            None
        }
        KeyCode::End => {
            select_edge(app, &rows, true);
            None
        }
        KeyCode::PageUp => {
            move_selection(app, &rows, -(app.work_surface.visible_rows.max(1) as isize));
            None
        }
        KeyCode::PageDown => {
            move_selection(app, &rows, app.work_surface.visible_rows.max(1) as isize);
            None
        }
        KeyCode::Enter => selected_row(app, &rows)
            .and_then(|row| activate_primary(app, &row.id, row.primary_action.clone())),
        _ => return None,
    };
    app.work_surface.clamp_selection(&rows);
    app.needs_redraw = true;
    Some(action)
}

pub fn handle_mouse(app: &mut App, mouse: MouseEvent) -> MouseOutcome {
    let Some(area) = app.work_surface.last_area else {
        return MouseOutcome::default();
    };
    let placement = app.work_surface.effective_placement;
    let on_divider = match placement {
        WorkSurfacePlacement::Off => false,
        WorkSurfacePlacement::Top => {
            mouse.row == area.bottom().saturating_sub(1)
                && mouse.column >= area.x
                && mouse.column < area.right()
        }
        WorkSurfacePlacement::Bottom => {
            mouse.row == area.y && mouse.column >= area.x && mouse.column < area.right()
        }
        WorkSurfacePlacement::Left => {
            mouse.column == area.right().saturating_sub(1)
                && mouse.row >= area.y
                && mouse.row < area.bottom()
        }
        WorkSurfacePlacement::Right => {
            mouse.column == area.x && mouse.row >= area.y && mouse.row < area.bottom()
        }
    };

    if matches!(mouse.kind, MouseEventKind::Moved) && app.work_surface.divider_hovered != on_divider
    {
        app.work_surface.divider_hovered = on_divider;
        app.needs_redraw = true;
    }

    match mouse.kind {
        MouseEventKind::Moved if on_divider => {
            return MouseOutcome {
                consumed: true,
                action: None,
            };
        }
        MouseEventKind::Down(MouseButton::Left) if on_divider => {
            app.work_surface.resizing = true;
            app.work_surface.divider_hovered = true;
            app.work_surface.resize_anchor_column = mouse.column;
            app.work_surface.resize_anchor_row = mouse.row;
            app.work_surface.resize_anchor_size = match placement {
                WorkSurfacePlacement::Top | WorkSurfacePlacement::Bottom => area.height,
                WorkSurfacePlacement::Left | WorkSurfacePlacement::Right => area.width,
                WorkSurfacePlacement::Off => area.width,
            };
            app.needs_redraw = true;
            return MouseOutcome {
                consumed: true,
                action: None,
            };
        }
        MouseEventKind::Drag(MouseButton::Left) if app.work_surface.resizing => {
            let anchor = i32::from(app.work_surface.resize_anchor_size);
            match placement {
                WorkSurfacePlacement::Top => {
                    let delta =
                        i32::from(mouse.row) - i32::from(app.work_surface.resize_anchor_row);
                    app.work_surface.top_height = (anchor + delta)
                        .clamp(i32::from(TOP_HEIGHT_MIN), i32::from(TOP_HEIGHT_MAX))
                        as u16;
                }
                // The bottom strip's divider is its TOP edge: dragging up
                // grows the strip, so the delta is inverted from Top.
                WorkSurfacePlacement::Bottom => {
                    let delta =
                        i32::from(app.work_surface.resize_anchor_row) - i32::from(mouse.row);
                    app.work_surface.top_height = (anchor + delta)
                        .clamp(i32::from(TOP_HEIGHT_MIN), i32::from(TOP_HEIGHT_MAX))
                        as u16;
                }
                WorkSurfacePlacement::Left => {
                    let delta =
                        i32::from(mouse.column) - i32::from(app.work_surface.resize_anchor_column);
                    app.work_surface.side_width = (anchor + delta)
                        .clamp(i32::from(SIDE_WIDTH_MIN), i32::from(SIDE_WIDTH_MAX))
                        as u16;
                }
                WorkSurfacePlacement::Right => {
                    let delta =
                        i32::from(app.work_surface.resize_anchor_column) - i32::from(mouse.column);
                    app.work_surface.side_width = (anchor + delta)
                        .clamp(i32::from(SIDE_WIDTH_MIN), i32::from(SIDE_WIDTH_MAX))
                        as u16;
                }
                WorkSurfacePlacement::Off => {}
            }
            app.needs_redraw = true;
            return MouseOutcome {
                consumed: true,
                action: None,
            };
        }
        MouseEventKind::Up(MouseButton::Left) if app.work_surface.resizing => {
            app.work_surface.resizing = false;
            app.work_surface.divider_hovered = on_divider;
            let top_height = app.work_surface.top_height;
            let side_width = app.work_surface.side_width;
            if let Err(error) = codewhale_runtime::settings::Settings::transact(|settings| {
                settings.work_surface_top_height = top_height;
                settings.work_surface_side_width = side_width;
                Ok(())
            }) {
                app.status_message =
                    Some(format!("Failed to save To-do/Sub-agent bar size: {error}"));
            }
            app.needs_redraw = true;
            return MouseOutcome {
                consumed: true,
                action: None,
            };
        }
        _ => {}
    }
    let inside = mouse.column >= area.x
        && mouse.column < area.right()
        && mouse.row >= area.y
        && mouse.row < area.bottom();
    if !inside {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) && app.work_surface.focused
        {
            // Another region is taking the pointer — release strip focus so
            // only one owner shows selection.
            release_focus(app);
        }
        if matches!(mouse.kind, MouseEventKind::Moved)
            && (app.work_surface.hovered.take().is_some()
                || app.work_surface.hovered_tab.take().is_some())
        {
            app.needs_redraw = true;
        }
        return MouseOutcome::default();
    }

    if placement.is_strip() {
        if let Some(target) = dock_tab_at(app, mouse.column, mouse.row) {
            match mouse.kind {
                MouseEventKind::Moved => {
                    let changed = app.work_surface.hovered_tab != Some(target)
                        || app.work_surface.hovered.take().is_some();
                    app.work_surface.hovered_tab = Some(target);
                    if changed {
                        app.needs_redraw = true;
                    }
                    return MouseOutcome {
                        consumed: true,
                        action: None,
                    };
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    app.work_surface.pressed_tab = Some(target);
                    app.needs_redraw = true;
                    return MouseOutcome {
                        consumed: true,
                        action: None,
                    };
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    let pressed = app.work_surface.pressed_tab.take();
                    if pressed.is_some() {
                        app.needs_redraw = true;
                    }
                    if pressed == Some(target) {
                        match target {
                            DockTabTarget::Panel(panel) if panel != app.work_surface.panel => {
                                super::interaction::select_dock_panel(app, panel);
                            }
                            DockTabTarget::Panel(_) | DockTabTarget::Close => {
                                super::interaction::dismiss_dock(app);
                            }
                        }
                    }
                    return MouseOutcome {
                        consumed: true,
                        action: None,
                    };
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let tabs = dock_panels(app);
                    if !tabs.is_empty() {
                        let current = tabs
                            .iter()
                            .position(|panel| *panel == app.work_surface.panel)
                            .unwrap_or(0);
                        let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                            tabs.len() - 1
                        } else {
                            1
                        };
                        super::interaction::select_dock_panel(
                            app,
                            tabs[(current + delta) % tabs.len()],
                        );
                    }
                    return MouseOutcome {
                        consumed: true,
                        action: None,
                    };
                }
                _ => {}
            }
        } else if (matches!(mouse.kind, MouseEventKind::Moved)
            && app.work_surface.hovered_tab.take().is_some())
            || (matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
                && app.work_surface.pressed_tab.take().is_some())
        {
            app.needs_redraw = true;
        }
    }

    if super::terminal::handle_mouse(app, mouse) {
        return MouseOutcome {
            consumed: true,
            action: None,
        };
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            claim_focus(app);
            app.work_surface.scroll_offset = app.work_surface.scroll_offset.saturating_sub(2);
            app.needs_redraw = true;
            MouseOutcome {
                consumed: true,
                action: None,
            }
        }
        MouseEventKind::ScrollDown => {
            claim_focus(app);
            let max = app
                .work_surface
                .total_rows
                .saturating_sub(app.work_surface.visible_rows.max(1));
            app.work_surface.scroll_offset =
                app.work_surface.scroll_offset.saturating_add(2).min(max);
            app.needs_redraw = true;
            MouseOutcome {
                consumed: true,
                action: None,
            }
        }
        MouseEventKind::Moved => {
            let hovered = hit_row(app, mouse.row).map(|row| row.id.clone());
            if app.work_surface.hovered != hovered {
                app.work_surface.hovered = hovered;
                app.needs_redraw = true;
            }
            MouseOutcome {
                consumed: true,
                action: None,
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let row = hit_row(app, mouse.row).cloned();
            let Some(row) = row else {
                claim_focus(app);
                return MouseOutcome {
                    consumed: true,
                    action: None,
                };
            };
            claim_focus(app);
            app.work_surface.selected = Some(row.id.clone());
            app.needs_redraw = true;

            let action = activate_primary(app, &row.id, row.primary_action.clone());
            MouseOutcome {
                consumed: true,
                action,
            }
        }
        // Right-click is the context menu's (`mouse_ui::open_context_menu`
        // reads the row under the pointer). The catch-all below used to mark
        // it consumed, so no menu ever opened over the work surface.
        MouseEventKind::Down(MouseButton::Right) => MouseOutcome::default(),
        _ => MouseOutcome {
            consumed: true,
            action: None,
        },
    }
}

fn dock_tab_at(app: &App, column: u16, row: u16) -> Option<DockTabTarget> {
    app.work_surface
        .dock_tabs
        .iter()
        .find(|hitbox| hitbox.area.contains((column, row).into()))
        .map(|hitbox| hitbox.target)
}

fn dock_panels(app: &App) -> Vec<RailPanel> {
    app.work_surface
        .dock_tabs
        .iter()
        .filter_map(|hitbox| match hitbox.target {
            DockTabTarget::Panel(panel) => Some(panel),
            DockTabTarget::Close => None,
        })
        .collect()
}

fn hit_row(app: &App, row_y: u16) -> Option<&WorkRow> {
    let id = app
        .work_surface
        .hitboxes
        .iter()
        .find(|hitbox| hitbox.row_y == row_y)
        .map(|hitbox| &hitbox.id)?;
    app.work_surface
        .latest_rows
        .iter()
        .find(|row| &row.id == id)
}

fn selected_row<'a>(app: &App, rows: &'a [WorkRow]) -> Option<&'a WorkRow> {
    let selected = app.work_surface.selected.as_ref()?;
    rows.iter().find(|row| &row.id == selected)
}

fn selectable_ids(rows: &[WorkRow]) -> Vec<WorkRowId> {
    rows.iter()
        .filter(|row| row.selectable)
        .map(|row| row.id.clone())
        .collect()
}

fn move_selection(app: &mut App, rows: &[WorkRow], delta: isize) {
    let ids = selectable_ids(rows);
    if ids.is_empty() {
        return;
    }
    let Some(current) = app
        .work_surface
        .selected
        .as_ref()
        .and_then(|selected| ids.iter().position(|id| id == selected))
    else {
        // Nothing selected — the dock opens this way, and a stale id lands here
        // too. `unwrap_or_default()` called that row 0, so the first Down moved
        // to row 1 and the first row could never be reached by pressing Down.
        // The first move lands on the edge it came from.
        app.work_surface.selected = Some(if delta.is_negative() {
            ids[ids.len().saturating_sub(1)].clone()
        } else {
            ids[0].clone()
        });
        return;
    };
    let next = if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current
            .saturating_add(delta as usize)
            .min(ids.len().saturating_sub(1))
    };
    app.work_surface.selected = Some(ids[next].clone());
}

fn select_edge(app: &mut App, rows: &[WorkRow], end: bool) {
    let ids = selectable_ids(rows);
    app.work_surface.selected = if end {
        ids.last().cloned()
    } else {
        ids.first().cloned()
    };
}

#[cfg(test)]
mod tests {
    use super::{App, WorkRow, move_selection};
    use crate::tui::work_surface::model::{WorkRowId, WorkTone};

    fn row(id: &str) -> WorkRow {
        WorkRow {
            id: WorkRowId(id.to_string()),
            mark: "·",
            label: id.to_string(),
            detail: String::new(),
            tone: WorkTone::Muted,
            selectable: true,
            primary_action: None,
            agent: None,
        }
    }

    fn app() -> App {
        App::new(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
            &codewhale_runtime::config::Config::default(),
        )
    }

    /// The dock opens with nothing selected (`select_dock_panel` sets
    /// `selected = None`). Resolving that to index 0 meant the first Down
    /// landed on the *second* row and the first row could never be reached by
    /// pressing Down at all. The same path catches a stale id that is no
    /// longer in the list.
    #[test]
    fn first_move_lands_on_the_edge_it_came_from() {
        let rows = [row("a"), row("b"), row("c")];

        for (start, delta, expected) in [
            (None, 1isize, "a"),
            (None, -1, "c"),
            (Some("gone"), 1, "a"),
            (Some("gone"), -1, "c"),
            // An established selection still moves by the delta.
            (Some("a"), 1, "b"),
            (Some("b"), -1, "a"),
        ] {
            let mut app = app();
            app.work_surface.selected = start.map(|id| WorkRowId(id.to_string()));
            move_selection(&mut app, &rows, delta);
            assert_eq!(
                app.work_surface.selected.as_ref().map(|id| id.0.as_str()),
                Some(expected),
                "start={start:?} delta={delta}"
            );
        }
    }
}

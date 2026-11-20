//! Where the surface goes and how tall it is — the arithmetic [`height`] and
//! [`super::render`] must agree on before a single cell is painted.

use ratatui::layout::Rect;

use crate::tui::app::App;
use crate::tui::work_surface::model::{
    self, RailPanel, WorkSurfacePlacement, visible_rows_for_panel,
};

use super::{progress_shares_goal_row, top_goal_title, top_todo_progress};

const SIDE_RAIL_MIN_HOST_WIDTH: u16 = 72;
const SIDE_RAIL_MIN_CHAT_WIDTH: u16 = 40;

fn effective_placement(configured: WorkSurfacePlacement, host_width: u16) -> WorkSurfacePlacement {
    if configured == WorkSurfacePlacement::Off {
        return WorkSurfacePlacement::Off;
    }
    // Only the side rails need width; strip placements work at any host
    // width, and a narrow host keeps the user's Bottom default.
    if matches!(
        configured,
        WorkSurfacePlacement::Left | WorkSurfacePlacement::Right
    ) && host_width < SIDE_RAIL_MIN_HOST_WIDTH
    {
        WorkSurfacePlacement::Top
    } else {
        configured
    }
}

/// Responsive work-surface height.
///
/// `rail_budget` is the caller's answer to "how many rows can the transcript
/// actually spare this frame" — terminal height minus fixed chrome minus the
/// transcript's own floor. See [`crate::tui::ui::rail_row_budget`]. The rail
/// takes spare rows; it never takes rows the transcript needs.
///
/// Every view auto-fits its content the same way: content rows + optional
/// goal title + the divider, capped by `top_height` and ambient room. A
/// two-item checklist is two rows; eight agents grow to show eight. The only
/// Top title is an active goal — never panel chrome. Which view is on screen
/// is decided first by [`model::resolve_view`]: the user's explicit pick, or
/// the first work view with content.
pub fn height(app: &mut App, width: u16, terminal_height: u16, rail_budget: u16) -> u16 {
    app.work_surface.effective_placement = effective_placement(app.work_surface.placement, width);
    // Off hides the rail outright: no strip, no side reservation, no stale
    // interaction state.
    if app.work_surface.effective_placement == WorkSurfacePlacement::Off {
        collapse_strip(app);
        return 0;
    }
    model::resolve_view(app);
    if app.work_surface.dismissed {
        // New work anywhere in the auto views re-opens the dock — a worker
        // spawning while the to-do list is on top counts, even though the
        // resolved view stays TODO.
        let current_rows = model::auto_work_rows(app);
        let new_work = app.work_surface.panel != app.work_surface.dismissed_view
            || current_rows > app.work_surface.dismissed_at_rows;
        if new_work {
            app.work_surface.dismissed = false;
        } else {
            collapse_strip(app);
            return 0;
        }
    }

    // The strip is something to SHOW, not a fixture: without an explicit
    // pick it opens only for work — live agents, the to-do list, background
    // jobs. The persisted `rail_panel` preference (files, context, git,
    // price…) always has rows to paint, and letting it auto-open made the
    // dock a permanent band under the composer. It still opens on demand
    // (cycle, tab click, `/workbar <view>`) and then sticks until Esc.
    let auto_open =
        app.work_surface.explicit_view || RailPanel::AUTO_ORDER.contains(&app.work_surface.panel);
    let rows = if auto_open {
        visible_rows_for_panel(app)
    } else {
        Vec::new()
    };
    let strip = app.work_surface.effective_placement.is_strip();
    let goal_rows = u16::from(strip && top_goal_title(app).is_some());
    let explicit = app.work_surface.explicit_view;
    if rows.is_empty() && !explicit {
        // A live goal alone still deserves a strip: title + divider.
        if goal_rows == 0 {
            collapse_strip(app);
            app.work_surface.latest_rows.clear();
            app.work_surface.visible_rows = 0;
            app.work_surface.total_rows = 0;
            app.work_surface.scroll_offset = 0;
            return 0;
        }
        if !strip {
            return 0;
        }
        let cap = top_cap(app, terminal_height, rail_budget);
        if cap < model::TOP_HEIGHT_MIN {
            collapse_strip(app);
            return 0;
        }
        return (goal_rows.saturating_add(2)).clamp(model::TOP_HEIGHT_MIN, cap);
    }
    if !strip {
        return 0;
    }
    // The strip auto-fits its content: the view's rows plus the optional
    // goal title, the pinned progress receipt, and the divider row, bounded
    // by `top_cap`. An explicitly opened empty view keeps one row for its
    // "nothing here yet" line so cycling never lands on a blank band.
    let cap = top_cap(app, terminal_height, rail_budget);
    if cap < model::TOP_HEIGHT_MIN && (!explicit || cap < 3) {
        collapse_strip(app);
        return 0;
    }
    if app.work_surface.panel == RailPanel::Terminal {
        return cap;
    }
    let list_rows = rows.len().max(usize::from(explicit));
    let body_width = if width >= 4 { width - 2 } else { width };
    let progress = u16::from(
        top_todo_progress(app, &rows).is_some()
            && !progress_shares_goal_row(body_width, goal_rows > 0),
    );
    let desired = u16::try_from(list_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(progress)
        .saturating_add(goal_rows)
        .saturating_add(2);
    desired.clamp(model::TOP_HEIGHT_MIN.min(cap), cap)
}

/// The ceilings the *terminal* imposes, independent of anything the user
/// asked for, smallest wins:
///
/// - half the terminal: proportional restraint, so a tall rail on a short
///   terminal still reads as a strip over a transcript.
/// - `rail_budget`: the rows the transcript can actually spare. This is the
///   only one that knows the transcript has a floor, and it is the one that
///   lets decorative water outrank a panel nobody is watching.
///
/// Kept separate from [`top_cap`] because the collapse cliff must be charged
/// against ambient room alone. Both are monotone non-decreasing in terminal
/// height, which is what keeps the strip from blinking across a resize.
fn ambient_cap(terminal_height: u16, rail_budget: u16) -> u16 {
    terminal_height
        .saturating_div(2)
        .clamp(model::TOP_HEIGHT_MIN, model::TOP_HEIGHT_MAX)
        .min(rail_budget)
}

/// [`ambient_cap`] plus `top_height` — what the user asked for via
/// drag-resize / settings. This is the ceiling on how *tall* a strip may
/// grow; it is deliberately not the quantity a collapse threshold is
/// compared against.
fn top_cap(app: &App, terminal_height: u16, rail_budget: u16) -> u16 {
    app.work_surface
        .top_height
        .min(ambient_cap(terminal_height, rail_budget))
}

/// Drop the interaction state that only means anything while a strip is on
/// screen. Every path reporting "no strip this frame" must run this: hitboxes
/// outlive the rows they described, so a strip that yielded its rows would
/// still swallow clicks landing on the transcript that replaced it.
pub(crate) fn collapse_strip(app: &mut App) {
    app.work_surface.last_area = None;
    app.work_surface.hitboxes.clear();
    app.work_surface.focused = false;
    app.work_surface.selected = None;
    app.work_surface.opened = None;
    app.work_surface.hovered = None;
    app.work_surface.resizing = false;
    app.work_surface.divider_hovered = false;
    app.work_surface.dock_tabs.clear();
    app.work_surface.pressed_tab = None;
    app.work_surface.hovered_tab = None;
}

/// Split the transcript slot for a side rail. Top placement consumes its own
/// vertical row before this point, so it returns the chat area unchanged.
///
/// Placement and auto-fit are orthogonal but share one rule: **empty work is
/// not a rail**. Top expresses that as `height() == 0`. Left/Right express it
/// here — no column is reserved when the selected panel has nothing to say.
/// When there *is* content, the rail takes the full chat height at the
/// configured `side_width` (width is the ceiling, the way `top_height` is the
/// ceiling on Top). Narrow terminals that cannot fit the rail fall back to
/// Top, where height auto-fit takes over.
///
/// `min_chat_width` is the column-axis twin of `height`'s `rail_budget`: the
/// columns the transcript must keep. When the idle ocean is on screen that is
/// the ambient floor, and a rail that cannot fit beside it hides rather than
/// squeezing the water into a strip too narrow to draw.
pub fn split_chat(app: &mut App, area: Rect, min_chat_width: u16) -> (Rect, Option<Rect>) {
    let placement = effective_placement(app.work_surface.placement, area.width);
    app.work_surface.effective_placement = placement;
    if placement.is_strip() || placement == WorkSurfacePlacement::Off {
        return (area, None);
    }
    // Same empty-collapse rule as Top: a panel with nothing to show does not
    // spend columns on a blank (or "No agents") column.
    if !side_rail_has_content(app) {
        collapse_strip(app);
        return (area, None);
    }

    let min_chat_width = min_chat_width.max(SIDE_RAIL_MIN_CHAT_WIDTH);
    let rail_width = app
        .work_surface
        .side_width
        .clamp(model::SIDE_WIDTH_MIN, model::SIDE_WIDTH_MAX)
        .min(area.width.saturating_sub(min_chat_width));
    if rail_width < model::SIDE_WIDTH_MIN {
        // Too narrow for a side column — fall back to Top. The caller will
        // re-ask height() with effective_placement Top so content auto-fits
        // as a strip instead of vanishing.
        app.work_surface.effective_placement = WorkSurfacePlacement::Top;
        collapse_strip(app);
        return (area, None);
    }

    let chat_width = area.width.saturating_sub(rail_width);
    match placement {
        WorkSurfacePlacement::Left => (
            Rect {
                x: area.x.saturating_add(rail_width),
                width: chat_width,
                ..area
            },
            Some(Rect {
                width: rail_width,
                ..area
            }),
        ),
        WorkSurfacePlacement::Right => (
            Rect {
                width: chat_width,
                ..area
            },
            Some(Rect {
                x: area.x.saturating_add(chat_width),
                width: rail_width,
                ..area
            }),
        ),
        WorkSurfacePlacement::Top | WorkSurfacePlacement::Bottom | WorkSurfacePlacement::Off => {
            (area, None)
        }
    }
}

/// Whether a Left/Right rail should reserve columns this frame.
fn side_rail_has_content(app: &mut App) -> bool {
    if app.work_surface.dismissed {
        return false;
    }
    model::resolve_view(app);
    app.work_surface.explicit_view || !visible_rows_for_panel(app).is_empty()
}

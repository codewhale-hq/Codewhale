//! Painting the work surface, and the two files it leans on.
//!
//! - [`layout`] answers *where and how tall* — placement fallback, the height
//!   and cap arithmetic, and the side-rail split.
//! - [`rows`] answers *what one row says* — the sub-agent column layout, its
//!   degradation tiers, and row styling.
//!
//! What stays here is the paint itself: the strip, the side rail, the dock
//! tab row, the divider and scrollbar chrome, and the strip header content
//! (goal title, to-do receipt) that height and paint must both agree on.
//! Every view — work rows and fact rows alike — goes through the one row
//! loop below; there is no second line-list renderer.

use std::collections::HashMap;

use ratatui::{
    Frame,
    layout::Rect,
    prelude::Widget,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::{App, SidebarHoverRow, SidebarHoverSection};
use codewhale_localization::MessageId;
use codewhale_palette::{ChromeInk, chrome_style};
use codewhale_tui_kit::ui_text::truncate_line_to_width;

use super::model::{
    DockTabHitbox, DockTabTarget, RailPanel, WorkHitbox, WorkRow, WorkSurfacePlacement, WorkTone,
    plan_step_row_ids, visible_rows_for, visible_rows_for_panel,
};

mod layout;
mod rows;

#[cfg(test)]
mod body_legacy;
#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod dock_tabs_legacy;
#[cfg(test)]
mod dock_tabs_tests;

pub(crate) use layout::collapse_strip;
pub use layout::{height, split_chat};

use rows::{
    AGENT_ROLE_GUTTER, AgentRowTier, agent_identity, agent_identity_cap, agent_identity_column,
    agent_receipt, agent_row_styles, agent_status_column, layout_agent_row, row_style,
};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        collapse_strip(app);
        return;
    }

    if let Some(previous) = app.work_surface.last_area {
        app.sidebar_hover
            .sections
            .retain(|section| section.content_area != previous);
    }

    let placement = app.work_surface.effective_placement;
    // Off renders no rail; height()/split_chat() never hand us an area for it.
    if placement == WorkSurfacePlacement::Off {
        collapse_strip(app);
        return;
    }
    let body_area = match placement {
        // Bottom mirrors Top's body/divider split; only the divider edge
        // differs (below-content for Top, above-content for Bottom).
        WorkSurfacePlacement::Top => Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(2),
            ..area
        },
        WorkSurfacePlacement::Bottom => Rect {
            y: area.y.saturating_add(2),
            height: area.height.saturating_sub(2),
            ..area
        },
        WorkSurfacePlacement::Left => Rect {
            width: area.width.saturating_sub(1),
            ..area
        },
        WorkSurfacePlacement::Right => Rect {
            x: area.x.saturating_add(1),
            width: area.width.saturating_sub(1),
            ..area
        },
        WorkSurfacePlacement::Off => unreachable!("off placement returned above"),
    };

    if !placement.is_strip() {
        app.work_surface.dock_tabs.clear();
        app.work_surface.pressed_tab = None;
        app.work_surface.hovered_tab = None;
    }

    super::model::resolve_view(app);
    let rows = visible_rows_for_panel(app);
    if app.work_surface.panel == RailPanel::Terminal {
        Block::default()
            .style(Style::default().bg(app.ui_theme.panel_bg))
            .render(area, frame.buffer_mut());
        render_dock_tabs(frame, area, app);
        register_dock_targets(app);
        super::terminal::render(frame, body_area, app);
        render_divider(frame, area, placement, app);
        app.work_surface.last_area = Some(area);
        return;
    }
    let todo_ordinals = if placement.is_strip() {
        todo_ordinals(&rows)
    } else {
        HashMap::new()
    };
    let ordinal_width = todo_ordinals.len().max(1).to_string().len();
    let todo_ids = plan_step_row_ids(app);
    let goal_title = placement.is_strip().then(|| top_goal_title(app)).flatten();
    let todo_progress = placement
        .is_strip()
        .then(|| top_todo_progress(app, &rows))
        .flatten();
    // Pin goal title, then progress receipt, above the scrollable rows.
    // A compact strip keeps its last usable row for content, ahead of headers.
    // Body geometry has one shared owner; Engine still supplies viewport,
    // caller rows, goal/progress truth and the remembered scroll offset.
    let body_layout = codewhale_ratatui::WorkbarLayout::for_body(
        body_area,
        rows.len(),
        app.work_surface.scroll_offset,
        goal_title.is_some(),
        todo_progress.is_some(),
    );
    let goal_height = body_layout.goal_height;
    let fold_progress = progress_shares_goal_row(body_area.width, goal_height > 0);
    let progress_height = body_layout.progress_height;
    let content_area = body_layout.content;
    let overflow = body_layout.overflow;
    let more_row = body_layout.more_row;
    let list_rows = body_layout.visible_rows;
    app.work_surface.visible_rows = list_rows;
    app.work_surface.total_rows = rows.len();
    // A redraw clamps the viewport, never reveals the remembered selection.
    app.work_surface.clamp_viewport(&rows);

    Block::default()
        .style(Style::default().bg(app.ui_theme.panel_bg))
        .render(area, frame.buffer_mut());
    render_dock_tabs(frame, area, app);
    register_dock_targets(app);

    if let Some((goal_text, goal_style)) = goal_title.filter(|_| goal_height > 0) {
        let full_width = usize::from(content_area.width);
        // Wide strips carry the receipt right-aligned on the goal row rather
        // than spending a second row announcing a count.
        let receipt = todo_progress.as_deref().filter(|_| fold_progress);
        let reserved = receipt
            .map(|text| UnicodeWidthStr::width(text).saturating_add(2))
            .unwrap_or(0);
        let goal_text = truncate_line_to_width(&goal_text, full_width.saturating_sub(reserved));
        let mut spans = vec![Span::styled(
            goal_text.clone(),
            goal_style.bg(app.ui_theme.panel_bg),
        )];
        if let Some(receipt) = receipt {
            let gap = full_width
                .saturating_sub(UnicodeWidthStr::width(goal_text.as_str()))
                .saturating_sub(UnicodeWidthStr::width(receipt));
            spans.push(Span::styled(
                format!("{}{receipt}", " ".repeat(gap)),
                Style::default()
                    .fg(app.ui_theme.text_muted)
                    .bg(app.ui_theme.panel_bg),
            ));
        }
        Paragraph::new(Line::from(spans)).render(
            Rect {
                y: body_area.y,
                height: 1,
                ..content_area
            },
            frame.buffer_mut(),
        );
    }

    if let Some(progress) = todo_progress.filter(|_| progress_height > 0) {
        let progress = truncate_line_to_width(&progress, usize::from(content_area.width));
        // Muted, not accent: accent_primary means "selected" everywhere else
        // in the strip, and spending it on a static count makes the actual
        // selection hard to find.
        Paragraph::new(Line::from(Span::styled(
            progress,
            Style::default()
                .fg(app.ui_theme.text_muted)
                .bg(app.ui_theme.panel_bg),
        )))
        .render(
            Rect {
                y: body_area.y.saturating_add(goal_height),
                height: 1,
                ..content_area
            },
            frame.buffer_mut(),
        );
    }

    let start = app.work_surface.scroll_offset;
    let visible = rows.iter().skip(start).take(list_rows).collect::<Vec<_>>();
    let identity_cap = agent_identity_cap(usize::from(content_area.width));
    let identity_column = agent_identity_column(&visible, identity_cap);
    let status_column = agent_status_column(&visible);
    let mut lines = Vec::with_capacity(visible.len().saturating_add(1));
    let mut hover_rows = Vec::new();
    let mut hitboxes = Vec::new();
    for (visible_index, row) in visible.iter().enumerate() {
        let row_y = content_area.y.saturating_add(visible_index as u16);
        let selected =
            app.work_surface.focused && app.work_surface.selected.as_ref() == Some(&row.id);
        let hovered = app.work_surface.hovered.as_ref() == Some(&row.id);
        let opened = app.work_surface.opened.as_ref() == Some(&row.id);
        let style = row_style(app, row, selected, hovered, opened);
        let compact_owner = if placement.is_strip() {
            todo_ordinals
                .get(&row.id.0)
                .map(|ordinal| format!("{ordinal:>ordinal_width$} · "))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mark = if opened && row.selectable {
            "▾"
        } else {
            row.mark
        };
        // Agent focus marker: while a worker is focused every row gains a
        // two-cell gutter and the focused worker's row shows the selection
        // glyph in it, so the addressed fork is visible at the left edge.
        let focus_gutter = if app.agent_focus.is_some() {
            let focused = row
                .id
                .0
                .strip_prefix("worker:")
                .is_some_and(|id| app.agent_focus.as_ref().is_some_and(|f| f.is(id)));
            if focused {
                "❯ ".to_string()
            } else {
                "  ".to_string()
            }
        } else {
            String::new()
        };
        let prefix = if row.tone == WorkTone::Heading {
            format!("{focus_gutter}{} ", mark)
        } else {
            format!("{focus_gutter}{compact_owner}{mark} ")
        };

        // Sub-agent rows own their own column layout: glyph, agent type,
        // objective, right-aligned elapsed and tokens. They stay ordinary
        // rows in every other respect — same hitbox, same selection, same
        // primary action.
        if let Some(facts) = row.agent.as_ref() {
            let queued = row
                .id
                .0
                .strip_prefix("worker:")
                .and_then(|id| crate::tui::agent_focus::queued_suffix(app, id))
                .map(|queued| format!(" · {queued}"));
            let queued_width = queued.as_deref().map(UnicodeWidthStr::width).unwrap_or(0);
            let laid_out = layout_agent_row(
                usize::from(content_area.width).saturating_sub(queued_width),
                UnicodeWidthStr::width(prefix.as_str()),
                &agent_identity(row, identity_cap),
                identity_column,
                status_column,
                facts,
            );
            let (normal, muted) = agent_row_styles(app, selected, hovered, opened);
            let display = format!(
                "{prefix}{}{}{}{}{}{}{}",
                laid_out.role,
                if laid_out.role.is_empty() {
                    String::new()
                } else {
                    " ".repeat(AGENT_ROLE_GUTTER)
                },
                laid_out.status,
                if laid_out.status.is_empty() {
                    String::new()
                } else {
                    " ".repeat(AGENT_ROLE_GUTTER)
                },
                laid_out.objective,
                " ".repeat(laid_out.gap),
                laid_out.receipt,
            );
            let mut spans = vec![Span::styled(prefix.clone(), normal)];
            if !laid_out.role.is_empty() {
                spans.push(Span::styled(
                    format!("{}{}", laid_out.role, " ".repeat(AGENT_ROLE_GUTTER)),
                    muted,
                ));
            }
            if !laid_out.status.is_empty() {
                spans.push(Span::styled(
                    format!("{}{}", laid_out.status, " ".repeat(AGENT_ROLE_GUTTER)),
                    muted,
                ));
            }
            spans.push(Span::styled(laid_out.objective.clone(), normal));
            spans.push(Span::styled(
                format!("{}{}", " ".repeat(laid_out.gap), laid_out.receipt),
                muted,
            ));
            if let Some(queued) = queued.as_deref() {
                // Truthful `· N queued`: follow-ups the running child has not
                // yet folded into its next round. Amber: a message is waiting
                // on a busy agent, the one thing on this row that needs you.
                spans.push(Span::styled(
                    queued.to_string(),
                    Style::default()
                        .fg(app.ui_theme.warning)
                        .bg(normal.bg.unwrap_or(app.ui_theme.panel_bg)),
                ));
            }
            lines.push(Line::from(spans));

            hitboxes.push(WorkHitbox {
                id: row.id.clone(),
                row_y,
            });
            hover_rows.push(SidebarHoverRow {
                row_y,
                display_text: display,
                full_text: format!("{} · {}", row.label, row.detail),
                detail: Some(row.detail.clone()),
                is_truncated: laid_out.objective != facts.objective
                    || laid_out.receipt != agent_receipt(facts, AgentRowTier::Full),
                click_action: row.primary_action.clone(),
                stop_action: None,
                stop_zone_start_col: None,
                stop_zone_end_col: None,
            });
            continue;
        }

        let detail_candidate = if row.tone != WorkTone::Heading && content_area.width >= 44 {
            format!("  {}", row.detail)
        } else {
            String::new()
        };
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        let row_width = usize::from(content_area.width);
        let detail_room = if todo_ids.contains(&row.id.0) {
            UnicodeWidthStr::width(detail_candidate.as_str())
                .min(row_width.saturating_sub(prefix_width) / 2)
        } else {
            0
        };
        let label_budget = row_width.saturating_sub(prefix_width + detail_room).max(1);
        let label = truncate_line_to_width(&row.label, label_budget);
        let detail_budget =
            row_width.saturating_sub(prefix_width + UnicodeWidthStr::width(label.as_str()));
        let detail = if detail_budget >= 4 {
            truncate_line_to_width(&detail_candidate, detail_budget)
        } else {
            String::new()
        };
        let detail_width = UnicodeWidthStr::width(detail.as_str());
        let gap = usize::from(content_area.width)
            .saturating_sub(prefix_width + UnicodeWidthStr::width(label.as_str()) + detail_width);
        let display = format!("{prefix}{label}{}{detail}", " ".repeat(gap));
        lines.push(Line::from(Span::styled(display.clone(), style)));

        hitboxes.push(WorkHitbox {
            id: row.id.clone(),
            row_y,
        });

        if row.selectable {
            hover_rows.push(SidebarHoverRow {
                row_y,
                display_text: display,
                full_text: format!("{} · {}", row.label, row.detail),
                detail: Some(row.detail.clone()),
                is_truncated: label != row.label || detail != detail_candidate,
                click_action: row.primary_action.clone(),
                stop_action: None,
                stop_zone_start_col: None,
                stop_zone_end_col: None,
            });
        }
    }

    if visible.is_empty() && app.work_surface.explicit_view && content_area.height > 0 {
        // An explicitly opened view with nothing in it says so, once, so
        // cycling never lands on a blank band.
        lines.push(Line::from(Span::styled(
            truncate_line_to_width(
                empty_view_hint(app.work_surface.panel),
                usize::from(content_area.width),
            ),
            Style::default()
                .fg(app.ui_theme.text_muted)
                .bg(app.ui_theme.panel_bg),
        )));
    }

    if more_row {
        // Right-aligned under the receipt column, muted like every other
        // secondary figure. Scrolled to the bottom there is nothing below, so
        // the reserved row stays blank rather than claiming a count of zero.
        let remaining = rows
            .len()
            .saturating_sub(start.saturating_add(visible.len()));
        let text = if remaining == 0 {
            String::new()
        } else {
            truncate_line_to_width(
                &format!("↓ {remaining} more"),
                usize::from(content_area.width),
            )
        };
        let pad = usize::from(content_area.width).saturating_sub(UnicodeWidthStr::width(&*text));
        lines.push(Line::from(Span::styled(
            format!("{}{text}", " ".repeat(pad)),
            Style::default()
                .fg(app.ui_theme.text_muted)
                .bg(app.ui_theme.panel_bg),
        )));
    }

    Paragraph::new(lines).render(content_area, frame.buffer_mut());
    render_divider(frame, area, placement, app);
    if overflow {
        render_scrollbar(
            frame,
            Rect {
                x: body_area.right().saturating_sub(1),
                y: content_area.y,
                width: 1,
                height: content_area.height,
            },
            app.work_surface.scroll_offset,
            list_rows,
            rows.len(),
            app,
        );
    }

    app.work_surface.last_area = Some(area);
    app.work_surface.hitboxes = hitboxes;
    app.sidebar_hover.sections.push(SidebarHoverSection {
        content_area,
        lines: visible.iter().map(|row| row.label.clone()).collect(),
        rows: hover_rows,
    });
    // The tab badges projected the other views on the way here; the rows
    // a click resolves against are the ones this frame painted.
    app.work_surface.latest_rows = rows;
}

/// What an explicitly opened, empty view says on its one row.
fn empty_view_hint(panel: RailPanel) -> &'static str {
    match panel {
        RailPanel::Agents => "no agents have run this session",
        RailPanel::Tasks => "no to-dos yet",
        RailPanel::Background => "nothing running in the background",
        RailPanel::Terminal => "",
        RailPanel::Files => "no files touched this session",
        RailPanel::Notepad => "/note add <text> to keep a note",
        RailPanel::Context => "context budget unknown",
        // The Git view always paints its own state row ("reading git
        // status…", "not a git repository", "git unavailable: …").
        RailPanel::Git => "reading git status…",
        RailPanel::Price => "no priced turns yet",
    }
}

/// Active goal as the Top strip's only title. Uses the same
/// paused/active/terminal resolution as the ocean header chip so a goal set
/// via `create_goal` is either visible everywhere or nowhere. Returns
/// `None` when no live goal exists — Top then paints no title row at all.
pub(super) fn top_goal_title(app: &App) -> Option<(String, Style)> {
    let (objective, paused) = crate::tui::footer_ui::active_goal_chip_state(app)?;
    let flat = objective.trim().replace(['\n', '\r'], " ");
    if flat.is_empty() {
        return None;
    }
    let text = if paused {
        format!("Goal (paused): {flat}")
    } else {
        format!("Goal: {flat}")
    };
    let style = if paused {
        Style::default()
            .fg(app.ui_theme.warning)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(app.ui_theme.status_working)
            .add_modifier(Modifier::BOLD)
    };
    Some((text, style))
}

fn todo_ordinals(rows: &[WorkRow]) -> HashMap<String, usize> {
    rows.iter()
        .filter(|row| row.id.0.starts_with("graph:"))
        .enumerate()
        .map(|(index, row)| (row.id.0.clone(), index.saturating_add(1)))
        .collect()
}

/// Shared goal/receipt folding rule used by both height and body painting.
pub(super) fn progress_shares_goal_row(width: u16, has_goal_title: bool) -> bool {
    codewhale_ratatui::WorkbarLayout::progress_shares_goal_row(width, has_goal_title)
}

pub(super) fn top_todo_progress(app: &App, rows: &[WorkRow]) -> Option<String> {
    let todos = rows
        .iter()
        .filter(|row| row.id.0.starts_with("graph:"))
        .collect::<Vec<_>>();
    let total = todos.len();
    if total == 0 {
        return None;
    }
    let completed = todos
        .iter()
        .filter(|row| row.tone == WorkTone::Success)
        .count();
    let remaining = total.saturating_sub(completed);
    let label = format!("{} ·", app.tr(MessageId::SidebarTodoLabel));
    Some(
        app.tr(MessageId::WorkSurfaceTodoProgress)
            .replace("{label}", &label)
            .replace("{completed}", &completed.to_string())
            .replace("{total}", &total.to_string())
            .replace("{remaining}", &remaining.to_string()),
    )
}

fn render_divider(frame: &mut Frame, area: Rect, placement: WorkSurfacePlacement, app: &App) {
    let active = app.work_surface.resizing || app.work_surface.divider_hovered;
    let color = if active {
        app.ui_theme.accent_primary
    } else {
        app.ui_theme.border
    };
    match placement {
        WorkSurfacePlacement::Off => {}
        WorkSurfacePlacement::Top => {
            let y = area.bottom().saturating_sub(1);
            for x in area.left()..area.right() {
                frame.buffer_mut()[(x, y)]
                    .set_symbol(if active { "━" } else { "─" })
                    .set_fg(color)
                    .set_bg(app.ui_theme.panel_bg);
            }
        }
        WorkSurfacePlacement::Bottom => {
            let y = area.top();
            for x in area.left()..area.right() {
                frame.buffer_mut()[(x, y)]
                    .set_symbol(if active { "━" } else { "─" })
                    .set_fg(color)
                    .set_bg(app.ui_theme.panel_bg);
            }
        }
        WorkSurfacePlacement::Left | WorkSurfacePlacement::Right => {
            let x = if placement == WorkSurfacePlacement::Left {
                area.right().saturating_sub(1)
            } else {
                area.left()
            };
            for y in area.top()..area.bottom() {
                frame.buffer_mut()[(x, y)]
                    .set_symbol(if active { "┃" } else { "│" })
                    .set_fg(color)
                    .set_bg(app.ui_theme.panel_bg);
            }
        }
    }
}

// Engine supplies the caller's available panels, exact close semantics and
// live theme. The kit owns the one fitted paint/hitbox plan.
fn kit_panel(panel: RailPanel) -> codewhale_ratatui::WorkbarPanel {
    use codewhale_ratatui::WorkbarPanel as P;
    match panel {
        RailPanel::Tasks => P::Tasks,
        RailPanel::Agents => P::Fleet,
        RailPanel::Background => P::Jobs,
        RailPanel::Terminal => P::Terminal,
        RailPanel::Files => P::Files,
        RailPanel::Notepad => P::Notes,
        RailPanel::Context => P::Context,
        RailPanel::Git => P::Git,
        RailPanel::Price => P::Cost,
    }
}

fn native_target(target: codewhale_ratatui::DockTabTarget) -> DockTabTarget {
    use codewhale_ratatui::{DockTabTarget as T, WorkbarPanel as P};
    match target {
        T::Close => DockTabTarget::Close,
        T::Panel(panel) => DockTabTarget::Panel(match panel {
            P::Tasks => RailPanel::Tasks,
            P::Fleet => RailPanel::Agents,
            P::Jobs => RailPanel::Background,
            P::Terminal => RailPanel::Terminal,
            P::Files => RailPanel::Files,
            P::Notes => RailPanel::Notepad,
            P::Context => RailPanel::Context,
            P::Git => RailPanel::Git,
            P::Cost => RailPanel::Price,
        }),
    }
}

fn render_dock_tabs(frame: &mut Frame, area: Rect, app: &mut App) {
    use codewhale_ratatui::{DockTabRow, DockTabStyles, DockTabTarget as T, WorkbarTab};
    let entries = RailPanel::ORDER
        .into_iter()
        .filter_map(|panel| {
            let count = dock_tab_count(app, panel);
            let useful = count.is_some_and(|count| count > 0)
                || super::views::view_always_has_content(panel);
            (useful || panel == app.work_surface.panel).then_some(WorkbarTab {
                panel: kit_panel(panel),
                count,
            })
        })
        .collect::<Vec<_>>();
    let close_mark = if crate::tui::color_compat::ascii_safe_enabled() {
        "x"
    } else {
        "×"
    };
    // Name Esc only while Engine's actual keyboard action closes this dock.
    let esc_closes = app.work_surface.panel != RailPanel::Terminal
        && app.work_surface.focused
        && !super::interaction::opened_detail_on_screen(app)
        && (app.work_surface.explicit_view || !visible_rows_for_panel(app).is_empty());
    let close = if esc_closes && area.width >= 60 {
        format!(" Esc {close_mark} ")
    } else {
        format!(" {close_mark} ")
    };
    let row = DockTabRow {
        tabs: &entries,
        active: kit_panel(app.work_surface.panel),
        bottom: app.work_surface.effective_placement == WorkSurfacePlacement::Bottom,
        close: close.into(),
        hovered: app.work_surface.hovered_tab.map(|target| match target {
            DockTabTarget::Panel(panel) => T::Panel(kit_panel(panel)),
            DockTabTarget::Close => T::Close,
        }),
        pressed: app
            .work_surface
            .pressed_tab
            .and_then(|target| match target {
                DockTabTarget::Panel(panel) => Some(kit_panel(panel)),
                DockTabTarget::Close => None,
            }),
        styles: DockTabStyles {
            idle: chrome_style(&app.ui_theme, ChromeInk::Metadata),
            active: Style::default()
                .fg(app.ui_theme.text_body)
                .bg(app.ui_theme.selection_bg)
                .add_modifier(Modifier::BOLD),
            hovered: Style::default()
                .fg(app.ui_theme.text_body)
                .bg(app.ui_theme.elevated_bg)
                .add_modifier(Modifier::UNDERLINED),
            close: chrome_style(&app.ui_theme, ChromeInk::MetadataHint),
            close_hovered: chrome_style(&app.ui_theme, ChromeInk::Info)
                .bg(app.ui_theme.elevated_bg)
                .add_modifier(Modifier::UNDERLINED),
        },
    };
    let plan = row.plan(area);
    plan.paint(frame.buffer_mut());
    app.work_surface.dock_tabs = plan
        .hitboxes()
        .into_iter()
        .map(|(target, area)| DockTabHitbox {
            target: native_target(target),
            area,
        })
        .collect();
}

/// The badge on a view's tab: how many rows of *work* it holds. `None` for
/// the fact views (context, git, price), which never badge.
fn dock_tab_count(app: &mut App, panel: RailPanel) -> Option<usize> {
    match panel {
        RailPanel::Agents => Some(
            visible_rows_for(app, panel)
                .iter()
                .filter(|row| row.id.0.starts_with("worker:"))
                .count(),
        ),
        RailPanel::Tasks => Some(
            visible_rows_for(app, panel)
                .iter()
                .filter(|row| row.id.0.starts_with("graph:"))
                .count(),
        ),
        // Group headings (`▾ Shells N`) are selectable doors, not jobs.
        RailPanel::Background => Some(
            visible_rows_for(app, panel)
                .iter()
                .filter(|row| row.selectable && !row.id.0.starts_with("section:"))
                .count(),
        ),
        RailPanel::Files => Some(super::views::files_touched_count(app)),
        RailPanel::Notepad => Some(usize::from(super::views::notepad_has_text(app))),
        RailPanel::Terminal => None,
        RailPanel::Context | RailPanel::Git | RailPanel::Price => None,
    }
}

fn register_dock_targets(app: &mut App) {
    let targets = app.work_surface.dock_tabs.clone();
    for hitbox in targets {
        let (id, action) = match hitbox.target {
            DockTabTarget::Panel(panel) => {
                use crate::tui::tideline::InteractionTargetId as Id;
                let id = match panel {
                    RailPanel::Agents => Id::DOCK_TAB_AGENTS,
                    RailPanel::Tasks => Id::DOCK_TAB_TASKS,
                    RailPanel::Background => Id::DOCK_TAB_BACKGROUND,
                    RailPanel::Terminal => Id::DOCK_TAB_TERMINAL,
                    RailPanel::Files => Id::DOCK_TAB_FILES,
                    RailPanel::Notepad => Id::DOCK_TAB_NOTEPAD,
                    RailPanel::Context => Id::DOCK_TAB_CONTEXT,
                    RailPanel::Git => Id::DOCK_TAB_GIT,
                    RailPanel::Price => Id::DOCK_TAB_PRICE,
                };
                (
                    id,
                    crate::tui::tideline::InteractionAction::ShowDockPanel(panel),
                )
            }
            DockTabTarget::Close => (
                crate::tui::tideline::InteractionTargetId::DOCK_CLOSE,
                crate::tui::tideline::InteractionAction::DismissDock,
            ),
        };
        app.viewport
            .interaction_targets
            .register(crate::tui::tideline::InteractionTarget {
                id,
                area: hitbox.area,
                focus: crate::tui::tideline::InteractionFocus::Direct,
                keyboard_action: Some(action),
                mouse_action: Some(action),
                inspect_detail: crate::tui::tideline::InspectDetail::Route,
            });
    }
}

fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    offset: usize,
    visible: usize,
    total: usize,
    app: &App,
) {
    codewhale_ratatui::WorkbarScrollbar {
        offset,
        visible,
        total,
        thumb: "┃",
        track: "│",
        thumb_style: Style::default()
            .fg(app.ui_theme.status_working)
            .bg(app.ui_theme.panel_bg),
        track_style: Style::default()
            .fg(app.ui_theme.border)
            .bg(app.ui_theme.panel_bg),
    }
    .paint(area, frame.buffer_mut());
}

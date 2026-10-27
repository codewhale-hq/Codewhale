//! Frozen native body renderer, acceptance only.
use super::*;

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
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

    super::super::model::resolve_view(app);
    let rows = visible_rows_for_panel(app);
    let todo_ordinals = if placement.is_strip() {
        todo_ordinals(&rows)
    } else {
        HashMap::new()
    };
    let ordinal_width = todo_ordinals.len().max(1).to_string().len();
    let goal_title = placement.is_strip().then(|| top_goal_title(app)).flatten();
    let todo_progress = placement
        .is_strip()
        .then(|| top_todo_progress(app, &rows))
        .flatten();
    // Pin goal title, then progress receipt, above the scrollable rows.
    // A compact strip keeps its last usable row for content, ahead of headers.
    let goal_height = u16::from(goal_title.is_some() && body_area.height >= 2);
    let fold_progress = progress_shares_goal_row(body_area.width, goal_height > 0);
    let progress_height = u16::from(
        todo_progress.is_some()
            && !fold_progress
            && body_area.height.saturating_sub(goal_height) >= 2,
    );
    let header_height = goal_height.saturating_add(progress_height);
    let list_height = body_area.height.saturating_sub(header_height);
    let body_height = usize::from(list_height);
    let overflow = rows.len() > body_height;
    // A capped list owes the reader the size of what it is hiding, so the
    // last painted row becomes `↓ N more`. The scrollbar shows position; only
    // this shows how much work is off-screen.
    let more_row = overflow && body_height >= 2;
    let list_rows = if more_row {
        body_height.saturating_sub(1)
    } else {
        body_height
    };
    let inset = u16::from(body_area.width >= 16);
    let rail_width = u16::from(overflow);
    let content_area = Rect {
        x: body_area.x.saturating_add(inset),
        y: body_area.y.saturating_add(header_height),
        width: body_area
            .width
            .saturating_sub(inset.saturating_mul(2))
            .saturating_sub(rail_width),
        height: list_height,
    };

    app.work_surface.visible_rows = list_rows;
    app.work_surface.total_rows = rows.len();
    // A redraw may clamp an obsolete offset, but it must not reveal the
    // remembered keyboard selection: doing so undoes mouse-wheel scrolling
    // whenever that selection is above the viewport (#4594).
    app.work_surface.clamp_viewport(&rows);
    let max_offset = rows.len().saturating_sub(list_rows.max(1));
    app.work_surface.scroll_offset = app.work_surface.scroll_offset.min(max_offset);

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
        let label_budget = row_width.saturating_sub(prefix_width).max(1);
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
        legacy_scrollbar(
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

fn legacy_scrollbar(
    frame: &mut Frame,
    area: Rect,
    offset: usize,
    visible: usize,
    total: usize,
    app: &App,
) {
    let rail_height = area.height;
    if rail_height == 0 || total == 0 {
        return;
    }
    let thumb_height = ((usize::from(rail_height) * visible) / total)
        .max(1)
        .min(usize::from(rail_height));
    let max_offset = total.saturating_sub(visible).max(1);
    let max_start = usize::from(rail_height).saturating_sub(thumb_height);
    let thumb_start = offset.saturating_mul(max_start) / max_offset;
    let x = area.right().saturating_sub(1);
    for row in 0..usize::from(rail_height) {
        let in_thumb = row >= thumb_start && row < thumb_start.saturating_add(thumb_height);
        frame.buffer_mut()[(x, area.y.saturating_add(row as u16))]
            // Match the transcript rail exactly: a fine border track with a
            // brighter, narrow thumb. The old solid block looked like a
            // separate native scrollbar bolted onto the work surface.
            .set_symbol(if in_thumb { "┃" } else { "│" })
            .set_fg(if in_thumb {
                app.ui_theme.status_working
            } else {
                app.ui_theme.border
            })
            .set_bg(app.ui_theme.panel_bg);
    }
}

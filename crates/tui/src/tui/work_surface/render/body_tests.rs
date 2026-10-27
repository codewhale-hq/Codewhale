use super::*;
use codewhale_runtime::config::Config;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::path::PathBuf;

fn app() -> App {
    let mut app = App::new(
        crate::tui::test_support::test_tui_options(PathBuf::from(".")),
        &Config::default(),
    );
    app.ui_locale = codewhale_localization::Locale::En;
    app.ui_theme.text_body = Color::Rgb(29, 47, 83);
    app.ui_theme.panel_bg = Color::Rgb(13, 31, 59);
    app.ui_theme.border = Color::Rgb(71, 43, 101);
    app.ui_theme.status_working = Color::Rgb(47, 109, 67);
    app
}
fn paint(app: &mut App, area: Rect, old: bool) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(150, 24)).unwrap();
    app.viewport.interaction_targets.clear();
    terminal
        .draw(|frame| {
            for cell in &mut frame.buffer_mut().content {
                cell.set_symbol("~").set_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Rgb(11, 23, 37))
                        .add_modifier(Modifier::ITALIC),
                );
            }
            if old {
                super::body_legacy::render(frame, area, app);
            } else {
                render(frame, area, app);
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
#[test]
fn shared_native_body_matches_frozen_whole_buffer_actions_and_hover_receipts() {
    let mut app = app();
    for placement in [
        WorkSurfacePlacement::Top,
        WorkSurfacePlacement::Bottom,
        WorkSurfacePlacement::Left,
        WorkSurfacePlacement::Right,
    ] {
        for panel in RailPanel::ORDER
            .into_iter()
            // These two panels have dedicated renderers; Terminal was added
            // after the legacy body was frozen and has no old counterpart.
            .filter(|panel| !matches!(panel, RailPanel::Git | RailPanel::Terminal))
        {
            for width in [12, 40, 59, 71, 72, 80, 120] {
                for height in [1, 2, 3, 5, 12] {
                    app.work_surface.effective_placement = placement;
                    app.work_surface.panel = panel;
                    app.work_surface.explicit_view = true;
                    app.work_surface.focused = true;
                    app.work_surface.scroll_offset = usize::MAX;
                    let area = Rect::new(7, 5, width, height);
                    let expected = paint(&mut app, area, true);
                    let expected_geometry = (
                        app.work_surface.visible_rows,
                        app.work_surface.total_rows,
                        app.work_surface.scroll_offset,
                    );
                    let expected_hitboxes = app
                        .work_surface
                        .hitboxes
                        .iter()
                        .map(|box_| (box_.id.clone(), box_.row_y))
                        .collect::<Vec<_>>();
                    let expected_actions = app
                        .viewport
                        .interaction_targets
                        .iter()
                        .copied()
                        .collect::<Vec<_>>();
                    let expected_hover = app.sidebar_hover.sections.last().map(|section| {
                        (
                            section.content_area,
                            section.lines.clone(),
                            section.rows.clone(),
                        )
                    });
                    let actual = paint(&mut app, area, false);
                    assert_eq!(
                        actual, expected,
                        "placement={placement:?} panel={panel:?} area={area:?}"
                    );
                    assert_eq!(
                        (
                            app.work_surface.visible_rows,
                            app.work_surface.total_rows,
                            app.work_surface.scroll_offset
                        ),
                        expected_geometry
                    );
                    assert_eq!(
                        app.work_surface
                            .hitboxes
                            .iter()
                            .map(|box_| (box_.id.clone(), box_.row_y))
                            .collect::<Vec<_>>(),
                        expected_hitboxes
                    );
                    assert_eq!(
                        app.viewport
                            .interaction_targets
                            .iter()
                            .copied()
                            .collect::<Vec<_>>(),
                        expected_actions
                    );
                    assert_eq!(
                        app.sidebar_hover.sections.last().map(|section| (
                            section.content_area,
                            section.lines.clone(),
                            section.rows.clone()
                        )),
                        expected_hover
                    );
                }
            }
        }
    }
}
#[test]
fn shared_native_scroll_rail_preserves_custom_ground_and_existing_modifiers() {
    let app = app();
    for height in 0..=12 {
        for total in [0, 1, 8, 128] {
            let area = Rect::new(7, 5, 1, height);
            let mut expected = Buffer::empty(Rect::new(2, 3, 30, 20));
            for cell in &mut expected.content {
                cell.set_symbol("~").set_style(
                    Style::default()
                        .bg(Color::Green)
                        .add_modifier(Modifier::ITALIC | Modifier::REVERSED),
                );
            }
            let mut actual = expected.clone();
            // Exact old rail formula with the host's original foreground/background assignments.
            if height > 0 && total > 0 {
                let h = usize::from(height);
                let size = (h * 3 / total).max(1).min(h);
                let start =
                    2usize.saturating_mul(h.saturating_sub(size)) / total.saturating_sub(3).max(1);
                for row in 0..h {
                    let active = row >= start && row < start.saturating_add(size);
                    expected[(7, 5 + row as u16)]
                        .set_symbol(if active { "┃" } else { "│" })
                        .set_fg(if active {
                            app.ui_theme.status_working
                        } else {
                            app.ui_theme.border
                        })
                        .set_bg(app.ui_theme.panel_bg);
                }
            }
            codewhale_ratatui::WorkbarScrollbar {
                offset: 2,
                visible: 3,
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
            .paint(area, &mut actual);
            assert_eq!(actual, expected, "height={height} total={total}");
        }
    }
}

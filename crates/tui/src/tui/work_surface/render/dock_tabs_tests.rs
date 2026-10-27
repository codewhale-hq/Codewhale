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
    app.ui_theme.text_body = Color::Rgb(19, 41, 83);
    app.ui_theme.selection_bg = Color::Rgb(89, 31, 67);
    app.ui_theme.elevated_bg = Color::Rgb(37, 59, 23);
    app
}

fn paint(
    app: &mut App,
    area: Rect,
    legacy: bool,
) -> (
    Buffer,
    Vec<DockTabHitbox>,
    Vec<crate::tui::tideline::InteractionTarget>,
) {
    let mut terminal = Terminal::new(TestBackend::new(150, 12)).unwrap();
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
            if legacy {
                super::dock_tabs_legacy::render_dock_tabs(frame, area, app);
            } else {
                render_dock_tabs(frame, area, app);
            }
            register_dock_targets(app);
        })
        .unwrap();
    (
        terminal.backend().buffer().clone(),
        app.work_surface.dock_tabs.clone(),
        app.viewport.interaction_targets.iter().copied().collect(),
    )
}

#[test]
fn native_dock_tab_buffer_and_typed_actions_match_frozen_renderer() {
    let mut app = app();
    for placement in [
        WorkSurfacePlacement::Top,
        WorkSurfacePlacement::Bottom,
        WorkSurfacePlacement::Left,
        WorkSurfacePlacement::Right,
    ] {
        for panel in RailPanel::ORDER {
            for (focused, explicit) in [(false, false), (true, false), (true, true)] {
                for interaction in [
                    None,
                    Some(DockTabTarget::Close),
                    Some(DockTabTarget::Panel(RailPanel::Context)),
                    Some(DockTabTarget::Panel(RailPanel::Tasks)),
                ] {
                    app.work_surface.effective_placement = placement;
                    app.work_surface.panel = panel;
                    app.work_surface.focused = focused;
                    app.work_surface.explicit_view = explicit;
                    app.work_surface.hovered_tab = interaction;
                    app.work_surface.pressed_tab = interaction;
                    for width in [1, 3, 8, 12, 24, 39, 40, 59, 60, 61, 80, 120, 128] {
                        let area = Rect::new(7, 5, width, 3);
                        let expected = paint(&mut app, area, true);
                        let actual = paint(&mut app, area, false);
                        assert_eq!(
                            actual, expected,
                            "placement={placement:?} panel={panel:?} focused={focused} explicit={explicit} interaction={interaction:?} area={area:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_dock_zero_viewport_retires_every_painted_target() {
    let mut app = app();
    app.work_surface.effective_placement = WorkSurfacePlacement::Top;
    app.work_surface.panel = RailPanel::Context;
    assert!(!paint(&mut app, Rect::new(7, 5, 80, 3), false).1.is_empty());
    let actual = paint(&mut app, Rect::new(7, 5, 0, 3), false);
    assert!(actual.1.is_empty());
    assert!(actual.2.is_empty());
}

#[test]
fn jobs_tab_counts_one_running_shell_as_one_job() {
    let mut app = app();
    app.task_panel.push(crate::tui::app::TaskPanelEntry {
        exit_code: None,
        id: "shell_a1b2c3d4".to_string(),
        status: "running".to_string(),
        prompt_summary: "shell: ls | head -40".to_string(),
        duration_ms: Some(34_000),
        kind: crate::tui::app::TaskPanelEntryKind::Shell,
        stale: false,
        elapsed_since_output_ms: None,
        owner_agent_id: None,
        owner_agent_name: None,
        current_tool: None,
        role: None,
        files_touched: 0,
    });
    // The `▾ Shells 1` heading is a selectable door, not a second job.
    assert_eq!(dock_tab_count(&mut app, RailPanel::Background), Some(1));
}

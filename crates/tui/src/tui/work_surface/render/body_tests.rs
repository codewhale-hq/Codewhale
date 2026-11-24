use super::*;
use codewhale_runtime::config::Config;
use ratatui::{buffer::Buffer, style::Color};
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

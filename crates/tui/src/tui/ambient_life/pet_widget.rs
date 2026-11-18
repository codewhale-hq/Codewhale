//! The shared braille renderer, promoted from the portable pet study's TUI.
//! It owns no clock, telemetry, or motion. The habitat owns placement and ink.

use super::pet_sim::{PetSim, PetState, braille};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};

pub struct PetWidget<'a> {
    pub sim: &'a PetSim,
    pub state: &'a PetState,
}

impl Widget for PetWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let frame = self.sim.frame;
        let label = format!(
            "{} · {}{}",
            frame.channel,
            frame.arch,
            if frame.hollow { " · resting" } else { "" }
        );
        // The creature and its non-colour cue are one unit. A narrow surface
        // withholds both instead of silently dropping uncertainty or the gait.
        if area.height < 4 || usize::from(area.width) < label.chars().count() {
            return;
        }
        let tank = Rect {
            height: area.height - 1,
            ..area
        };
        let grid = braille(
            self.sim,
            tank.width as usize,
            tank.height as usize,
            self.state,
        );
        let fg = Color::Rgb(
            (frame.r * frame.alpha).clamp(0.0, 255.0) as u8,
            (frame.g * frame.alpha).clamp(0.0, 255.0) as u8,
            (frame.b * frame.alpha).clamp(0.0, 255.0) as u8,
        );
        render_grid(area, buf, &grid, &label, Style::default().fg(fg));
    }
}

/// Shared paint path for the Rust cameo and the embedded world's live raster.
/// A tiny viewport keeps the complete text cue before spending cells on dots.
pub(crate) fn render_grid(area: Rect, buf: &mut Buffer, grid: &[u8], label: &str, style: Style) {
    codewhale_ratatui::BrailleFrame {
        cells: grid,
        caption: label,
        style,
    }
    .render(area, buf);
}

#[cfg(test)]
#[path = "pet_widget_tests.rs"]
mod pet_widget_tests;

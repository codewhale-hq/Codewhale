//! The composer's image thumbnail.
//!
//! Placed between the pending-input preview and the composer, so a pasted
//! screenshot is visible before it is sent. The picture itself is drawn by
//! [`crate::tui::image_preview`] as cell-sized half blocks, which every
//! truecolor terminal can render - no kitty graphics or sixel required.
//!
//! Zero rows while nothing is attached, while the decode is still running, or
//! when the terminal is in ASCII-safe mode. The transcript therefore never
//! loses rows to a preview that cannot be drawn.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::tui::app::App;
use crate::tui::widgets::Renderable;
use codewhale_palette as palette;

/// Upper half block: foreground paints the top pixel, background the bottom.
const UPPER_HALF: &str = "\u{2580}";
/// Lower half block, for a cell whose top pixel is transparent.
const LOWER_HALF: &str = "\u{2584}";

/// The image files the draft currently attaches, in attachment order.
#[must_use]
pub fn attached_image_paths(app: &App) -> Vec<PathBuf> {
    codewhale_core::media_attachment_references(&app.input)
        .into_iter()
        .filter(|reference| reference.kind == "image")
        .map(|reference| PathBuf::from(reference.path))
        .collect()
}

/// Widget drawing the first attached image as a cell thumbnail.
pub struct AttachmentPreviewWidget<'a> {
    app: &'a App,
    path: PathBuf,
}

impl<'a> AttachmentPreviewWidget<'a> {
    /// `None` when the draft has no image to preview.
    #[must_use]
    pub fn for_app(app: &'a App) -> Option<Self> {
        let path = attached_image_paths(app).into_iter().next()?;
        Some(Self { app, path })
    }
}

impl Renderable for AttachmentPreviewWidget<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let Some(thumbnail) = self.app.attachment_preview.get(&self.path) else {
            return;
        };
        // A split pane can be narrower than the thumbnail's natural width.
        // Decimate the sampled grid instead of dropping the picture: the
        // larger of the two overflow factors keeps the aspect ratio, so a
        // narrow dock shows a smaller thumbnail rather than nothing.
        let step = thumbnail
            .cols
            .div_ceil(area.width.max(1))
            .max(thumbnail.rows.div_ceil(area.height.max(1)))
            .max(1);
        let cols = thumbnail.cols / step;
        let rows = thumbnail.rows / step;
        if cols == 0 || rows == 0 {
            return;
        }
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = thumbnail.cell(col * step, row * step) else {
                    continue;
                };
                let Some(buffer_cell) = buf.cell_mut((area.x + col, area.y + row)) else {
                    continue;
                };
                match (cell.upper, cell.lower) {
                    (Some(upper), Some(lower)) => {
                        buffer_cell.set_symbol(UPPER_HALF);
                        buffer_cell
                            .set_fg(Color::Rgb(upper[0], upper[1], upper[2]))
                            .set_bg(Color::Rgb(lower[0], lower[1], lower[2]));
                    }
                    (Some(upper), None) => {
                        buffer_cell.set_symbol(UPPER_HALF);
                        buffer_cell.set_fg(Color::Rgb(upper[0], upper[1], upper[2]));
                    }
                    (None, Some(lower)) => {
                        buffer_cell.set_symbol(LOWER_HALF);
                        buffer_cell.set_fg(Color::Rgb(lower[0], lower[1], lower[2]));
                    }
                    (None, None) => {}
                }
            }
        }
        // The caption row names the file so several screenshots stay
        // distinguishable when only one is drawn. Its size is the one on
        // screen, not the decode grid.
        if area.height > rows {
            let name = self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image");
            let label = format!(
                "{name}  {}x{}",
                cols.saturating_mul(2) * step,
                rows.saturating_mul(2) * step
            );
            buf.set_stringn(
                area.x,
                area.y + rows,
                label,
                usize::from(area.width),
                Style::default().fg(palette::TEXT_DIM),
            );
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        if width == 0 || crate::tui::color_compat::ascii_safe_enabled() {
            return 0;
        }
        let Some(thumbnail) = self.app.attachment_preview.get(&self.path) else {
            return 0;
        };
        if thumbnail.cols == 0 || thumbnail.rows == 0 {
            return 0;
        }
        // Same decimation the render uses, so the reserved rows match what
        // is actually painted on a narrow terminal.
        let step = thumbnail.cols.div_ceil(width).max(1);
        let rows = thumbnail.rows / step;
        if rows == 0 {
            return 0;
        }
        // One caption row under the picture.
        rows.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::tui::test_support::test_tui_options;
    use std::path::Path;

    fn app_with_attachment(path: &Path) -> App {
        let mut app = App::new(
            test_tui_options("."),
            &codewhale_runtime::config::Config::default(),
        );
        app.input = format!("[Attached image: 8x4 PNG (2KB) at {}]", path.display());
        app
    }

    #[test]
    fn draft_attachments_are_read_from_the_placeholder_line() {
        let app = app_with_attachment(Path::new("/tmp/pasted.png"));
        assert_eq!(
            attached_image_paths(&app),
            vec![PathBuf::from("/tmp/pasted.png")]
        );
    }

    #[test]
    fn a_draft_without_an_attachment_has_no_widget() {
        let app = App::new(
            test_tui_options("."),
            &codewhale_runtime::config::Config::default(),
        );
        assert!(AttachmentPreviewWidget::for_app(&app).is_none());
    }

    #[test]
    fn an_undecoded_attachment_takes_no_rows() {
        let app = app_with_attachment(Path::new("/tmp/never-decoded.png"));
        let widget = AttachmentPreviewWidget::for_app(&app).expect("widget");
        assert_eq!(widget.desired_height(80), 0);
    }
}

//! The composer's image thumbnails.
//!
//! Placed between the pending-input preview and the composer, so pasted
//! screenshots are visible before they are sent. The pictures are drawn by
//! [`crate::tui::image_preview`] as cell-sized half blocks, which every
//! truecolor terminal can render - no kitty graphics or sixel required.
//!
//! Attachments are laid out left to right and share the available width, so a
//! draft holding several screenshots previews all of them instead of only the
//! first. Zero rows while nothing is attached, while the decode is still
//! running, or when the terminal is in ASCII-safe mode, so the transcript
//! never loses rows to a preview that cannot be drawn.

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

/// Narrowest a single thumbnail may get before it stops reading as a picture.
const MIN_SLOT_COLS: u16 = 6;

/// The image files the draft currently attaches, in attachment order.
#[must_use]
pub fn attached_image_paths(app: &App) -> Vec<PathBuf> {
    codewhale_core::media_attachment_references(&app.input)
        .into_iter()
        .filter(|reference| reference.kind == "image")
        .map(|reference| PathBuf::from(reference.path))
        .collect()
}

/// Widget drawing every attached image as a row of cell thumbnails.
pub struct AttachmentPreviewWidget<'a> {
    app: &'a App,
    paths: Vec<PathBuf>,
}

impl<'a> AttachmentPreviewWidget<'a> {
    /// `None` when the draft has no image to preview.
    #[must_use]
    pub fn for_app(app: &'a App) -> Option<Self> {
        let paths = attached_image_paths(app);
        (!paths.is_empty()).then_some(Self { app, paths })
    }

    /// Width one thumbnail gets when `count` pictures share `width`.
    fn slot_width(width: u16, count: usize) -> u16 {
        let count = u16::try_from(count.max(1)).unwrap_or(u16::MAX);
        (width / count).max(MIN_SLOT_COLS)
    }
}

impl Renderable for AttachmentPreviewWidget<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let slot = Self::slot_width(area.width, self.paths.len());
        let right = area.x.saturating_add(area.width);
        let mut max_rows = 0u16;
        let mut drawn = 0usize;

        for (index, path) in self.paths.iter().enumerate() {
            let Some(thumbnail) = self.app.attachment_preview.get(path) else {
                continue;
            };
            let x = area.x.saturating_add(
                u16::try_from(index)
                    .unwrap_or(u16::MAX)
                    .saturating_mul(slot),
            );
            if x >= right {
                break;
            }
            let width = slot.min(right - x);
            // A split pane can be narrower than a thumbnail's natural width.
            // Decimate the sampled grid instead of dropping the picture: the
            // larger of the two overflow factors keeps the aspect ratio, so a
            // narrow slot shows a smaller thumbnail rather than nothing.
            let step = thumbnail
                .cols
                .div_ceil(width.max(1))
                .max(thumbnail.rows.div_ceil(area.height.max(1)))
                .max(1);
            let cols = thumbnail.cols / step;
            let rows = thumbnail.rows / step;
            if cols == 0 || rows == 0 {
                continue;
            }
            drawn += 1;
            max_rows = max_rows.max(rows);

            for row in 0..rows {
                for col in 0..cols {
                    let Some(cell) = thumbnail.cell(col * step, row * step) else {
                        continue;
                    };
                    let Some(buffer_cell) = buf.cell_mut((x + col, area.y + row)) else {
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
        }

        if drawn == 0 || area.height <= max_rows {
            return;
        }
        let label = if self.paths.len() == 1 {
            let name = self
                .paths
                .first()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("image");
            name.to_string()
        } else {
            format!("{} images", self.paths.len())
        };
        buf.set_stringn(
            area.x,
            area.y + max_rows,
            label,
            usize::from(area.width),
            Style::default().fg(palette::TEXT_DIM),
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        if width == 0 || crate::tui::color_compat::ascii_safe_enabled() {
            return 0;
        }
        let slot = Self::slot_width(width, self.paths.len());
        let mut max_rows = 0u16;
        for path in &self.paths {
            let Some(thumbnail) = self.app.attachment_preview.get(path) else {
                continue;
            };
            if thumbnail.cols == 0 || thumbnail.rows == 0 {
                continue;
            }
            // Same decimation the render uses, so the reserved rows match what
            // is actually painted.
            let step = thumbnail.cols.div_ceil(slot).max(1);
            max_rows = max_rows.max(thumbnail.rows / step);
        }
        if max_rows == 0 {
            return 0;
        }
        // One caption row under the pictures.
        max_rows.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::tui::test_support::test_tui_options;

    fn app_with_attachments(paths: &[&str]) -> App {
        let mut app = App::new(
            test_tui_options("."),
            &codewhale_runtime::config::Config::default(),
        );
        app.input = paths
            .iter()
            .map(|path| format!("[Attached image: 8x4 PNG (2KB) at {path}]"))
            .collect::<Vec<_>>()
            .join("\n");
        app
    }

    #[test]
    fn every_attached_image_is_read_from_the_placeholder_lines() {
        let app = app_with_attachments(&["/tmp/one.png", "/tmp/two.png"]);
        assert_eq!(
            attached_image_paths(&app),
            vec![PathBuf::from("/tmp/one.png"), PathBuf::from("/tmp/two.png")]
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
        let app = app_with_attachments(&["/tmp/never-decoded.png"]);
        let widget = AttachmentPreviewWidget::for_app(&app).expect("widget");
        assert_eq!(widget.desired_height(80), 0);
    }

    #[test]
    fn slots_share_the_width_and_never_fall_below_the_floor() {
        assert_eq!(AttachmentPreviewWidget::slot_width(120, 3), 40);
        assert_eq!(AttachmentPreviewWidget::slot_width(120, 1), 120);
        assert_eq!(AttachmentPreviewWidget::slot_width(20, 8), MIN_SLOT_COLS);
    }

    #[test]
    fn a_widget_keeps_every_attached_path() {
        let app = app_with_attachments(&["/tmp/a.png", "/tmp/b.png", "/tmp/c.png"]);
        let widget = AttachmentPreviewWidget::for_app(&app).expect("widget");
        assert_eq!(widget.paths.len(), 3);
    }
}

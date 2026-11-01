//! Bounded RGBA avatar tiles painted with terminal half-blocks. This renderer
//! neither loads plugins nor owns a timer. Hosts supply reviewed pixels and
//! sample an action with the same Director used by the rest of their UI.
use crate::{
    Paint, Role, Theme,
    color::{self, ColorDepth},
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

/// Row-major RGBA pixels within each tile, tiles in manifest order.
/// Hosts decode PNG once and concatenate its tiles; terminal authors can also
/// ship predecoded tiles. Construction validates the complete byte geometry.
#[derive(Clone, Copy)]
pub struct Sprite<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
    frame: usize,
}
impl<'a> Sprite<'a> {
    /// One already-decoded RGBA image, without an avatar manifest. Useful for
    /// live vector scenes; shares the same half-block and capability painter.
    pub fn image(pixels: &'a [u8], width: usize, height: usize) -> Result<Self, String> {
        Self::tiles(pixels, width, height, 0, 1)
    }

    pub fn new(
        pack: &crate::avatar::Pack,
        pixels: &'a [u8],
        width: usize,
        height: usize,
        frame: usize,
    ) -> Result<Self, String> {
        pack.validate()?;
        let count = usize::from(pack.columns) * usize::from(pack.rows) * pack.atlases.len();
        Self::tiles(pixels, width, height, frame, count)
    }
    pub fn page(
        pack: &crate::avatar::Pack,
        pixels: &'a [u8],
        width: usize,
        height: usize,
        frame: usize,
    ) -> Result<Self, String> {
        pack.validate()?;
        if pack.page(frame) >= pack.atlases.len() {
            return Err("avatar frame exceeds page count".into());
        }
        Self::tiles(
            pixels,
            width,
            height,
            pack.local_frame(frame),
            usize::from(pack.columns) * usize::from(pack.rows),
        )
    }
    fn tiles(
        pixels: &'a [u8],
        width: usize,
        height: usize,
        frame: usize,
        count: usize,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > 2048
            || height > 2048
            || frame >= count
            || width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(4))
                .and_then(|n| n.checked_mul(count))
                != Some(pixels.len())
            || pixels.len() > 16 * 1024 * 1024
        {
            return Err("avatar pixels do not match bounded tile dimensions".into());
        }
        Ok(Self {
            pixels,
            width,
            height,
            frame,
        })
    }
    fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let offset = ((self.frame * self.height + y) * self.width + x) * 4;
        self.pixels[offset..offset + 4]
            .try_into()
            .expect("validated RGBA pixel")
    }

    fn sample(&self, x: usize, y: usize, up: usize, down: usize) -> [u8; 4] {
        self.pixel(
            ((2 * x + 1) * down / (2 * up)).min(self.width - 1),
            ((2 * y + 1) * down / (2 * up)).min(self.height - 1),
        )
    }
}
impl Paint for Sprite<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        // One cell is approximately twice as tall as wide. Fit without changing
        // the artwork's aspect ratio and never allocate from terminal geometry.
        let (columns, pixel_rows) = (usize::from(area.width), usize::from(area.height) * 2);
        let (up, down) = if self.width <= columns && self.height <= pixel_rows {
            ((columns / self.width).min(pixel_rows / self.height), 1)
        } else {
            (
                1,
                self.width
                    .div_ceil(columns)
                    .max(self.height.div_ceil(pixel_rows)),
            )
        };
        let height = (self.height * up).div_ceil(down);
        let width = (self.width * up).div_ceil(down) as u16;
        let rows = height.div_ceil(2) as u16;
        let left = area.x + (area.width - width) / 2;
        let top = area.y + (area.height - rows) / 2;
        let colored = theme.caps().paints_tokens() && !theme.ascii();
        for y in 0..rows {
            for x in 0..width {
                let sample = |half: usize| {
                    let y = usize::from(y) * 2 + half;
                    if y < height {
                        self.sample(usize::from(x), y, up, down)
                    } else {
                        [0; 4]
                    }
                };
                let a = sample(0);
                let b = sample(1);
                if a[3] < 40 && b[3] < 40 {
                    continue;
                }
                let cell = &mut buf[(left + x, top + y)];
                if !colored {
                    // A legible one-ink density rendering for NO_COLOR, unknown
                    // ground and ASCII-safe profiles; no forced palette escapes.
                    let alpha = u16::from(a[3]) + u16::from(b[3]);
                    let shade = (u16::from(a[0])
                        + u16::from(a[1])
                        + u16::from(a[2])
                        + u16::from(b[0])
                        + u16::from(b[1])
                        + u16::from(b[2]))
                        / 6;
                    let symbol = if alpha < 160 {
                        "."
                    } else if shade > 220 {
                        ":"
                    } else if shade > 130 {
                        "+"
                    } else {
                        "#"
                    };
                    cell.set_symbol(symbol).set_style(theme.fg(Role::Accent));
                    continue;
                }
                let bg = color::resolvable_rgb(cell.bg);
                let blend = |p: [u8; 4]| {
                    let rgb = if let Some((r, g, b)) = bg {
                        let mix = |fg: u8, bg: u8| {
                            ((u32::from(fg) * u32::from(p[3])
                                + u32::from(bg) * (255 - u32::from(p[3])))
                                / 255) as u8
                        };
                        (mix(p[0], r), mix(p[1], g), mix(p[2], b))
                    } else {
                        (p[0], p[1], p[2])
                    };
                    if theme.caps().depth == ColorDepth::Ansi256 {
                        Color::Indexed(color::rgb_to_ansi256(rgb.0, rgb.1, rgb.2))
                    } else {
                        Color::Rgb(rgb.0, rgb.1, rgb.2)
                    }
                };
                let background = cell.bg;
                match (a[3] >= 40, b[3] >= 40) {
                    (true, true) => {
                        cell.set_symbol("▀")
                            .set_style(Style::default().fg(blend(a)).bg(blend(b)));
                    }
                    (true, false) => {
                        cell.set_symbol("▀")
                            .set_style(Style::default().fg(blend(a)).bg(background));
                    }
                    (false, true) => {
                        cell.set_symbol("▄")
                            .set_style(Style::default().fg(blend(b)).bg(background));
                    }
                    _ => {}
                }
            }
        }
    }
}

use std::collections::BTreeMap;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::{Paint, Role, Theme, color, text};

#[derive(Clone, Copy, Debug)]
pub struct DotWhale<'a> {
    points: &'a [[f64; 2]],
    materials: &'a [[f64; 4]],
    caption: &'a str,
    action_id: Option<&'a str>,
    hollow: bool,
}

impl<'a> DotWhale<'a> {
    #[must_use]
    pub const fn new(points: &'a [[f64; 2]], materials: &'a [[f64; 4]]) -> Self {
        Self {
            points,
            materials,
            caption: "Activity unobserved",
            action_id: None,
            hollow: false,
        }
    }

    #[must_use]
    pub const fn caption(mut self, caption: &'a str) -> Self {
        self.caption = caption;
        self
    }

    #[must_use]
    pub const fn action_id(mut self, action_id: Option<&'a str>) -> Self {
        self.action_id = action_id;
        self
    }

    #[must_use]
    pub const fn hollow(mut self, hollow: bool) -> Self {
        self.hollow = hollow;
        self
    }

    fn valid(&self) -> bool {
        !self.points.is_empty()
            && self.points.len() <= 980
            && self.points.len() == self.materials.len()
            && self
                .points
                .iter()
                .flatten()
                .all(|n| n.is_finite() && (-1.0..=1.0).contains(n))
            && self.materials.iter().all(|m| {
                m[..3]
                    .iter()
                    .all(|n| n.is_finite() && (0.0..=255.0).contains(n))
                    && m[3].is_finite()
                    && (0.0..=1.0).contains(&m[3])
            })
    }

    fn label(&self, theme: &Theme) -> String {
        let caption = text::display_safe(self.caption);
        let caption = text::truncate(&caption, 192, theme.ascii());
        let Some(action) = self.action_id.filter(|id| !id.is_empty()) else {
            return caption.into_owned();
        };
        let action = text::display_safe(action);
        let action = text::truncate(&action, 256, theme.ascii());
        if caption.is_empty() {
            action.into_owned()
        } else {
            format!(
                "{caption} {} {action}",
                if theme.ascii() { "/" } else { "·" }
            )
        }
    }
}

#[derive(Default)]
struct DotCell {
    bits: u8,
    pigment: [f64; 3],
    weight: f64,
    count: u16,
}

fn caption(label: &str, area: Rect, buf: &mut Buffer, style: Style) {
    let width = area.width.min(512);
    if width == 0 || area.height == 0 {
        return;
    }
    let paragraph = Paragraph::new(label)
        .style(style)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
    let height = paragraph.line_count(width).min(usize::from(area.height)) as u16;
    if height == 0 {
        return;
    }
    let x = u32::from(area.x) + u32::from((area.width - width) / 2);
    let y = u32::from(area.y) + u32::from((area.height - height) / 2);
    let mut local = Buffer::empty(Rect::new(0, 0, width, height));
    paragraph.render(local.area, &mut local);
    for row in 0..height {
        for col in 0..width {
            let (Ok(x), Ok(y)) = (
                u16::try_from(x + u32::from(col)),
                u16::try_from(y + u32::from(row)),
            ) else {
                continue;
            };
            if let Some(cell) = buf.cell_mut((x, y)) {
                let background = cell.bg;
                *cell = local[(col, row)].clone();
                cell.set_bg(background);
            }
        }
    }
}

impl Paint for DotWhale<'_> {
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        let label = self.label(theme);
        let rows = Paragraph::new(label)
            .wrap(Wrap { trim: false })
            .line_count(width.min(512))
            .min(512) as u16;
        if theme.ascii() || width < 12 || !self.valid() {
            rows
        } else {
            rows.saturating_add((width / 2).clamp(4, 20))
        }
    }

    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.is_empty() || area.intersection(buf.area).is_empty() {
            return;
        }
        let label = self.label(theme);
        let label_height = Paragraph::new(label.as_str())
            .wrap(Wrap { trim: false })
            .line_count(area.width.min(512))
            .min(usize::from(area.height)) as u16;
        let art_height = area.height.saturating_sub(label_height);
        if theme.ascii() || area.width < 12 || art_height < 4 || !self.valid() {
            caption(&label, area, buf, theme.fg(Role::Primary));
            return;
        }
        let width = f64::from(area.width) * 2.0;
        let height = f64::from(art_height) * 4.0;
        let scale = (width - 1.0).min(height - 1.0) / 2.0;
        let bits = [[1, 8], [2, 16], [4, 32], [64, 128]];
        let mut cells: BTreeMap<(u16, u16), DotCell> = BTreeMap::new();
        for (point, material) in self.points.iter().zip(self.materials) {
            if material[3] <= 0.01 {
                continue;
            }
            let x = ((width - 1.0) / 2.0 + point[0] * scale).round() as u32;
            let y = ((height - 1.0) / 2.0 + point[1] * scale).round() as u32;
            let (Ok(col), Ok(row)) = (
                u16::try_from(u32::from(area.x) + x / 2),
                u16::try_from(u32::from(area.y) + y / 4),
            ) else {
                continue;
            };
            if !buf.area.contains((col, row).into()) {
                continue;
            }
            let cell = cells.entry((col, row)).or_default();
            cell.bits |= bits[(y % 4) as usize][(x % 2) as usize];
            for (sum, value) in cell.pigment.iter_mut().zip(material) {
                *sum += value * material[3];
            }
            cell.weight += material[3];
            cell.count += 1;
        }
        for ((x, y), dot) in cells {
            if let Some(cell) = buf.cell_mut((x, y)) {
                let [r, g, b] = dot.pigment.map(|sum| (sum / dot.weight).round() as u8);
                let mut style = color::prepared_ink(
                    [r, g, b],
                    (dot.weight / f64::from(dot.count)) as f32,
                    cell.bg,
                    theme,
                );
                if self.hollow {
                    style = style.add_modifier(Modifier::DIM);
                }
                let glyph = char::from_u32(0x2800 + u32::from(dot.bits)).unwrap_or(' ');
                let mut encoded = [0; 4];
                cell.set_symbol(glyph.encode_utf8(&mut encoded))
                    .set_style(style);
            }
        }
        caption(
            &label,
            Rect::new(
                area.x,
                area.y.saturating_add(art_height),
                area.width,
                label_height,
            ),
            buf,
            theme.fg(Role::Primary),
        );
    }
}

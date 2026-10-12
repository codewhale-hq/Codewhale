//! The GPUI cove/contour paint order in a bounded RGBA plane. The existing
//! Sprite adapter handles terminal half-blocks, alpha and color capabilities.

use super::{CoveScene, Layer, Path, Role, ink::Fill, rig::holes_for, scene::segments};

pub(crate) const MAX_SIDE: usize = 160;

struct Canvas {
    side: usize,
    // Premultiplied RGBA: averaging transparent edges must not darken them.
    pixels: Vec<[f64; 4]>,
}

impl Canvas {
    fn fill(&mut self, paths: &[Path], fill: Fill, alpha: f64) {
        if !alpha.is_finite() || alpha <= 0. {
            return;
        }
        let alpha = alpha.min(1.);
        let side = self.side as f64;
        let scale = side / 124.;
        let edges: Vec<_> = paths
            .iter()
            .filter(|path| {
                path.iter()
                    .flat_map(super::rig::Cmd::coords)
                    .all(|v| v.is_finite())
            })
            .flat_map(segments)
            .map(|[a, b, c, d]| {
                [
                    side / 2. + a * scale,
                    side / 2. + b * scale,
                    side / 2. + c * scale,
                    side / 2. + d * scale,
                ]
            })
            .collect();
        let mut crossings = Vec::new();
        for y in 0..self.side {
            let cy = y as f64 + 0.5;
            crossings.clear();
            for &[x1, y1, x2, y2] in &edges {
                if (y1 > cy) != (y2 > cy) {
                    crossings.push(x1 + (cy - y1) * (x2 - x1) / (y2 - y1));
                }
            }
            crossings.sort_by(f64::total_cmp);
            for pair in crossings.as_chunks::<2>().0 {
                let left = (pair[0] - 0.5).ceil().clamp(0., side) as usize;
                let right = (pair[1] - 0.5).ceil().clamp(0., side) as usize;
                for x in left..right {
                    let t = (((x as f64 + cy + 0.5 - side) / scale + 100.) / 200.).clamp(0., 1.);
                    let channel = |hex: u32, shift: u32| f64::from((hex >> shift) & 255_u32);
                    let color = match fill {
                        Fill::Solid(hex) => [16, 8, 0].map(|s| channel(hex, s)),
                        Fill::Gradient([a, b]) => {
                            [16, 8, 0].map(|s| channel(a, s) + (channel(b, s) - channel(a, s)) * t)
                        }
                    };
                    let pixel = &mut self.pixels[y * self.side + x];
                    for (dst, src) in pixel[..3].iter_mut().zip(color) {
                        *dst = src * alpha + *dst * (1. - alpha);
                    }
                    pixel[3] = alpha + pixel[3] * (1. - alpha);
                }
            }
        }
    }

    fn layers(&mut self, layers: &[Layer], opacity: f64, current_ink: Option<u32>) {
        for layer in layers {
            let color = if layer.color == 0xf7fcfb {
                current_ink.unwrap_or(layer.color)
            } else {
                layer.color
            };
            self.fill(&layer.paths, Fill::Solid(color), layer.alpha * opacity);
        }
    }
}

/// Square design space at one sample per pixel, so shape edges stay hard on a
/// half-block grid instead of blending into the terminal background.
/// Public terminal rectangles never control an unbounded allocation.
pub(crate) fn render(scene: &CoveScene, side: usize, dark: bool) -> Vec<u8> {
    if !(1..=MAX_SIDE).contains(&side) {
        return Vec::new();
    }
    let samples = 1;
    let mut canvas = Canvas {
        side: side * samples,
        pixels: vec![[0.; 4]; side * side * samples * samples],
    };
    canvas.layers(&scene.behind, if dark { 0.68 } else { 0.9 }, None);
    for shape in &scene.parts.shapes {
        if shape.role == Role::Cutout {
            continue;
        }
        if let Some(fill) = super::ink::fill(shape.role, dark) {
            let mut paths = vec![shape.path.clone()];
            paths.extend(holes_for(&shape.id, &scene.parts.shapes, false));
            canvas.fill(&paths, fill, shape.opacity);
        }
    }
    canvas.layers(&scene.front, 0.82, (!dark).then_some(0x3e7789));
    let mut out = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        for x in 0..side {
            let mut rgba = [0.; 4];
            for dy in 0..samples {
                for dx in 0..samples {
                    let p = canvas.pixels[(y * samples + dy) * canvas.side + x * samples + dx];
                    for (dst, src) in rgba.iter_mut().zip(p) {
                        *dst += src / (samples * samples) as f64;
                    }
                }
            }
            for c in &rgba[..3] {
                out.push(if rgba[3] > 0. {
                    (c / rgba[3]).round() as u8
                } else {
                    0
                });
            }
            out.push((rgba[3] * 255.).round() as u8);
        }
    }
    out
}

/*
Adapted from Hmbown/CodeWhale crates/tui/src/tui/ambient_life/native_poses.rs
at a79ce5c4d5ed1a5f7032185710c27343a900351c. Authored native dot poses.

MIT License

Copyright (c) 2024-2025 DeepSeek-TUI Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

//! Bounded native poses for the habitat: fish, minnow, jellyfish, whale and
//! a single-dot mote. The ASCII fallback keeps fish, jellyfish and bubbles.
//! These are authored dot poses, not another simulation. The habitat owns
//! travel, collision and lifetime from caller-supplied elapsed time. Five tiny
//! immutable tables retain their rows so a frame borrows glyphs without
//! allocating strings. Braille remains discrete: half-column / quarter-row
//! motion is not pixels.

use crate::whale::BITS as BRAILLE_BITS;
use std::sync::LazyLock;

// Fish: 2 directions × 4 tail poses × 2 horizontal × 2 vertical dot offsets.
static FISH: LazyLock<Vec<String>> = LazyLock::new(|| {
    (0..32)
        .map(|index| {
            let dy = index % 2;
            let dx = index / 2 % 2;
            let pose = index / 4 % 4;
            let right = index / 16 != 0;
            let mut dots = Vec::from([(2, 1), (3, 0), (4, 0), (5, 1), (6, 1), (3, 2), (4, 2)]);
            // A small forked tail opens, folds and opens; no blinking eye or flash.
            dots.extend(match pose {
                0 => [(0, 0), (0, 2), (1, 1)],
                1 => [(0, 0), (1, 1), (1, 2)],
                2 => [(1, 0), (0, 1), (1, 2)],
                _ => [(1, 0), (1, 1), (0, 2)],
            });
            if !right {
                for point in &mut dots {
                    point.0 = 6 - point.0;
                }
            }
            raster(&dots, 4, 1, dx, dy).remove(0)
        })
        .collect()
});

// Jelly: 16 bell/tentacle poses × 2 horizontal × 4 vertical dot offsets.
// Eight-by-eight source dots plus offsets fit the existing 5×3-cell habitat.
static JELLY: LazyLock<Vec<[String; 3]>> = LazyLock::new(|| {
    (0..128)
        .map(|index| {
            let dy = index % 4;
            let dx = index / 4 % 2;
            let phase = (index / 8) as f64 / 16.0 * std::f64::consts::TAU;
            let contracted = (1.0 - phase.cos()) * 0.5;
            let inset = usize::from(contracted > 0.55);
            let mut dots = Vec::with_capacity(24);
            // An open contour, never a solid luminous block.
            dots.extend([
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (1 + inset, 1),
                (6 - inset, 1),
            ]);
            for x in inset..8 - inset {
                dots.push((x, 2));
            }
            for y in 3..8 {
                // Wave travels down both arms after the bell; columns are offset.
                let lag = (y - 2) as f64 * 0.45 + 0.75;
                let left = (2.0 + (phase - lag).sin() * 0.9).round() as usize;
                let right = (5.0 + (phase - lag - 0.8).sin() * 0.9).round() as usize;
                dots.extend([(left, y), (right, y)]);
            }
            let mut rows = raster(&dots, 5, 3, dx, dy).into_iter();
            std::array::from_fn(|_| rows.next().expect("three jelly rows"))
        })
        .collect()
});

static MINNOW: LazyLock<Vec<String>> = LazyLock::new(|| {
    (0..16)
        .map(|index| {
            let dy = index % 2;
            let dx = index / 2 % 2;
            let pose = index / 4 % 2;
            let right = index / 8 != 0;
            let mut dots = Vec::from([(2, 1), (3, 1), (4, 1), (3, 0), (3, 2)]);
            dots.extend(if pose == 0 {
                [(0, 0), (1, 1), (0, 2)]
            } else {
                [(1, 0), (0, 1), (1, 2)]
            });
            if !right {
                for point in &mut dots {
                    point.0 = 4 - point.0;
                }
            }
            raster(&dots, 3, 1, dx, dy).remove(0)
        })
        .collect()
});

static WHALE: LazyLock<Vec<[String; 3]>> = LazyLock::new(|| {
    (0..64)
        .map(|index| {
            let dy = index % 4;
            let dx = index / 4 % 2;
            let pose = index / 8 % 4;
            let right = index / 32 != 0;
            let mut dots = Vec::from([
                (8, 1),
                (9, 1),
                (10, 1),
                (11, 1),
                (12, 1),
                (6, 2),
                (7, 2),
                (13, 2),
                (14, 2),
                (5, 3),
                (15, 3),
                (16, 4),
                (17, 5),
                (4, 4),
                (16, 6),
                (5, 6),
                (6, 6),
                (7, 6),
                (8, 6),
                (9, 6),
                (10, 6),
                (11, 6),
                (12, 6),
                (13, 6),
                (14, 6),
                (15, 6),
                (10, 7),
                (11, 7),
                (3, 4),
                (2, 4),
            ]);
            dots.extend(match pose {
                0 => [(1, 3), (0, 2), (1, 2), (0, 1)],
                1 => [(1, 4), (0, 3), (1, 3), (0, 4)],
                2 => [(1, 5), (0, 5), (1, 6), (0, 6)],
                _ => [(1, 4), (0, 4), (1, 5), (0, 5)],
            });
            if !right {
                for point in &mut dots {
                    point.0 = 17 - point.0;
                }
            }
            let mut rows = raster(&dots, 10, 3, dx, dy).into_iter();
            std::array::from_fn(|_| rows.next().expect("three whale rows"))
        })
        .collect()
});

static MOTE: LazyLock<Vec<String>> = LazyLock::new(|| {
    (0..8)
        .map(|index| raster(&[(index % 2, index / 2)], 1, 1, 0, 0).remove(0))
        .collect()
});

fn raster(
    dots: &[(usize, usize)],
    width: usize,
    height: usize,
    dx: usize,
    dy: usize,
) -> Vec<String> {
    let mut cells = vec![0u8; width * height];
    for &(x, y) in dots {
        let (x, y) = (x + dx, y + dy);
        assert!(
            x < width * 2 && y < height * 4,
            "native pose escaped its habitat"
        );
        cells[y / 4 * width + x / 2] |= BRAILLE_BITS[y % 4][x % 2];
    }
    cells
        .chunks_exact(width)
        .map(|row| {
            row.iter()
                .map(|bits| {
                    if *bits == 0 {
                        ' '
                    } else {
                        char::from_u32(0x2800 + u32::from(*bits)).expect("braille dot mask")
                    }
                })
                .collect()
        })
        .collect()
}

pub(super) fn fish(right: bool, pose: usize, dx: usize, dy: usize) -> &'static str {
    &FISH[usize::from(right) * 16 + pose % 4 * 4 + dx % 2 * 2 + dy % 2]
}

pub(super) fn jelly(pose: usize, dx: usize, dy: usize) -> &'static [String; 3] {
    &JELLY[pose % 16 * 8 + dx % 2 * 4 + dy % 4]
}

pub(super) fn minnow(right: bool, pose: usize, dx: usize, dy: usize) -> &'static str {
    &MINNOW[usize::from(right) * 8 + pose % 2 * 4 + dx % 2 * 2 + dy % 2]
}

pub(super) fn whale(right: bool, pose: usize, dx: usize, dy: usize) -> &'static [String; 3] {
    &WHALE[usize::from(right) * 32 + pose % 4 * 8 + dx % 2 * 4 + dy % 4]
}

pub(super) fn mote(dx: usize, dy: usize) -> &'static str {
    &MOTE[dy % 4 * 2 + dx % 2]
}

//! Color depth, quantization and contrast.
//!
//! Extracted from the Codewhale engine's `crates/palette` (`adapt.rs` and
//! `contrast.rs` at `Hmbown/CodeWhale` `58b1dd3dd`; depth detection last
//! changed in `c6416b203`, "Honor NO_COLOR with colorless terminal output").
//! The engine's legacy-constant remapping is not carried over: components
//! here name roles, and [`crate::theme::Theme`] resolves a role for the
//! terminal's depth when it paints.

use ratatui::style::{Color, Style};

use crate::theme::{Role, Theme};

/// How many colors the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorDepth {
    /// `NO_COLOR` is set: the terminal owns foreground and background. Bold,
    /// dim, underline and every glyph still render.
    Monochrome,
    /// 16 named colors, remapped by the user's terminal profile.
    Ansi16,
    /// The fixed xterm 256-color cube and gray ramp.
    Ansi256,
    /// 24-bit color.
    TrueColor,
}

impl ColorDepth {
    /// Detect the active terminal's depth from the environment.
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with(|key| std::env::var_os(key))
    }

    /// Decide depth from an injected environment reader, so every branch is
    /// testable without touching the process environment.
    ///
    /// Order: `NO_COLOR` (present and non-empty, per no-color.org) wins, then
    /// `COLORTERM`, Windows Terminal, known truecolor `TERM_PROGRAM`s, and
    /// finally `TERM`. An unknown `TERM` gets 256 colors, not 24-bit: older
    /// remote terminals render truecolor backgrounds as bright blocks.
    #[must_use]
    pub fn detect_with(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        if let Some(no_color) = get("NO_COLOR")
            && !no_color.is_empty()
        {
            return Self::Monochrome;
        }
        if let Some(ct) = get("COLORTERM") {
            let ct = ct.to_string_lossy().to_ascii_lowercase();
            if ct.contains("truecolor") || ct.contains("24bit") {
                return Self::TrueColor;
            }
        }
        if get("WT_SESSION").is_some() {
            return Self::TrueColor;
        }
        if let Some(term_program) = get("TERM_PROGRAM") {
            let term_program = term_program.to_string_lossy().to_ascii_lowercase();
            if ["iterm", "wezterm", "vscode", "warp"]
                .iter()
                .any(|name| term_program.contains(name))
            {
                return Self::TrueColor;
            }
        }
        let term = get("TERM")
            .map(|t| t.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if term.contains("truecolor") || term.contains("24bit") {
            Self::TrueColor
        } else if term.contains("256") {
            Self::Ansi256
        } else if term.is_empty() || term == "dumb" {
            Self::Ansi16
        } else {
            Self::Ansi256
        }
    }
}

/// A 24-bit color from a `0xRRGGBB` token value.
#[must_use]
pub const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Mix two RGB colors at `alpha` (0.0 = `bg`, 1.0 = `fg`). A non-RGB input
/// returns `fg`: a named color has no meaningful blend.
#[must_use]
pub fn blend(fg: Color, bg: Color, alpha: f32) -> Color {
    let alpha = alpha.clamp(0.0, 1.0);
    match (fg, bg) {
        (Color::Rgb(fr, fg_, fb), Color::Rgb(br, bg_, bb)) => {
            let mix = |a: u8, b: u8| -> u8 {
                let a = f32::from(a);
                let b = f32::from(b);
                (b + (a - b) * alpha).round().clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(fr, br), mix(fg_, bg_), mix(fb, bb))
        }
        _ => fg,
    }
}

/// Map an RGB triple to the nearest xterm 256-color index. Uses only the
/// stable 6x6x6 cube and gray ramp (16..=255), never the user-remapped 0..=15.
#[must_use]
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    fn nearest_cube_level(channel: u8) -> usize {
        CUBE_LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| channel.abs_diff(**level))
            .map_or(0, |(idx, _)| idx)
    }

    fn dist_sq(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
        let dr = i32::from(a.0) - i32::from(b.0);
        let dg = i32::from(a.1) - i32::from(b.1);
        let db = i32::from(a.2) - i32::from(b.2);
        (dr * dr + dg * dg + db * db) as u32
    }

    let ri = nearest_cube_level(r);
    let gi = nearest_cube_level(g);
    let bi = nearest_cube_level(b);
    let cube_rgb = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube_index = 16 + (36 * ri) as u8 + (6 * gi) as u8 + bi as u8;

    let avg = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
    let gray_i = if avg <= 8 {
        0
    } else if avg >= 238 {
        23
    } else {
        ((u16::from(avg) - 8 + 5) / 10).min(23) as u8
    };
    let gray = 8 + 10 * gray_i;
    let gray_index = 232 + gray_i;

    if dist_sq((r, g, b), (gray, gray, gray)) < dist_sq((r, g, b), cube_rgb) {
        gray_index
    } else {
        cube_index
    }
}

/// The RGB an xterm 256-color index renders as. Only 16..=255 are fixed by
/// the specification; 0..=15 belong to the user's profile.
#[must_use]
pub fn indexed_rgb(index: u8) -> Option<(u8, u8, u8)> {
    const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => None,
        16..=231 => {
            let i = index - 16;
            Some((
                CUBE_LEVELS[usize::from(i / 36)],
                CUBE_LEVELS[usize::from((i / 6) % 6)],
                CUBE_LEVELS[usize::from(i % 6)],
            ))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Some((v, v, v))
        }
    }
}

/// The RGB a color is *known* to render as, or `None` when the terminal owns
/// the decision (`Reset`, named colors, indices 0..=15).
#[must_use]
pub fn resolvable_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) => indexed_rgb(index),
        _ => None,
    }
}

/// WCAG 2.x relative luminance in `0.0..=1.0`, or `None` for a color whose
/// RGB the terminal decides.
#[must_use]
pub fn relative_luminance(color: Color) -> Option<f32> {
    let (r, g, b) = resolvable_rgb(color)?;
    fn channel(value: u8) -> f32 {
        let c = f32::from(value) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
}

/// WCAG contrast ratio in `1.0..=21.0`, or `None` if either side is
/// terminal-defined.
#[must_use]
pub fn contrast_ratio(fg: Color, bg: Color) -> Option<f32> {
    let a = relative_luminance(fg)?;
    let b = relative_luminance(bg)?;
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    Some((hi + 0.05) / (lo + 0.05))
}

pub(crate) fn prepared_ink(
    pigment: [u8; 3],
    alpha: f32,
    background: Color,
    theme: &Theme,
) -> Style {
    if !theme.paints_grounds() {
        return theme.fg(Role::Primary);
    }
    let ground = resolvable_rgb(background)
        .map(|(r, g, b)| Color::Rgb(r, g, b))
        .unwrap_or_else(|| theme.token(Role::Background));
    let base = blend(
        Color::Rgb(pigment[0], pigment[1], pigment[2]),
        ground,
        alpha,
    );
    let quantize = |color| match (theme.depth(), color) {
        (ColorDepth::Ansi256, Color::Rgb(r, g, b)) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        (_, color) => color,
    };
    let mut result = quantize(base);
    for step in 1..=8 {
        if contrast_ratio(result, ground).is_none_or(|ratio| ratio >= 3.0) {
            break;
        }
        result = quantize(blend(theme.token(Role::Primary), base, step as f32 / 8.0));
    }
    Style::default().fg(result)
}

/// `hex` moved `amount` of the way toward `toward`, then rescaled so its
/// WCAG relative luminance stays `hex`'s. Contrast depends only on
/// luminance, so every contrast pair audited for `hex` holds for the result
/// (within rounding, which the audits check). Colors are `0xRRGGBB`.
#[must_use]
pub fn tint_keeping_luminance(hex: u32, toward: u32, amount: f64) -> u32 {
    fn split(v: u32) -> [f64; 3] {
        [(v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff].map(|c| f64::from(c) / 255.0)
    }
    fn lin(c: f64) -> f64 {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn unlin(v: f64) -> u32 {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round().clamp(0.0, 255.0) as u32
    }
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let amount = amount.clamp(0.0, 1.0);
    let from = split(hex);
    let to = split(toward);
    let mixed: [f64; 3] = std::array::from_fn(|i| {
        let a = (from[i] * 255.0).round();
        let b = (to[i] * 255.0).round();
        lin((a + (b - a) * amount).round() / 255.0)
    });
    let want = lum(from.map(lin));
    let have = lum(mixed);
    let k = if have > 0.0 { want / have } else { 1.0 };
    let [r, g, b] = mixed.map(|c| unlin(c * k));
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(*v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn no_color_wins_over_every_other_signal() {
        let get = env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_with(get), ColorDepth::Monochrome);
        // An empty NO_COLOR is not a request (no-color.org).
        let get = env(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")]);
        assert_eq!(ColorDepth::detect_with(get), ColorDepth::TrueColor);
    }

    #[test]
    fn term_fallbacks_are_conservative() {
        assert_eq!(
            ColorDepth::detect_with(env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(ColorDepth::detect_with(env(&[])), ColorDepth::Ansi16);
        assert_eq!(
            ColorDepth::detect_with(env(&[("TERM", "dumb")])),
            ColorDepth::Ansi16
        );
        assert_eq!(
            ColorDepth::detect_with(env(&[("TERM", "screen")])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            ColorDepth::detect_with(env(&[("TERM_PROGRAM", "WezTerm")])),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn ansi256_round_trips_through_the_fixed_cube() {
        for index in 16..=255u8 {
            let (r, g, b) = indexed_rgb(index).unwrap();
            let back = rgb_to_ansi256(r, g, b);
            assert_eq!(indexed_rgb(back), Some((r, g, b)), "index {index}");
        }
        assert_eq!(indexed_rgb(7), None);
    }

    #[test]
    fn contrast_matches_wcag_extremes() {
        let ratio = contrast_ratio(rgb(0xffffff), rgb(0x000000)).unwrap();
        assert!((ratio - 21.0).abs() < 0.01);
        assert_eq!(contrast_ratio(Color::Reset, rgb(0)), None);
    }
}

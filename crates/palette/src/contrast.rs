//! WCAG relative-luminance and contrast enforcement.
//!
//! The palette's dark→light adaptation ([`super::adapt`]) is an equality
//! whitelist: a token that isn't literally listed passes through unchanged.
//! That is fine when the surface is the one the tokens were tuned for, and
//! illegible when it isn't — a near-white body token landing on a near-white
//! terminal background (#4833).
//!
//! This module is the enumeration-independent backstop. It works on the pair
//! that actually matters — *resolved foreground* and *effective surface* — and
//! lifts any foreground that falls under the floor, whether or not anyone
//! remembered to add it to a whitelist.
//!
//! Everything here is a pure function over colors, so contrast can be asserted
//! in unit tests without a terminal.

use ratatui::style::Color;

use super::adapt::blend;
use super::themes::UiTheme;

/// WCAG 2.x AA contrast floor for body text. Applied to every resolved
/// foreground we can reason about.
pub const AA_BODY_CONTRAST: f32 = 4.5;

/// Contrast floor for secondary chrome: hint/dim text and status roles.
/// Matches the WCAG 2.x AA threshold for large text and UI components (3:1).
/// Status roles qualify because they are redundant by design — every status
/// also carries a glyph and a word label, so color is never the only channel.
pub const SECONDARY_CHROME_CONTRAST: f32 = 3.0;

/// Relative luminance per WCAG 2.x, in `0.0..=1.0`.
///
/// Returns `None` for colors whose true RGB we cannot know:
/// - [`Color::Reset`] — the terminal decides.
/// - Named ANSI colors and `Indexed(0..=15)` — remapped by the user's terminal
///   profile, so any RGB we assumed would be a guess.
///
/// `Indexed(16..=255)` is resolvable: the 6x6x6 cube and the grayscale ramp are
/// fixed by the xterm specification, not user-configurable.
#[must_use]
pub fn relative_luminance(color: Color) -> Option<f32> {
    let (r, g, b) = resolvable_rgb(color)?;
    Some(luminance_rgb(r, g, b))
}

/// The RGB triple a color is *known* to render as, or `None` when the terminal
/// owns that decision. See [`relative_luminance`].
#[must_use]
pub fn resolvable_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) if index >= 16 => Some(indexed_rgb(index)),
        _ => None,
    }
}

fn luminance_rgb(r: u8, g: u8, b: u8) -> f32 {
    fn channel(value: u8) -> f32 {
        let c = f32::from(value) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Contrast ratio between two relative luminances, in `1.0..=21.0`.
#[must_use]
pub fn contrast_from_luminance(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG contrast ratio between two colors, or `None` if either side is
/// terminal-defined (see [`relative_luminance`]).
#[must_use]
pub fn contrast_ratio(fg: Color, bg: Color) -> Option<f32> {
    Some(contrast_from_luminance(
        relative_luminance(fg)?,
        relative_luminance(bg)?,
    ))
}

/// `true` when the pair is known to clear `min_ratio`. An unknowable pair is
/// *not* reported as passing — callers use this to decide whether to intervene,
/// and we only intervene on evidence.
#[must_use]
pub fn meets_contrast(fg: Color, bg: Color, min_ratio: f32) -> bool {
    contrast_ratio(fg, bg).is_some_and(|ratio| ratio >= min_ratio)
}

/// Lift `fg` until it clears `min_ratio` against `surface`, preserving hue as
/// far as the floor allows.
///
/// Returns `fg` unchanged when:
/// - the pair already clears the floor,
/// - either side is terminal-defined (we refuse to rewrite colors whose
///   rendering the user's terminal profile owns — that is what the `Terminal`
///   theme is *for*),
/// - or `fg` is not [`Color::Rgb`], since blending an indexed color would
///   silently opt it out of the depth-adaptation stage.
///
/// Otherwise the color is blended toward whichever pole (black or white) has
/// more contrast headroom against the surface, by the smallest amount that
/// satisfies the floor. Blending is monotonic in luminance, so a bisection
/// finds that minimum. If even the pole cannot reach `min_ratio` — a
/// mid-luminance surface — the pole is returned as the best available.
#[must_use]
pub fn enforce_contrast(fg: Color, surface: Color, min_ratio: f32) -> Color {
    if !matches!(fg, Color::Rgb(..)) {
        return fg;
    }
    let (Some(fg_luma), Some(bg_luma)) = (relative_luminance(fg), relative_luminance(surface))
    else {
        return fg;
    };
    if contrast_from_luminance(fg_luma, bg_luma) >= min_ratio {
        return fg;
    }

    const BLACK: Color = Color::Rgb(0, 0, 0);
    const WHITE: Color = Color::Rgb(255, 255, 255);
    let black_ratio = contrast_from_luminance(0.0, bg_luma);
    let white_ratio = contrast_from_luminance(1.0, bg_luma);
    let (pole, pole_ratio) = if white_ratio >= black_ratio {
        (WHITE, white_ratio)
    } else {
        (BLACK, black_ratio)
    };
    if pole_ratio < min_ratio {
        return pole;
    }

    // Bisect the blend factor: 0.0 keeps `fg`, 1.0 is the pole. Contrast is
    // monotonically non-decreasing along this path, so the invariant "lo fails,
    // hi passes" holds and converges on the least-shifted compliant color.
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    let mut best = pole;
    for _ in 0..20 {
        let mid = f32::midpoint(lo, hi);
        let candidate = blend(pole, fg, mid);
        if meets_contrast(candidate, surface, min_ratio) {
            best = candidate;
            hi = mid;
        } else {
            lo = mid;
        }
    }
    best
}

/// Pick the surface a foreground is actually drawn on.
///
/// Cells frequently carry [`Color::Reset`] for the background — meaning "let
/// the terminal show through". In that case the real surface is the terminal's
/// own background, which is exactly what [`super::detect::TerminalBackground`]
/// carries.
///
/// There is deliberately no theme-surface fallback. The theme surface is what
/// we *intended* to paint; on a `Reset` cell it is precisely what the user is
/// not seeing, and #4833 is what happens when you reason against it. With no
/// painted background and no measurement we return `None` and leave the color
/// alone — declining to act beats acting on a guess.
#[must_use]
pub fn effective_surface(cell_bg: Color, detected_background: Option<Color>) -> Option<Color> {
    if resolvable_rgb(cell_bg).is_some() {
        return Some(cell_bg);
    }
    detected_background.filter(|color| resolvable_rgb(*color).is_some())
}

/// Whether a cell's symbol carries text, and therefore needs the body-text
/// contrast floor.
///
/// Box-drawing, block, and geometric-shape glyphs are frame chrome. This
/// palette uses deliberately quiet borders (`BORDER_COLOR` sits at 1.9:1 on the
/// dark stage — a design choice, not a defect), and clamping them to a text
/// floor would rewrite the visual weight of every frame. Blank cells have no
/// foreground to speak of. #4833 is a body-text bug; the floor stays on text.
#[must_use]
pub fn symbol_needs_text_contrast(symbol: &str) -> bool {
    symbol.chars().any(|ch| {
        !ch.is_whitespace()
            && !matches!(
                ch,
                // Box Drawing, Block Elements, Geometric Shapes,
                // Miscellaneous Symbols/Arrows drawing glyphs, Braille.
                '\u{2500}'..='\u{259F}'
                | '\u{25A0}'..='\u{25FF}'
                | '\u{2800}'..='\u{28FF}'
            )
    })
}

/// RGB for an xterm palette index `>= 16` (6x6x6 cube then grayscale ramp).
fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if index < 232 {
        let i = u16::from(index) - 16;
        let r = CUBE_LEVELS[(i / 36) as usize];
        let g = CUBE_LEVELS[((i / 6) % 6) as usize];
        let b = CUBE_LEVELS[(i % 6) as usize];
        (r, g, b)
    } else {
        let level = 8 + 10 * (index - 232);
        (level, level, level)
    }
}

/// A single theme color pair that fails its contrast floor. See
/// [`theme_contrast_violations`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeContrastViolation {
    /// Static name of the failing pair, e.g. `"text_muted on panel_bg"`.
    pub pair: &'static str,
    /// The foreground as shipped by the theme.
    pub fg: Color,
    /// The surface it sits on.
    pub bg: Color,
    /// The measured WCAG contrast ratio (always below `floor`).
    pub ratio: f32,
    /// The floor the pair was audited against.
    pub floor: f32,
}

/// `true` when the theme's surfaces belong to the terminal, not to us.
///
/// The `Terminal` theme and the built-in Whale pair's ordinary Flat shell use
/// [`Color::Reset`] so the host terminal's own scheme shows through. Those
/// colors are terminal-owned and not enforceable: we cannot know their RGB,
/// so the audit neither passes nor fails them — it stands down. Deepsea paints
/// its authored RGB column later. This function is how callers tell the Flat
/// exemption apart from a clean bill of health.
#[must_use]
pub fn theme_uses_terminal_owned_surfaces(theme: &UiTheme) -> bool {
    theme.surface_bg == Color::Reset
}

/// Audit a theme's text and status color pairs against their contrast floors.
///
/// Pair table and floors:
/// - `text_body`, `text_soft`, `text_muted` on each of `surface_bg`,
///   `panel_bg`, `composer_bg`, `elevated_bg` — [`AA_BODY_CONTRAST`] (4.5:1).
/// - `text_hint` on the same four surfaces, and `text_dim` on `surface_bg` —
///   [`SECONDARY_CHROME_CONTRAST`] (3:1). Hint/dim are secondary chrome:
///   de-emphasized metadata, never the sole carrier of meaning.
/// - `status_ready` / `status_working` / `status_warning` and
///   `warning` / `success` / `info` on `surface_bg` — 3:1, because these
///   roles are redundant: every status is also spelled out by a glyph and a
///   word label, so color is never the only signal.
/// - `text_body` on `selection_bg` — 4.5:1 (the theme picker renders body
///   text on the selection surface).
/// - `diff_added_fg` on `diff_added_bg`, `diff_deleted_fg` on
///   `diff_deleted_bg` — 3:1.
/// - `error_text` on `error_surface` — 4.5:1.
///
/// Pairs where either color is terminal-defined ([`Color::Reset`], named
/// ANSI, `Indexed(0..=15)`) are *skipped*, not passed: their real RGB is
/// owned by the user's terminal profile and cannot be audited. The
/// terminal-native themes are therefore partly exempt by design — see
/// [`theme_uses_terminal_owned_surfaces`], which makes that exemption explicit
/// rather than silent.
#[must_use]
pub fn theme_contrast_violations(theme: &UiTheme) -> Vec<ThemeContrastViolation> {
    let mut violations = Vec::new();
    let mut check = |pair: &'static str, fg: Color, bg: Color, floor: f32| {
        // An unresolvable side means the terminal owns the color: skip the
        // pair rather than recording a pass we cannot substantiate.
        if let Some(ratio) = contrast_ratio(fg, bg)
            && ratio < floor
        {
            violations.push(ThemeContrastViolation {
                pair,
                fg,
                bg,
                ratio,
                floor,
            });
        }
    };
    macro_rules! audit {
        ($floor:expr; $(($fg:ident, $bg:ident)),+ $(,)?) => {
            $(check(
                concat!(stringify!($fg), " on ", stringify!($bg)),
                theme.$fg,
                theme.$bg,
                $floor,
            );)+
        };
    }
    audit!(AA_BODY_CONTRAST;
        (text_body, surface_bg),
        (text_body, panel_bg),
        (text_body, composer_bg),
        (text_body, elevated_bg),
        (text_soft, surface_bg),
        (text_soft, panel_bg),
        (text_soft, composer_bg),
        (text_soft, elevated_bg),
        (text_muted, surface_bg),
        (text_muted, panel_bg),
        (text_muted, composer_bg),
        (text_muted, elevated_bg),
        (text_body, selection_bg),
        (error_text, error_surface),
    );
    audit!(SECONDARY_CHROME_CONTRAST;
        (text_hint, surface_bg),
        (text_hint, panel_bg),
        (text_hint, composer_bg),
        (text_hint, elevated_bg),
        (text_dim, surface_bg),
        (status_ready, surface_bg),
        (status_working, surface_bg),
        (status_warning, surface_bg),
        (warning, surface_bg),
        (success, surface_bg),
        (info, surface_bg),
        (diff_added_fg, diff_added_bg),
        (diff_deleted_fg, diff_deleted_bg),
    );
    violations
}

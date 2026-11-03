//! Focus-context texture prototype (#4823).
//!
//! When a modal view is open, the area *outside* the focused modal can get a
//! subtle treatment so the focused region stands out:
//!
//! - `scrim` dims the already-rendered background toward the theme surface;
//! - `grain` sprinkles sparse deterministic dots over blank cells.
//!
//! Scope and guarantees, by construction:
//!
//! - **Prototype, bounded to modal contexts.** The only consumer is
//!   `ViewStack::render`, which passes the top view's `occupied_region` as the
//!   focus rect. Nothing else in the shell opts in.
//! - **Default off.** `FocusTextureMode::Off` (the default) returns zeroed
//!   stats and leaves the buffer untouched, so the render path stays
//!   byte-identical to the pre-prototype path.
//! - **Static, not animated.** The grain pattern is a pure function of cell
//!   coordinates with no time component, so it is motion-off-safe: the
//!   `low_motion` / `MotionPolicy::allows_decorative` path needs no special
//!   handling here, and two applications over the same buffer produce
//!   identical output.
//! - **Explicit fallbacks.** Off is the fallback for unknown setting values;
//!   cells whose background is `Color::Reset` (transparent terminals) are
//!   skipped under Scrim — the terminal owns that background; the grain dot
//!   falls back to `.` when ASCII-safe mode is on.
//! - **The focus rect is never painted** — the caller applies the texture
//!   before painting the backdrop and views, so the focused modal is drawn
//!   afterward at full strength and the texture can never overwrite it.
//! - **Text is never obscured.** Grain only writes blank/whitespace cells and
//!   never touches a cell that carries a symbol. Scrim preserves the
//!   WCAG AA body-text floor (4.5:1) whenever both colors are resolvable:
//!   after blending, the foreground is lifted with
//!   `palette::enforce_contrast` against the *new* background. Colors the
//!   terminal owns (`Reset`, named ANSI) are left alone rather than guessed.
//!
//! Near-fullscreen focus regions (covering at least
//! `FOCUS_COVERAGE_NOOP_PERCENT`% of the frame) and frames smaller than
//! [`FOCUS_TEXTURE_MIN_WIDTH`]x[`FOCUS_TEXTURE_MIN_HEIGHT`] refuse the
//! treatment entirely: there is no meaningful outside left to texture.

use ratatui::{buffer::Buffer, layout::Rect, style::Color};

use codewhale_palette::{self as palette, AA_BODY_CONTRAST, UiTheme};

/// Minimum frame size that earns the texture. Below this, content and
/// controls own every cell. Mirrors the ambient-life floors.
pub const FOCUS_TEXTURE_MIN_WIDTH: u16 = crate::tui::ocean::AMBIENT_MIN_WIDTH;
pub const FOCUS_TEXTURE_MIN_HEIGHT: u16 = crate::tui::ocean::AMBIENT_MIN_HEIGHT;

/// Focus regions covering at least this share of the frame's cells leave no
/// meaningful outside to texture, so the pass is a no-op.
const FOCUS_COVERAGE_NOOP_PERCENT: u64 = 90;

/// Scrim background blend toward the theme surface.
const SCRIM_BG_BLEND: f32 = 0.5;
/// Scrim foreground blend toward the theme surface, before the contrast
/// floor lifts the result back to legibility.
const SCRIM_FG_BLEND: f32 = 0.25;

/// Grain dot glyph. Deterministic placement (see [`grain_dot_at`]) keeps the
/// texture static; `glyphs::ascii_fallback` maps this to `.` in ASCII-safe
/// mode.
const GRAIN_DOT: &str = "·";

/// Focus-context texture mode for modal views (#4823 prototype).
///
/// Parsed from the `focus_texture` setting at the consumption point; unknown
/// values fall back to `Off` and never panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusTextureMode {
    /// No treatment. The render path is byte-identical to the pre-prototype
    /// path in this mode.
    #[default]
    Off,
    /// Dim cells outside the focused modal toward the theme surface.
    Scrim,
    /// Sparse deterministic dots on blank cells outside the focused modal.
    Grain,
}

impl FocusTextureMode {
    /// Parse a setting value; `None` for anything unknown (callers map that
    /// to `Off`). Case-insensitive, surrounding whitespace ignored.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "scrim" => Some(Self::Scrim),
            "grain" => Some(Self::Grain),
            _ => None,
        }
    }
}

/// Accounting for one [`apply_focus_texture`] pass.
///
/// Identity: `cells_examined == cells_scrimmed + cells_dotted
/// + cells_skipped_focus + cells_skipped_transparent + cells_skipped_text`.
///
/// Scrim examines every cell of the area. Grain examines focus cells, text
/// cells, and deterministic dot candidates; blank cells that earn no dot are
/// left untouched and unexamined, which keeps the identity exact without a
/// "blank but not dotted" bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusTextureStats {
    pub cells_examined: u32,
    pub cells_scrimmed: u32,
    pub cells_dotted: u32,
    pub cells_skipped_focus: u32,
    pub cells_skipped_transparent: u32,
    pub cells_skipped_text: u32,
}

impl FocusTextureStats {
    /// The accounting identity asserted by the unit tests. This type's only
    /// consumer is the test gate below, hence the `dead_code` allowance.
    #[cfg(test)]
    #[must_use]
    pub fn accounted(&self) -> bool {
        self.cells_examined
            == self.cells_scrimmed
                + self.cells_dotted
                + self.cells_skipped_focus
                + self.cells_skipped_transparent
                + self.cells_skipped_text
    }
}

/// `true` when `(x, y)` lies inside `rect`.
fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.left() && x < rect.right() && y >= rect.top() && y < rect.bottom()
}

/// Deterministic grain placement: a pure function of the cell coordinates,
/// so the texture is static (motion-off-safe) and reproducible.
fn grain_dot_at(x: u16, y: u16) -> bool {
    x.wrapping_mul(7)
        .wrapping_add(y.wrapping_mul(13))
        .is_multiple_of(11)
}

/// Apply the focus-context texture to `area`, treating `focus` as the
/// focused modal's occupied region. See the module docs for the guarantees.
///
/// Returns zeroed stats and leaves the buffer untouched when the mode is
/// `Off`, the frame is below the minimum size, or the focus rect (clamped to
/// the area) covers at least `FOCUS_COVERAGE_NOOP_PERCENT`% of the frame.
pub fn apply_focus_texture(
    area: Rect,
    buf: &mut Buffer,
    focus: Rect,
    theme: &UiTheme,
    mode: FocusTextureMode,
    ascii_safe: bool,
) -> FocusTextureStats {
    let mut stats = FocusTextureStats::default();
    if mode == FocusTextureMode::Off {
        return stats;
    }
    if area.width < FOCUS_TEXTURE_MIN_WIDTH || area.height < FOCUS_TEXTURE_MIN_HEIGHT {
        return stats;
    }
    let focus = focus.intersection(area);
    // u64: a full u16-square frame already fits u32, but the *100 coverage
    // compare would not.
    let area_cells = u64::from(area.width) * u64::from(area.height);
    let focus_cells = u64::from(focus.width) * u64::from(focus.height);
    if area_cells == 0 || focus_cells * 100 >= area_cells * FOCUS_COVERAGE_NOOP_PERCENT {
        return stats;
    }

    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if rect_contains(focus, x, y) {
                stats.cells_examined += 1;
                stats.cells_skipped_focus += 1;
                continue;
            }
            match mode {
                FocusTextureMode::Off => unreachable!("off returns early"),
                FocusTextureMode::Scrim => {
                    stats.cells_examined += 1;
                    let cell = &buf[(x, y)];
                    // Transparent-terminal fallback: the terminal owns this
                    // background, so dimming it would be a guess. Skip.
                    if cell.bg == Color::Reset {
                        stats.cells_skipped_transparent += 1;
                        continue;
                    }
                    let new_bg =
                        crate::tui::ocean::mix_colors(cell.bg, theme.surface_bg, SCRIM_BG_BLEND);
                    let blended_fg =
                        crate::tui::ocean::mix_colors(cell.fg, theme.surface_bg, SCRIM_FG_BLEND);
                    // Text is never obscured by construction: when both
                    // colors are resolvable this lifts the foreground back to
                    // the AA body floor against the *new* background; when
                    // either side is terminal-owned the color is left alone.
                    let new_fg = palette::enforce_contrast(blended_fg, new_bg, AA_BODY_CONTRAST);
                    let cell = &mut buf[(x, y)];
                    cell.bg = new_bg;
                    cell.fg = new_fg;
                    stats.cells_scrimmed += 1;
                }
                FocusTextureMode::Grain => {
                    let cell = &buf[(x, y)];
                    // Never write over a cell that carries a symbol: grain is
                    // a background texture, not an ink.
                    if !cell.symbol().trim().is_empty() {
                        stats.cells_examined += 1;
                        stats.cells_skipped_text += 1;
                        continue;
                    }
                    // Blank cells that earn no dot are left untouched and
                    // unexamined (see the stats docs).
                    if !grain_dot_at(x, y) {
                        continue;
                    }
                    stats.cells_examined += 1;
                    let dot = if ascii_safe {
                        codewhale_tui_kit::glyphs::ascii_fallback(GRAIN_DOT).unwrap_or(".")
                    } else {
                        GRAIN_DOT
                    };
                    let cell = &mut buf[(x, y)];
                    cell.set_symbol(dot);
                    // Set the dim ink only when it is resolvable; a
                    // terminal-owned `text_dim` (the Terminal theme) stays
                    // as-is rather than being guessed.
                    if palette::resolvable_rgb(theme.text_dim).is_some() {
                        cell.set_fg(theme.text_dim);
                    }
                    stats.cells_dotted += 1;
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    /// A fully resolvable (Rgb) theme for the texture's color math.
    ///
    /// Whale Flat deliberately leaves its shell surface terminal-owned, so it
    /// is not a valid fixture for tests that exercise RGB scrim blending.
    fn theme() -> UiTheme {
        let theme = codewhale_palette::ThemeId::Dracula.ui_theme();
        assert!(palette::resolvable_rgb(theme.surface_bg).is_some());
        assert!(palette::resolvable_rgb(theme.text_dim).is_some());
        theme
    }

    /// A 60x20 frame: large enough for the texture, small enough to eyeball.
    fn test_area() -> Rect {
        Rect::new(0, 0, 60, 20)
    }

    /// A focus rect well under the 90% coverage threshold (200 of 1200).
    fn test_focus() -> Rect {
        Rect::new(10, 5, 20, 10)
    }

    fn blank_buffer(area: Rect) -> Buffer {
        Buffer::empty(area)
    }

    fn assert_accounted(stats: FocusTextureStats) {
        assert!(stats.accounted(), "accounting identity broken: {stats:?}");
    }

    #[test]
    fn mode_parse_covers_every_setting_value() {
        for (value, mode) in [
            ("off", FocusTextureMode::Off),
            ("scrim", FocusTextureMode::Scrim),
            ("grain", FocusTextureMode::Grain),
        ] {
            assert_eq!(FocusTextureMode::parse(value), Some(mode));
        }
        assert_eq!(
            FocusTextureMode::parse(" SCRIM "),
            Some(FocusTextureMode::Scrim)
        );
        assert_eq!(
            FocusTextureMode::parse("Grain"),
            Some(FocusTextureMode::Grain)
        );
        assert_eq!(FocusTextureMode::parse("static"), None);
        assert_eq!(FocusTextureMode::parse(""), None);
        assert_eq!(FocusTextureMode::default(), FocusTextureMode::Off);
    }

    #[test]
    fn off_leaves_buffer_untouched() {
        let area = test_area();
        let mut buf = blank_buffer(area);
        buf[(0, 0)].set_symbol("a").set_fg(Color::White);
        buf[(1, 0)].set_bg(Color::Reset);
        let original = buf.clone();

        let stats = apply_focus_texture(
            area,
            &mut buf,
            test_focus(),
            &theme(),
            FocusTextureMode::Off,
            false,
        );

        assert_eq!(stats, FocusTextureStats::default());
        assert_eq!(buf, original);
    }

    #[test]
    fn near_fullscreen_focus_is_noop() {
        let area = test_area();
        // 59x20 = 1180 of 1200 cells (98%): over the 90% threshold.
        for focus in [area, Rect::new(0, 0, 59, 20)] {
            let mut buf = blank_buffer(area);
            let original = buf.clone();
            let stats = apply_focus_texture(
                area,
                &mut buf,
                focus,
                &theme(),
                FocusTextureMode::Scrim,
                false,
            );
            assert_eq!(stats, FocusTextureStats::default(), "focus {focus:?}");
            assert_eq!(buf, original, "focus {focus:?}");
        }
    }

    #[test]
    fn small_area_is_noop() {
        for area in [Rect::new(0, 0, 39, 20), Rect::new(0, 0, 60, 9)] {
            let mut buf = blank_buffer(area);
            let original = buf.clone();
            let stats = apply_focus_texture(
                area,
                &mut buf,
                Rect::new(0, 0, 4, 2),
                &theme(),
                FocusTextureMode::Grain,
                false,
            );
            assert_eq!(stats, FocusTextureStats::default(), "area {area:?}");
            assert_eq!(buf, original, "area {area:?}");
        }
    }

    #[test]
    fn scrim_preserves_focus_and_transparent_cells() {
        let area = test_area();
        let focus = test_focus();
        let mut buf = blank_buffer(area);
        let focus_style = Style::default().fg(Color::White).bg(Color::Blue);
        let mut reset_cells = 0_u32;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if rect_contains(focus, x, y) {
                    buf[(x, y)].set_symbol("F").set_style(focus_style);
                } else if (x + y) % 2 == 0 {
                    // Transparent-terminal cells outside the focus.
                    buf[(x, y)].set_bg(Color::Reset);
                    reset_cells += 1;
                } else {
                    buf[(x, y)]
                        .set_symbol("t")
                        .set_fg(Color::Rgb(200, 200, 200))
                        .set_bg(Color::Rgb(40, 40, 40));
                }
            }
        }
        let original = buf.clone();

        let stats = apply_focus_texture(
            area,
            &mut buf,
            focus,
            &theme(),
            FocusTextureMode::Scrim,
            false,
        );

        assert_accounted(stats);
        assert_eq!(stats.cells_examined, 60 * 20);
        assert_eq!(stats.cells_skipped_focus, 20 * 10);
        assert_eq!(stats.cells_skipped_transparent, reset_cells);
        assert_eq!(stats.cells_dotted, 0);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if rect_contains(focus, x, y) {
                    assert_eq!(
                        buf[(x, y)],
                        original[(x, y)],
                        "focus cell ({x},{y}) must stay byte-identical"
                    );
                } else if (x + y) % 2 == 0 {
                    assert_eq!(
                        buf[(x, y)],
                        original[(x, y)],
                        "Reset-bg cell ({x},{y}) must stay untouched"
                    );
                }
            }
        }
    }

    #[test]
    fn scrim_text_keeps_aa_contrast_when_resolvable() {
        let area = test_area();
        let focus = test_focus();
        let theme = theme();
        let combos = [
            (Color::Rgb(255, 255, 255), Color::Rgb(30, 30, 30)),
            (Color::Rgb(200, 200, 200), Color::Rgb(240, 240, 240)),
            (Color::Rgb(120, 120, 120), Color::Rgb(110, 110, 110)),
            (Color::Rgb(40, 80, 200), Color::Rgb(20, 20, 30)),
        ];
        for (row, (fg, bg)) in combos.iter().enumerate() {
            let mut buf = blank_buffer(area);
            let y = row as u16;
            let mut seeded = Vec::new();
            for x in area.left()..area.right() {
                if rect_contains(focus, x, y) {
                    continue;
                }
                buf[(x, y)].set_symbol("t").set_fg(*fg).set_bg(*bg);
                seeded.push(x);
            }

            let stats = apply_focus_texture(
                area,
                &mut buf,
                focus,
                &theme,
                FocusTextureMode::Scrim,
                false,
            );

            assert_accounted(stats);
            for x in seeded {
                let cell = &buf[(x, y)];
                assert_eq!(cell.symbol(), "t", "glyph must survive the scrim");
                let ratio = palette::contrast_ratio(cell.fg, cell.bg)
                    .expect("seeded colors are Rgb and stay resolvable");
                assert!(
                    ratio >= AA_BODY_CONTRAST,
                    "combo {fg:?}/{bg:?} ended at {ratio}:1, below the AA floor"
                );
            }
        }
    }

    #[test]
    fn grain_never_overwrites_text_and_dots_are_deterministic() {
        let area = test_area();
        let focus = test_focus();
        let theme = theme();
        let mut buf = blank_buffer(area);
        let mut text_cells = Vec::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if !rect_contains(focus, x, y) && (x + y) % 3 == 0 {
                    buf[(x, y)].set_symbol("a").set_fg(Color::White);
                    text_cells.push((x, y));
                }
            }
        }
        let original = buf.clone();

        let stats = apply_focus_texture(
            area,
            &mut buf,
            focus,
            &theme,
            FocusTextureMode::Grain,
            false,
        );

        assert_accounted(stats);
        assert_eq!(stats.cells_skipped_focus, 20 * 10);
        assert_eq!(stats.cells_skipped_text, text_cells.len() as u32);
        assert_eq!(stats.cells_scrimmed, 0);
        // Every seeded text cell is untouched, byte for byte.
        for (x, y) in &text_cells {
            assert_eq!(
                buf[(*x, *y)],
                original[(*x, *y)],
                "text cell ({x},{y}) must never be overwritten"
            );
        }
        // Dots land exactly at the deterministic positions on blank cells.
        let mut expected_dots = 0_u32;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if rect_contains(focus, x, y) || (x + y) % 3 == 0 {
                    continue;
                }
                if grain_dot_at(x, y) {
                    expected_dots += 1;
                    assert_eq!(buf[(x, y)].symbol(), GRAIN_DOT, "dot at ({x},{y})");
                    assert_eq!(buf[(x, y)].fg, theme.text_dim);
                } else {
                    assert_eq!(
                        buf[(x, y)],
                        original[(x, y)],
                        "non-dot blank cell ({x},{y}) must stay untouched"
                    );
                }
            }
        }
        assert_eq!(stats.cells_dotted, expected_dots);
    }

    #[test]
    fn grain_ascii_safe_uses_plain_dot() {
        let area = test_area();
        let focus = test_focus();
        let mut buf = blank_buffer(area);

        let stats = apply_focus_texture(
            area,
            &mut buf,
            focus,
            &theme(),
            FocusTextureMode::Grain,
            true,
        );

        assert_accounted(stats);
        assert!(stats.cells_dotted > 0);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if !rect_contains(focus, x, y) && grain_dot_at(x, y) {
                    assert_eq!(buf[(x, y)].symbol(), ".", "ascii dot at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn grain_is_deterministic_across_applications() {
        let area = test_area();
        let focus = test_focus();
        let theme = theme();
        let mut first = blank_buffer(area);
        let mut second = blank_buffer(area);

        apply_focus_texture(
            area,
            &mut first,
            focus,
            &theme,
            FocusTextureMode::Grain,
            false,
        );
        apply_focus_texture(
            area,
            &mut second,
            focus,
            &theme,
            FocusTextureMode::Grain,
            false,
        );

        // Static texture: no time component, so motion-off needs no special
        // path and repeated passes over the same buffer agree exactly.
        assert_eq!(first, second);
    }
}

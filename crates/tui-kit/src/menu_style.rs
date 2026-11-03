//! Shared selection-row styles and non-color status marks for menus/pickers.
//!
//! Contract:
//! - The selection vocabulary is single-sourced here. Every menu, picker, and
//!   option list renders its selected row with [`selected_row_style`] (or one
//!   of the documented variants below) instead of hand-copying the
//!   `SELECTION_TEXT`-on-`SELECTION_BG` + bold trio.
//! - This module owns *styling* only. Verbs, action-hint labels, keybindings,
//!   and localized strings stay with the views and `ActionHint`; nothing here
//!   changes what any surface says.
//! - Status tones are palette tokens (`palette::STATUS_*`), never per-view
//!   colors, and every [`StatusKind`] pairs a charter glyph (`glyphs.rs`) with
//!   an English word so state never depends on color alone. Surfaces that
//!   already source their tone from the live `UiTheme` (the footer) keep that
//!   tone and consume only the glyph/word half of the mark.

use ratatui::style::{Color, Modifier, Style};

use crate::glyphs;
use codewhale_palette::{self as palette, UiTheme};

/// Canonical selected-row treatment: selection ink on the selection
/// background, bolded so the active row reads even without color.
#[must_use]
pub fn selected_row_style() -> Style {
    Style::default()
        .fg(palette::SELECTION_TEXT)
        .bg(palette::SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

/// Hover is an elevated band with an underline, distinct from the filled,
/// bold keyboard selection even on terminals without color.
#[must_use]
pub fn hovered_row_style() -> Style {
    Style::default()
        .bg(palette::SURFACE_ELEVATED)
        .add_modifier(Modifier::UNDERLINED)
}

/// Selected row with a caller-chosen foreground (the provider picker tints
/// per-field ink while keeping the shared selection background).
#[must_use]
pub fn selected_row_style_with_fg(fg: Color) -> Style {
    Style::default()
        .fg(fg)
        .bg(palette::SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

/// Selection background alone, for filler/spacer cells so the highlight band
/// runs the full width of a selected row.
#[must_use]
pub fn selected_row_bg_style() -> Style {
    Style::default().bg(palette::SELECTION_BG)
}

/// Selected-but-disabled row (e.g. a locked model): the cursor position is
/// still visible, but muted ink on the elevated surface plus a dim modifier
/// says the row cannot be chosen.
#[must_use]
pub fn disabled_selected_row_style() -> Style {
    Style::default()
        .fg(palette::TEXT_MUTED)
        .bg(palette::SURFACE_ELEVATED)
        .add_modifier(Modifier::DIM)
}

/// Theme-preview variant: the theme picker shows each candidate theme's *own*
/// selection treatment, so ink and background come from the previewed theme
/// rather than the global tokens. `UiTheme` has no dedicated selection-ink
/// field, so the theme's body text reads on its selection background — the
/// exact pairing the picker has always rendered.
#[must_use]
pub fn theme_selected_row_style(theme: &UiTheme) -> Style {
    Style::default()
        .fg(theme.text_body)
        .bg(theme.selection_bg)
        .add_modifier(Modifier::BOLD)
}

/// Non-color status cue: a charter glyph, an English word, and a palette
/// status tone. Surfaces may render any subset, but glyph and word are always
/// both available so the state never depends on color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusMark {
    pub glyph: &'static str,
    pub word: &'static str,
    pub tone: Color,
}

/// The status states shared by the footer, work surface, and pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Ready,
    Working,
    Paused,
    Done,
    /// Chartered vocabulary entry. No surface renders a failure mark yet, so
    /// nothing constructs it outside the exhaustiveness sweep below.
    Failed,
    Attention,
}

impl StatusKind {
    /// Every kind, for exhaustive checks. Consumed by the non-color-meaning
    /// test gate rather than by a runtime renderer.
    #[cfg(test)]
    pub const ALL: [StatusKind; 6] = [
        StatusKind::Ready,
        StatusKind::Working,
        StatusKind::Paused,
        StatusKind::Done,
        StatusKind::Failed,
        StatusKind::Attention,
    ];
}

/// Single source pairing each status with its charter glyph, English word,
/// and palette tone. Words match the established footer vocabulary
/// (`working`, `paused`, `idle`); tones are `palette::STATUS_*` tokens, not
/// per-view colors. `Working` has no dedicated charter glyph yet, so it
/// borrows the charter's neutral dot until one is chartered.
#[must_use]
pub const fn status_mark(kind: StatusKind) -> StatusMark {
    match kind {
        StatusKind::Ready => StatusMark {
            glyph: glyphs::READY,
            word: "idle",
            tone: palette::STATUS_SUCCESS,
        },
        StatusKind::Working => StatusMark {
            glyph: glyphs::NEUTRAL,
            word: "working",
            tone: palette::WHALE_ACTION,
        },
        StatusKind::Paused => StatusMark {
            glyph: glyphs::PAUSED,
            word: "paused",
            tone: palette::STATUS_WARNING,
        },
        StatusKind::Done => StatusMark {
            glyph: glyphs::DONE,
            word: "done",
            tone: palette::STATUS_SUCCESS,
        },
        StatusKind::Failed => StatusMark {
            glyph: glyphs::FAILED,
            word: "failed",
            tone: palette::STATUS_ERROR,
        },
        StatusKind::Attention => StatusMark {
            glyph: glyphs::ATTENTION,
            word: "attention",
            tone: palette::STATUS_WARNING,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_row_style_is_the_canonical_trio() {
        assert_eq!(
            selected_row_style(),
            Style::default()
                .fg(palette::SELECTION_TEXT)
                .bg(palette::SELECTION_BG)
                .add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn selected_row_variants_keep_the_shared_background() {
        assert_eq!(
            selected_row_bg_style(),
            Style::default().bg(palette::SELECTION_BG)
        );
        assert_eq!(
            selected_row_style_with_fg(palette::WHALE_ACTION),
            Style::default()
                .fg(palette::WHALE_ACTION)
                .bg(palette::SELECTION_BG)
                .add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn disabled_selected_row_is_muted_ink_on_elevated_surface() {
        assert_eq!(
            disabled_selected_row_style(),
            Style::default()
                .fg(palette::TEXT_MUTED)
                .bg(palette::SURFACE_ELEVATED)
                .add_modifier(Modifier::DIM)
        );
    }

    #[test]
    fn hovered_row_is_elevated_band_without_selection_ink() {
        let hovered = hovered_row_style();
        assert_eq!(
            hovered,
            Style::default()
                .bg(palette::SURFACE_ELEVATED)
                .add_modifier(Modifier::UNDERLINED)
        );
        assert_ne!(hovered, selected_row_style());
        assert_ne!(hovered, selected_row_bg_style());
    }

    #[test]
    fn theme_variant_uses_the_previewed_themes_own_tokens() {
        let theme = palette::UI_THEME;
        assert_eq!(
            theme_selected_row_style(&theme),
            Style::default()
                .fg(theme.text_body)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn every_status_kind_pairs_a_glyph_with_a_word() {
        // Non-color redundancy: neither cue may be empty, so a status never
        // depends on color alone.
        for kind in StatusKind::ALL {
            let mark = status_mark(kind);
            assert!(!mark.glyph.trim().is_empty(), "{kind:?} needs a glyph");
            assert!(!mark.word.trim().is_empty(), "{kind:?} needs a word");
        }
    }

    #[test]
    fn status_tones_are_palette_status_tokens() {
        let tokens = [
            palette::STATUS_SUCCESS,
            palette::STATUS_WARNING,
            palette::STATUS_ERROR,
            palette::WHALE_ACTION,
        ];
        for kind in StatusKind::ALL {
            let tone = status_mark(kind).tone;
            assert!(
                tokens.contains(&tone),
                "{kind:?} tone is not a STATUS_* token"
            );
        }
        assert_eq!(status_mark(StatusKind::Ready).tone, palette::STATUS_SUCCESS);
        assert_eq!(status_mark(StatusKind::Working).tone, palette::WHALE_ACTION);
        assert_eq!(
            status_mark(StatusKind::Paused).tone,
            palette::STATUS_WARNING
        );
        assert_eq!(status_mark(StatusKind::Done).tone, palette::STATUS_SUCCESS);
        assert_eq!(status_mark(StatusKind::Failed).tone, palette::STATUS_ERROR);
        assert_eq!(
            status_mark(StatusKind::Attention).tone,
            palette::STATUS_WARNING
        );
    }
}

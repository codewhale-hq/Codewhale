//! Small marks, one cell each: sonar, tide, shell, kelp, and the v5.5.0 ADE marks.
//!
//! Drawn in the spirit of Susan Kare's icons: precise, friendly, and each
//! with a word for screen readers and an ASCII form for plain terminals. They
//! are marks, not characters: the whale is the only character, and none of
//! these stands for a product concept on its own. Each sits beside words.

use std::time::Duration;

use ratatui::{buffer::Buffer, layout::Rect, text::Span, widgets::Widget};

use crate::{MotionMode, Paint, Role, Theme};

/// A small mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    /// Searching: a ping that widens while the search runs.
    Sonar {
        elapsed: Duration,
        motion: MotionMode,
    },
    /// Real progress, drawn only when the total is a fact.
    Tide { done: u32, total: u32 },
    /// A receipt: something finished and left a record.
    Shell,
    /// A quiet divider between items on one line.
    Kelp,
    /// Fast action or high-energy trigger (Design 5.5.0 ADE icon).
    Bolt,
    /// Compute, model runtime, or local engine (Design 5.5.0 ADE icon).
    Cpu,
    /// Stacked context, workspaces, or layers (Design 5.5.0 ADE icon).
    Layers,
    /// Orbiting subagents or background loop (Design 5.5.0 ADE icon).
    Orbit,
    /// Discovery, scan, or live telemetry sweep (Design 5.5.0 ADE icon).
    Radar,
    /// AI assistance or generated suggestion (Design 5.5.0 ADE icon).
    Sparkle,
}

/// A ping widening: small ring, dotted ring, ring with its centre. None of
/// these is a state mark (`○` is "ready", `·` a separator).
const SONAR: [&str; 3] = ["∘", "◌", "◎"];
const TIDE: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
/// `.` and `#` are the ASCII marks for "working" and "stopped", so the
/// ASCII tide rises through other marks.
const TIDE_ASCII: [&str; 5] = ["_", ",", "-", "=", "%"];

impl Icon {
    /// A tide mark for `done` of `total`, or `None` when the total is
    /// unknown: no fake progress.
    #[must_use]
    pub fn tide(done: u32, total: u32) -> Option<Self> {
        (total > 0).then_some(Self::Tide {
            done: done.min(total),
            total,
        })
    }

    /// The word a screen reader hears.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Icon::Sonar { .. } => "searching".into(),
            Icon::Tide { done, total } => format!("{done} of {total}"),
            Icon::Shell => "receipt".into(),
            Icon::Kelp => "divider".into(),
            Icon::Bolt => "bolt".into(),
            Icon::Cpu => "cpu".into(),
            Icon::Layers => "layers".into(),
            Icon::Orbit => "orbit".into(),
            Icon::Radar => "radar".into(),
            Icon::Sparkle => "sparkle".into(),
        }
    }

    #[must_use]
    pub fn role(&self) -> Role {
        match self {
            Icon::Sonar { .. } | Icon::Tide { .. } | Icon::Radar => Role::Live,
            Icon::Bolt => Role::Attention,
            Icon::Orbit | Icon::Sparkle => Role::Accent,
            Icon::Shell | Icon::Cpu | Icon::Layers => Role::Muted,
            Icon::Kelp => Role::Border,
        }
    }

    /// The glyph for this terminal and moment.
    #[must_use]
    pub fn glyph(&self, theme: &Theme) -> &'static str {
        let ascii = theme.ascii();
        match *self {
            Icon::Sonar { elapsed, motion } => {
                if ascii {
                    // `o` is "ready" in ASCII; a sound wave is not.
                    ")"
                } else if motion.animates() {
                    SONAR[((elapsed.as_millis() / 250) % SONAR.len() as u128) as usize]
                } else {
                    SONAR[SONAR.len() - 1]
                }
            }
            Icon::Tide { done, total } => {
                let table: &[&str] = if ascii { &TIDE_ASCII } else { &TIDE };
                let last = table.len() - 1;
                let idx = (u64::from(done) * last as u64 + u64::from(total) / 2) / u64::from(total);
                table[idx as usize]
            }
            Icon::Shell => {
                if ascii {
                    "="
                } else {
                    "◒"
                }
            }
            Icon::Kelp => {
                if ascii {
                    "|"
                } else {
                    "┊"
                }
            }
            Icon::Bolt => {
                if ascii {
                    "/"
                } else {
                    "↯"
                }
            }
            Icon::Cpu => {
                if ascii {
                    "+"
                } else {
                    "⊞"
                }
            }
            Icon::Layers => {
                if ascii {
                    ":"
                } else {
                    "≡"
                }
            }
            Icon::Orbit => {
                if ascii {
                    "@"
                } else {
                    "⊕"
                }
            }
            Icon::Radar => {
                if ascii {
                    "^"
                } else {
                    "⌖"
                }
            }
            Icon::Sparkle => {
                if ascii {
                    "&"
                } else {
                    "✦"
                }
            }
        }
    }

    #[must_use]
    pub fn span(&self, theme: &Theme) -> Span<'static> {
        Span::styled(self.glyph(theme), theme.fg(self.role()))
    }
}

impl Paint for Icon {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.span(theme).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Profile;

    #[test]
    fn tide_needs_a_real_total() {
        assert_eq!(Icon::tide(3, 0), None);
        let theme = Profile::DarkTrue.theme();
        assert_eq!(Icon::tide(0, 5).unwrap().glyph(&theme), "▁");
        assert_eq!(Icon::tide(5, 5).unwrap().glyph(&theme), "█");
        assert_eq!(Icon::tide(9, 5).unwrap().label(), "5 of 5");
    }

    #[test]
    fn every_icon_has_a_word_and_an_ascii_form() {
        let ascii = Profile::Ascii.theme();
        let mut icons: Vec<Icon> = (0..1000)
            .step_by(250)
            .map(|ms| Icon::Sonar {
                elapsed: Duration::from_millis(ms),
                motion: MotionMode::Full,
            })
            .collect();
        icons.extend((0..=4).map(|done| Icon::tide(done, 4).unwrap()));
        icons.extend([
            Icon::Shell,
            Icon::Kelp,
            Icon::Bolt,
            Icon::Cpu,
            Icon::Layers,
            Icon::Orbit,
            Icon::Radar,
            Icon::Sparkle,
        ]);
        let state_marks: Vec<&str> = crate::State::ALL
            .iter()
            .flat_map(|s| [s.glyph(), crate::glyphs::pick(s.glyph(), true)])
            .collect();
        for icon in icons {
            for profile in [Profile::DarkTrue, Profile::Ascii] {
                let glyph = icon.glyph(&profile.theme());
                assert!(
                    !state_marks.contains(&glyph),
                    "{icon:?} borrows the state mark {glyph}"
                );
            }
            assert!(!icon.label().is_empty());
            assert!(icon.glyph(&ascii).is_ascii(), "{icon:?}");
            assert_eq!(
                crate::text::width(icon.glyph(&Profile::DarkTrue.theme())),
                1
            );
        }
    }
}

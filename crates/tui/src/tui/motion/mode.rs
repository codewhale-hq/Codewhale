//! Explicit motion modes with semantic (non-animated) fallbacks.

use codewhale_tui_kit::spinner::{BRAILLE_SPINNER_STILL_FRAME, LIVE_STATIC_MARKER};

/// How the shell presents motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MotionMode {
    /// Full decorative + status motion; streaming at the display-clock cadence.
    Full,
    /// Semantically calm: static markers, no ambient life, no catch-up bursts.
    /// Streaming still follows the steady display clock (not a typewriter).
    Reduced,
    /// No decorative or status animation frames; redraw on state change only.
    Still,
}

/// Resolved presentation for a live-work marker / spinner cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerPresentation {
    /// Use the shared braille animation table.
    Animate,
    /// Hold a readable mid-fill glyph (reduced motion).
    StaticCalm,
    /// Hold the pre-delay static chevron (still / not-yet-earned).
    StaticChevron,
}

/// Policy derived from settings + runtime overlays (tmux low-motion, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionPolicy {
    pub mode: MotionMode,
    constrained_frame_rate: bool,
}

impl MotionPolicy {
    /// Derive presentation semantics independently from the runtime frame cap.
    /// Terminal compatibility limits may reduce redraw frequency, but must not
    /// masquerade as an accessibility preference or disable authored motion.
    #[must_use]
    pub fn from_settings(
        low_motion: bool,
        fancy_animations: bool,
        constrained_frame_rate: bool,
    ) -> Self {
        let mode = if low_motion {
            MotionMode::Reduced
        } else if !fancy_animations {
            MotionMode::Still
        } else {
            MotionMode::Full
        };
        Self {
            mode,
            constrained_frame_rate,
        }
    }

    #[must_use]
    pub fn mode(self) -> MotionMode {
        self.mode
    }

    #[must_use]
    pub fn allows_decorative(self) -> bool {
        matches!(self.mode, MotionMode::Full)
    }

    #[must_use]
    pub fn allows_catch_up_bursts(self) -> bool {
        matches!(self.mode, MotionMode::Full)
    }

    /// Whether the render loop should use its 30 FPS compatibility cap.
    #[must_use]
    pub fn uses_constrained_frame_rate(self) -> bool {
        self.constrained_frame_rate || !matches!(self.mode, MotionMode::Full)
    }

    #[must_use]
    pub fn spinner_presentation(self, earned_live_marker: bool) -> SpinnerPresentation {
        match self.mode {
            MotionMode::Full if earned_live_marker => SpinnerPresentation::Animate,
            MotionMode::Full => SpinnerPresentation::StaticChevron,
            MotionMode::Reduced => SpinnerPresentation::StaticCalm,
            MotionMode::Still => SpinnerPresentation::StaticChevron,
        }
    }

    /// Resolve the glyph a widget should paint for a running marker.
    #[must_use]
    pub fn spinner_glyph(
        self,
        animated_frame: &'static str,
        earned_live_marker: bool,
    ) -> &'static str {
        match self.spinner_presentation(earned_live_marker) {
            SpinnerPresentation::Animate => animated_frame,
            SpinnerPresentation::StaticCalm => BRAILLE_SPINNER_STILL_FRAME,
            SpinnerPresentation::StaticChevron => LIVE_STATIC_MARKER,
        }
    }

    /// Whether widgets should request future animation frames.
    #[must_use]
    pub fn should_request_animation_frames(self) -> bool {
        matches!(self.mode, MotionMode::Full)
    }

    /// Bridge to the legacy `low_motion` bool used across history/streaming.
    #[must_use]
    pub fn as_low_motion(self) -> bool {
        !matches!(self.mode, MotionMode::Full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_uses_calm_markers_without_decorative_motion() {
        let reduced = MotionPolicy::from_settings(true, true, false);
        let full = MotionPolicy::from_settings(false, true, false);
        assert!(full.allows_decorative());
        assert!(!reduced.allows_catch_up_bursts());
        assert!(!reduced.allows_decorative());
        assert_eq!(
            reduced.spinner_presentation(true),
            SpinnerPresentation::StaticCalm
        );
    }

    #[test]
    fn still_disables_animation_frames_and_uses_static_marker() {
        let still = MotionPolicy::from_settings(false, false, false);
        assert_eq!(still.mode, MotionMode::Still);
        assert!(!still.should_request_animation_frames());
        assert_eq!(
            still.spinner_presentation(true),
            SpinnerPresentation::StaticChevron
        );
    }

    #[test]
    fn frame_cap_preserves_authored_motion_semantics() {
        let policy = MotionPolicy::from_settings(false, true, true);
        assert_eq!(policy.mode, MotionMode::Full);
        assert!(!policy.as_low_motion());
        assert!(policy.allows_decorative());
        assert!(policy.allows_catch_up_bursts());
        assert!(policy.should_request_animation_frames());
        assert!(policy.uses_constrained_frame_rate());
    }

    #[test]
    fn full_mode_animates_after_live_marker_delay() {
        let full = MotionPolicy::from_settings(false, true, false);
        assert_eq!(
            full.spinner_presentation(true),
            SpinnerPresentation::Animate
        );
        assert_eq!(full.spinner_glyph("⣿", true), "⣿");
        assert_eq!(full.spinner_glyph("⣿", false), LIVE_STATIC_MARKER);
    }
}

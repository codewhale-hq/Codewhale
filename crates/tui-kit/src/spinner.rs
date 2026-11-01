//! Working and verification indicators from the shared terminal component kit.
//!
//! This replaces the Engine's duplicate frame tables, earned-marker delay and
//! frame indexing. The Engine still supplies clocks and motion policy, and its
//! existing backend performs ASCII/color adaptation. Quiet indicators use the
//! kit's semantic current-work mark rather than a frozen animation frame.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codewhale_ratatui::{MotionMode, VerificationSpinner, spin};

#[cfg(any(test, feature = "test-support"))]
pub const BRAILLE_SPINNER_FRAMES: [&str; 8] = spin::FRAMES;
#[cfg(test)]
const VERIFY_TICK_FRAMES: [&str; 8] = VerificationSpinner::FRAMES;
pub const LIVE_MARKER_DELAY_MS: u64 = spin::EARN_DELAY.as_millis() as u64;
pub const LIVE_STATIC_MARKER: &str = spin::PENDING_FRAME;
pub const BRAILLE_SPINNER_STILL_FRAME: &str = spin::STILL_FRAME;
pub const BRAILLE_SPINNER_FRAME_MS: u64 = spin::FRAME_INTERVAL.as_millis() as u64;

#[must_use]
pub fn braille_spinner_frame_for_elapsed_ms(elapsed_ms: u128, low_motion: bool) -> &'static str {
    spin::frame(elapsed(elapsed_ms), motion(low_motion), false)
}

#[must_use]
pub fn braille_spinner_frame(started_at: Option<Instant>, low_motion: bool) -> &'static str {
    braille_spinner_frame_for_elapsed_ms(marker_elapsed_ms(started_at), low_motion)
}

#[must_use]
pub fn verification_tick_frame(started_at: Option<Instant>, low_motion: bool) -> &'static str {
    VerificationSpinner::frame(
        elapsed(marker_elapsed_ms(started_at)),
        motion(low_motion),
        false,
    )
}

fn motion(low_motion: bool) -> MotionMode {
    if low_motion {
        MotionMode::Reduced
    } else {
        MotionMode::Full
    }
}

fn elapsed(milliseconds: u128) -> Duration {
    Duration::from_millis(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}

fn marker_elapsed_ms(started_at: Option<Instant>) -> u128 {
    started_at.map_or_else(
        || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis())
        },
        |started| started.elapsed().as_millis(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_spinner_advances_at_shared_cadence() {
        // Assert cadence behavior against the frame table rather than specific
        // glyphs so the whale-spout pattern can be retuned without churn here.
        assert_eq!(
            braille_spinner_frame_for_elapsed_ms(0, false),
            LIVE_STATIC_MARKER
        );
        assert_eq!(
            braille_spinner_frame_for_elapsed_ms(u128::from(LIVE_MARKER_DELAY_MS) - 1, false),
            LIVE_STATIC_MARKER
        );
        assert_eq!(
            braille_spinner_frame_for_elapsed_ms(u128::from(LIVE_MARKER_DELAY_MS), false),
            BRAILLE_SPINNER_FRAMES[0]
        );
        assert_eq!(
            braille_spinner_frame_for_elapsed_ms(
                u128::from(LIVE_MARKER_DELAY_MS + BRAILLE_SPINNER_FRAME_MS),
                false,
            ),
            BRAILLE_SPINNER_FRAMES[1]
        );
    }

    #[test]
    fn active_marker_uses_a_stable_five_hertz_wall_clock() {
        assert_eq!(BRAILLE_SPINNER_FRAME_MS, 200);
        for (index, frame) in BRAILLE_SPINNER_FRAMES.iter().enumerate() {
            assert_eq!(
                braille_spinner_frame_for_elapsed_ms(
                    u128::from(LIVE_MARKER_DELAY_MS)
                        + u128::from(BRAILLE_SPINNER_FRAME_MS) * index as u128,
                    false,
                ),
                *frame
            );
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(*frame),
                1,
                "active marker frames must never shift adjacent text"
            );
        }
    }

    #[test]
    fn working_swell_has_no_blank_flash_or_loop_seam() {
        let dots: Vec<u32> = BRAILLE_SPINNER_FRAMES
            .iter()
            .map(|frame| (u32::from(frame.chars().next().unwrap()) - 0x2800).count_ones())
            .collect();
        for index in 0..dots.len() {
            assert!((2..=6).contains(&dots[index]));
            assert_eq!(dots[index].abs_diff(dots[(index + 1) % dots.len()]), 1);
        }
    }

    #[test]
    fn braille_spinner_respects_low_motion() {
        assert_eq!(
            braille_spinner_frame_for_elapsed_ms(u128::from(BRAILLE_SPINNER_FRAME_MS) * 3, true),
            BRAILLE_SPINNER_STILL_FRAME
        );
    }

    #[test]
    fn verification_tick_is_distinct_and_freezes_legibly() {
        let start = Instant::now() - std::time::Duration::from_millis(LIVE_MARKER_DELAY_MS);
        assert_eq!(
            verification_tick_frame(Some(start), false),
            VERIFY_TICK_FRAMES[0]
        );
        assert_eq!(
            verification_tick_frame(Some(start), true),
            BRAILLE_SPINNER_STILL_FRAME
        );
        assert_ne!(VERIFY_TICK_FRAMES[0], BRAILLE_SPINNER_FRAMES[0]);
    }
}

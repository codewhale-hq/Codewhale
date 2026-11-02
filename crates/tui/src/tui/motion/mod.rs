//! Central motion contract for the underwater TUI.
//!
//! All decorative motion, status spinners, and streaming display cadence
//! should ask this module what is allowed. Ad-hoc shimmer/spinner timing
//! outside [`MotionMode`] / [`FrameRequester`] is a regression.
//!
//! # Semantics (not merely fewer frames)
//!
//! | Mode | Decorative ambient | Status spinner | Streaming text |
//! |------|--------------------|----------------|----------------|
//! | [`MotionMode::Full`] | yes | animated | steady 30–60 FPS display clock |
//! | [`MotionMode::Reduced`] | no | static calm glyph | same display clock; no catch-up bursts; **not** a slow typewriter |
//! | [`MotionMode::Still`] | no | static | state-change redraws only; stream still coalesces on the display clock |
//!
//! Provider SSE deltas are **input**, never animation timing. The display
//! clock ([`codewhale_tui_kit::streaming::StreamDisplayClock`]) coalesces them.

pub mod frame_requester;
pub mod mode;

pub use frame_requester::FrameRequester;
pub use mode::{MotionMode, MotionPolicy};

//! The components. Each one paints with a [`crate::Theme`] through
//! [`crate::Paint`], names roles rather than colors, and pairs every state
//! with a mark and a word.
//!
//! Every module is re-exported whole (`pub use module::*`), so a component's
//! author makes items `pub` in the component's own file and never edits this
//! file or `lib.rs`. `spinner` is the exception: its constants and functions
//! live under [`spin`] and are exported by name below.

mod hints;
mod icons;
mod picker;
mod spinner;
mod status;
mod surface;
mod toast;

pub use hints::*;
pub use icons::*;
pub use picker::*;
pub use spinner::{MotionMode, Spinner, duration};
pub use status::*;
pub use surface::*;
pub use toast::*;

/// Spinner frames and cadence.
pub mod spin {
    pub use super::spinner::{
        EARN_DELAY, FRAME_INTERVAL, FRAMES, PENDING_FRAME, STILL_FRAME, frame, next_frame_in,
    };
}

// Component families share the same Paint and Theme contracts.

// Package Input.
mod form;
mod text_input;
pub use form::*;
pub use text_input::*;

// Package Lists.
mod empty;
mod fuzzy;
mod list;
pub use empty::*;
pub use fuzzy::*;
pub use list::*;

// Package Chrome.
mod heading;
mod keymap;
mod segmented;
mod tabs;
mod toggle;
pub use heading::*;
pub use keymap::*;
pub use segmented::*;
pub use tabs::*;
pub use toggle::*;

// Package Display.
mod diff;
mod progress;
mod receipt;
mod tree;
pub use diff::*;
pub use progress::*;
pub use receipt::*;
pub use tree::*;

// Package Approval.
mod approval;
pub use approval::*;

// Package Motion.
mod motion;
pub use motion::*;
mod verification;
pub use verification::*;

// Distinctive session surfaces and settings share the same theme authority.
mod settings;
mod workspace;
pub use settings::*;
pub use workspace::*;

mod artifacts;
mod attention;
mod workbench;
pub use artifacts::*;
pub use attention::*;
pub use workbench::*;
mod habitat;
pub use habitat::*;

mod pending_input;
mod transcript;
pub use pending_input::*;
pub use transcript::*;

mod workbar;
pub use workbar::*;
mod shell;
pub use shell::*;
mod native_chrome;
pub use native_chrome::*;
mod posture;
pub use posture::*;

mod instrument;
pub use instrument::*;

mod native_composer;
pub use native_composer::*;

mod dock_tabs;
pub use dock_tabs::*;
mod braille_frame;
pub(crate) use braille_frame::paint_braille_cells;
pub use braille_frame::*;

mod whale_pet;
pub use whale_pet::*;
mod subagents;
pub use subagents::*;
mod dot_whale;
pub use dot_whale::*;

mod pet_mode;
pub use pet_mode::*;

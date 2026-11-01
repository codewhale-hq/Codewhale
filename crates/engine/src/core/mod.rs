//! Core engine module for `DeepSeek` CLI.
//!
//! This module provides the event-driven architecture that separates
//! the UI from the AI interaction logic:
//!
//! - `engine`: The main engine that processes operations
//! - `events`: Events emitted by the engine to the UI
//! - `ops`: Operations submitted by the UI to the engine
//! - `session`: Session state management
//! - `turn`: Turn context and tracking

// Engine code runs inside the TUI alt-screen — see `runtime_log` for why
// raw stdio prints must not appear here. Use `tracing::*` instead.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub mod authority;
pub mod engine;
pub mod events;
// The first production consumer of the staged runtime contract is the
// provider-neutral model boundary. Keep the remaining contract files staged
// until their own consumers land instead of compiling dead scaffolding.
#[path = "runtime_contract/model.rs"]
pub mod model_client;
pub mod ops;
// Phase A1 of the core/protocol extraction: exhaustive projections of the
// engine's `Op` / `Event` onto `codewhale_protocol::{op::Op, EventMsg}`.
// A new engine variant without a protocol twin fails to compile there.
pub mod protocol_parity;
#[cfg(test)]
mod queued_approval_tests;
pub mod session;
#[path = "runtime_contract/termination.rs"]
pub mod termination;
pub mod turn;

// Re-exports

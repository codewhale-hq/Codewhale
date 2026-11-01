//! Federated plugin marketplace catalog parsing (#5311).
//!
//! Parses real published catalog schemas — Kimi, Claude, Codex, and the
//! native Codewhale format — into one normalized candidate model with
//! per-entry fault isolation, provenance that never grants trust, and
//! install plans that say honestly whether Codewhale can fetch a source.
//!
//! This layer is parser-only: no network, no filesystem reads, no process
//! execution. Every fetch happens through the existing reviewed installer
//! when an operator explicitly installs a candidate. The one filesystem
//! seam — reading a local catalog document an operator pointed at — lives in
//! [`document`] and is shared by the TUI command and the Runtime API.

pub mod document;
pub mod parsers;
pub mod store;
#[cfg(test)]
mod tests;
pub mod types;

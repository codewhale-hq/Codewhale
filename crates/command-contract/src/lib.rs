//! Portable command shapes and shared formatting for command implementations.
//!
//! Host registration and adapters remain with their clients. Decoupled command
//! groups move into `codewhale-commands` with all consumers migrated per slice.

pub mod config;
pub mod config_policy;
pub mod elapsed;
pub mod facets;
pub mod handler;
pub mod metadata;
pub mod navigation;
pub mod outcome;
pub mod types;

pub use facets::*;
pub use handler::{CommandCapabilities, CommandContexts, CommandHandler, ContextParts};
pub use metadata::{CommandDiscovery, CommandInfo, RegisterCommand};
pub use types::*;

#[cfg(test)]
mod tests;

pub mod metrics;
pub mod money;
pub mod tool_outputs;

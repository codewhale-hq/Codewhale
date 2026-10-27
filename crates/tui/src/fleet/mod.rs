//! Agent Fleet control plane — local-first manager, ledger, and workers.

pub mod alerts;
pub(crate) mod artifacts;
pub mod capability_badges;
pub mod control;
pub mod exact;
pub mod executor;
pub mod host;
pub mod identity;
pub mod ledger;
pub mod manager;
pub mod members;
pub mod placement;
pub mod profile;
pub mod roster;
pub mod scheduler;
pub mod scout;
pub mod store;
pub mod tailnet;
pub mod task_spec;
pub mod worker_runtime;

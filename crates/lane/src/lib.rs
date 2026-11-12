//! Lane registry + Runtime backends (#4176).
//!
//! A **Lane** is a running workflow instance (one issue/goal). **Runtime** owns
//! where/how it executes (tmux or inline) — never Fleet.
//!
//! Persistence: `$CODEWHALE_HOME/lanes/<lane-id>.json` plus stream-json logs
//! under `$CODEWHALE_HOME/lanes/logs/<lane-id>.ndjson`.

pub mod control;
mod registry;
mod runtime;
mod worktree;

pub use control::{
    Availability, BackendCapability, ControlAuthority, ControlContext, ControlDomain,
    ControlExecution, ControlFailure, ControlFailureKind, ControlOperation, ControlReceipt,
    ControlSurface, ControlTarget, Known, LifecycleOutcome, OperationDescriptor, PersistenceScope,
    Retryability, RunListPage, RunRouteDto, RunSummaryDto, RunUsageDto, TargetKind,
    UnavailableReason, UnknownReason,
};
pub use registry::{
    LaneRecord, LaneRegistry, LaneStatus, TerminalTransition, lane_registry_root, lanes_dir,
};
pub use runtime::{
    InlineRuntime, LaneLogProxySpec, LaneStartSpec, RuntimeBackend, RuntimeBackendKind,
    TmuxRuntime, backend_for, resolve_backend, run_lane_log_proxy,
};
pub use worktree::{
    GitCommandFn, ProvisionedWorktree, WorktreeProvision, provision_worktree, reject_linked_below,
    remove_worktree_if_expired, set_git_command,
};

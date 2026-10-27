//! Dynamic Workflow runtime for CodeWhale.
//!
//! This crate is the imperative half of Workflow: a sandboxed QuickJS
//! (rquickjs) runtime that executes a model-authored JS program which
//! dispatches fleet-routed subagents via `task()`, fans out with
//! `parallel()`/`pipeline()`, reports progress with `log()`/`phase()`, and
//! scales itself to a token pool via the `budget` global. The static,
//! declarative IR (record/replay, model policy) stays in `codewhale-workflow`;
//! this crate only speaks to the outside world through the
//! [`WorkflowDriver`] seam, so it is fully testable without spawning a real
//! subagent (see [`testing::FakeDriver`]).
//!
//! # Script surface
//!
//! Every script runs inside an async function with these globals:
//!
//! * `args` — the invocation input, verbatim.
//! * `await task(opts)` — dispatch one subagent; resolves to the full result
//!   text, or to a parsed + schema-validated object when `opts.responseSchema`
//!   is set. Throws on rejection, failure, cancellation or an owner budget
//!   exhaustion. A reply that fails `responseSchema` gets up to
//!   [`SCHEMA_REPAIR_MAX_ATTEMPTS`] bounded repairs (#5583) before it throws;
//!   the failed attempts are reported as driver receipts either way.
//! * `parallel(thunks, opts)` — fan-out, at most [`PARALLEL_MAX_ITEMS`] items.
//!   `opts.mode` is `settled` (default), `fail-fast`, or `partial`; an
//!   unrecognized mode throws. `settled` keeps today's ergonomics — an
//!   ordinary failed slot resolves to `null` — while schema-contract failures
//!   and run cancellation still fail the run. `fail-fast` rejects the fan-out
//!   with the first non-fatal slot error instead of nulling it. `partial`
//!   resolves every non-cancellation failure to
//!   `{ __taskError: { index, kind, message } }`.
//! * `pipeline(items, ...stages)` — per-item stage chains with no barrier
//!   between stages; same cap and the same three modes via the
//!   `pipeline(items, { stages, mode })` overload.
//!
//! Both fan-outs attach a non-enumerable `errors` array to the resolved
//! result — `[{ index, kind, message }]`, ordered by index, empty when
//! nothing failed. The array's own contents, length, and JSON encoding are
//! unchanged, so a dropped slot is now inspectable instead of erased.
//!
//! `kind` is a [`TaskErrorKind`] wire string (`admission`, `budget`,
//! `cancelled`, `agent`, `schema`, `driver`) assigned by the host where the
//! failure happened, or `script` for an error the script threw itself. It is
//! read from the thrown `Error`'s `.kind`, never inferred from message text.
//! * `log(msg)` / `phase(title)` — progress events forwarded to the driver.
//! * `budget.total` / `budget.spent()` / `budget.remaining()` — live driver
//!   snapshots. An unmetered driver exposes `budget.metered === false` and
//!   null spent/remaining; total is then only the declared hint. Metered
//!   drivers use null total and infinite remaining for an unbounded pool.
//!
//! `Date.now()`, `new Date()`, `Date.parse/UTC`, and `Math.random()` throw:
//! runs must be deterministic so recorded traces can be replayed.
//!
//! # Ownership boundaries
//!
//! Token accounting and admission belong to the driver; the VM only reads
//! snapshots and fast-fails a spawn when the shared pool is already exhausted.
//! Already-running parallel children can reconcile above the hint because
//! provider usage arrives at response boundaries, not token-by-token. Fleet
//! roster resolution for `profile` also happens driver-side; this crate
//! normalizes and token-validates the profile string, nothing more.

mod driver;
mod error;
mod schema;
pub mod testing;
mod vm;

pub use driver::{
    BudgetSnapshot, ProgressEvent, SpawnedTask, TaskCompletion, TaskRequest, ToolCallRequest,
    ToolCallResponse, ToolInvoker, WorkflowDriver, normalize_profile,
};
pub use error::{DriverError, TaskErrorKind, WorkflowJsError};
pub use schema::{SCHEMA_RAW_CARRY_CHARS, SCHEMA_RAW_PREVIEW_CHARS, SCHEMA_REPAIR_MAX_ATTEMPTS};
pub use vm::{VmLimits, WorkflowRunCancel, WorkflowVm, normalize_task_cwd, normalize_task_cwd_in};

/// Maximum concurrently executing agents within one Workflow run.
///
/// Fan-out may *declare* more work via `parallel()` / `pipeline()`, but the
/// host admits at most this many live `task()` children at once; additional
/// spawns wait for a slot.
pub const WORKFLOW_MAX_CONCURRENT: usize = 16;

/// Maximum items per `parallel()` or `pipeline()` call (design §4.2).
/// Bounds one materialized fan-out array; it does not limit total work.
pub const PARALLEL_MAX_ITEMS: usize = 1000;

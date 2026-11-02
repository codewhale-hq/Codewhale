//! Default-on, user-disableable aggregate product usage counting for Codewhale.
//!
//! The whole of what this crate may ever send is [`event`]. The whole of what
//! decides whether it may send anything is [`decision`]. Nothing else in the
//! tree is permitted to construct a payload or to reach the wire, and nothing in
//! here reads a prompt, a completion, a tool argument, a file, a path, a git
//! remote, a branch, a model id, a provider table name, an MCP server name, an
//! approval rule, an error body, a panic message, or a credential.
//!
//! # The shape of the guarantee
//!
//! Permission is a **value**, not a convention. [`decide`] is the only constructor
//! of [`TelemetryConsent`]; [`init`] takes one by value and there is no
//! bool-taking sibling. Six init sites cannot each drift from the predicate,
//! because they never see the predicate.
//!
//! Arming is a **`OnceLock`**, consulted by every write path including
//! [`record_blocking`]. This matters because the process panic hook is installed
//! before the command line is even parsed, long before any config resolution: it
//! cannot consult a resolved value, but it can consult a lock that is by
//! construction empty until resolution completes. A disabled user's panic
//! therefore writes nothing and creates no directory.
//!
//! Arming also **truncates** any stale buffer before a newly permitted process
//! begins recording.
//!
//! # Failure posture
//!
//! Fail-open is absolute. Every fallible step ends in `.ok()?` or `let _ =`.
//! Nothing here returns an error to a caller, blocks a turn, blocks a tool, or
//! blocks process exit. Telemetry that costs a user their session is worse than
//! no telemetry.

#![deny(missing_docs)]

mod actor;
pub mod buffer;
pub mod client;
pub mod counters;
pub mod decision;
pub mod envelope;
pub mod event;
pub mod notice;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

pub use actor::{BATCH_MAX_BYTES, BATCH_MAX_EVENTS, FlushOutcome};
pub use counters::{Counter, ErrorCounter, SessionCounters, TurnOutcome, TurnTokens};
pub use decision::{
    EndpointError, TELEMETRY_DIR, TelemetryConsent, TelemetryDecision, decide, decide_in_home,
    load_setup_state_for_decision, load_setup_state_for_decision_at, re_decide, validate_endpoint,
};
pub use envelope::reduce_panic_site;
pub use event::{
    Arch, Batch, ColdStartBucket, Counters, DurationBucket, Errors, Event, ExitClass, InstallKind,
    Libc, ModelCounters, NOTICE_VERSION, Os, ProductCounters, SCHEMA_VERSION, SessionSource,
    Surface, TurnWall,
};

/// How long the shutdown flush may hold the process.
///
/// The terminal is still in alt-screen while this runs. The persistence actor's
/// unbounded `task.await` next door is not a pattern to copy: a hung TLS
/// handshake would hold a user's terminal past exit.
pub const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// Everything a write path needs once the process is armed.
struct Armed {
    handle: actor::Handle,
    root: std::path::PathBuf,
    exit_class: AtomicU8,
}

/// The one gate. Unset means every write path is a hard no-op.
static ARMED: OnceLock<Armed> = OnceLock::new();

/// Arm telemetry for this process.
///
/// Takes [`TelemetryConsent`] **by value**: there is no way to call this without
/// having gone through [`decide`], and no overload that accepts a `bool`.
///
/// Idempotent — a second call is ignored, so a surface that dispatches twice
/// cannot start two writers against one buffer.
pub fn init(consent: TelemetryConsent) {
    if ARMED.get().is_some() {
        return;
    }
    let root = consent.root().to_path_buf();
    let observed_generation = consent.tombstone_generation().cloned();
    let config_path = consent.config_path().map(std::path::Path::to_path_buf);

    // Re-check durable permission under the same ordering lock as wipe. The
    // generation match prevents consent resolved before a newer opt-out from
    // clearing that opt-out; the fresh predicate preserves intentional
    // `config set telemetry true` re-enablement.
    if let Err(error) = buffer::arm(&root, observed_generation.as_ref(), || {
        decision::permission_still_enabled(config_path.as_deref(), &root)
    }) {
        tracing::debug!("telemetry could not prepare its buffer: {error}");
        return;
    }

    notice::show_startup_disclosure(consent.surface());

    let context = actor::Context {
        root: root.clone(),
        endpoint: consent.endpoint().map(str::to_string),
        surface: consent.surface(),
        config_path: consent.config_path().map(std::path::Path::to_path_buf),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: envelope::release_build_sha(),
        tty: envelope::current_tty(),
    };

    let _ = ARMED.set(Armed {
        handle: actor::Handle::spawn(context),
        root: root.clone(),
        exit_class: AtomicU8::new(ExitClass::Clean.as_u8()),
    });

    record_install_or_upgrade(&root);
}

/// Note that this binary's version differs from the one last seen on this
/// machine, at most once per version.
///
/// The previous version comes from `$CODEWHALE_HOME/telemetry/state.json` and
/// from nowhere else. Session history and config mtimes would answer the same
/// question and carry a different privacy contract; reading them here would put
/// this crate one refactor away from the thread store.
///
/// The state file is updated before the event is queued, so a process that dies
/// between the two reports nothing rather than reporting the same upgrade on
/// every launch.
fn record_install_or_upgrade(root: &std::path::Path) {
    let current = env!("CARGO_PKG_VERSION");
    let mut state = envelope::read_state(root);
    if state.last_version.as_deref() == Some(current) {
        return;
    }
    let kind = match state.last_version.as_deref() {
        None => InstallKind::Install,
        Some(previous) if version_is_older(previous, current) => InstallKind::Upgrade,
        Some(_) => InstallKind::Downgrade,
    };
    let previous_version = state.last_version.clone();
    state.schema_version = SCHEMA_VERSION;
    state.last_version = Some(current.to_string());
    if envelope::write_state(root, &state).is_err() {
        // Nothing was recorded, so the next launch will try again. Emitting
        // without the write would re-report the same upgrade forever.
        return;
    }
    record(Event::InstallOrUpgrade {
        kind,
        previous_version,
    });
}

/// Compare two dotted release numbers, ignoring any pre-release suffix.
///
/// Deliberately not a semver dependency: the only question asked is which of
/// install / upgrade / downgrade to name, and a version this crate cannot parse
/// answers "not older", which reports a downgrade — the conservative direction,
/// since it never invents an upgrade that did not happen.
fn version_is_older(previous: &str, current: &str) -> bool {
    fn parts(value: &str) -> Vec<u64> {
        value
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or_default())
            .collect()
    }
    let (previous, current) = (parts(previous), parts(current));
    let width = previous.len().max(current.len());
    for index in 0..width {
        let left = previous.get(index).copied().unwrap_or_default();
        let right = current.get(index).copied().unwrap_or_default();
        if left != right {
            return left < right;
        }
    }
    false
}

/// Whether this process is armed. Every write path checks this first.
#[must_use]
pub fn is_armed() -> bool {
    ARMED.get().is_some()
}

/// This process's session accumulators.
///
/// Deliberately **not** behind the arming gate. Every bump is a relaxed atomic
/// increment on a counter that never leaves this process unless [`init`] was
/// reached, so gating them would buy nothing and would put an `is_armed()`
/// branch on eleven hot call sites. The gate that matters is on the write
/// paths, and a snapshot of these numbers only ever reaches a payload through
/// one.
pub fn session_counters() -> &'static SessionCounters {
    static COUNTERS: OnceLock<SessionCounters> = OnceLock::new();
    COUNTERS.get_or_init(SessionCounters::default)
}

/// Queue an event for the writer thread.
///
/// Non-blocking, and a no-op when unarmed.
pub fn record(event: Event) {
    let Some(armed) = ARMED.get() else {
        return;
    };
    armed.handle.record(event);
}

/// Write an event synchronously, without the writer thread.
///
/// The synchronous escape hatch for the three paths where the async world is
/// gone or going: the panic hook, `record_caught_panic`, and the signal task
/// immediately before `std::process::exit`. One `O_APPEND` `write(2)` under
/// `PIPE_BUF`, a `sync_data`, and return — microseconds.
///
/// The append takes the shared privacy lock with `try_write()`, never a blocking
/// acquisition. If the actor, a wipe, or another Codewhale process sharing
/// `CODEWHALE_HOME` holds it, the event is dropped immediately. This preserves
/// the panic/SIGINT liveness contract without allowing a write to race past a
/// completed opt-out.
///
/// A no-op when unarmed, which is what makes a disabled user's panic write
/// nothing and create no directory.
pub fn record_blocking(event: Event) {
    let Some(armed) = ARMED.get() else {
        return;
    };
    let Ok(line) = serde_json::to_string(&event) else {
        return;
    };
    let path = buffer::buffer_path(&armed.root);
    let _ = buffer::append(&armed.root, &path, &line);
}

/// Record how this process is ending.
///
/// Set by the panic hook, by the signal task before `std::process::exit`, and on
/// the clean path from the run's termination reason. **Never derived from an
/// exit code**: a cancelled turn and a SIGINT both exit 130, so a code-based
/// derivation would report every Esc as a signal.
pub fn set_exit_class(class: ExitClass) {
    let Some(armed) = ARMED.get() else {
        return;
    };
    armed.exit_class.store(class.as_u8(), Ordering::Relaxed);
}

/// The exit class recorded so far. `Clean` when unarmed or unset.
#[must_use]
pub fn exit_class() -> ExitClass {
    ARMED.get().map_or(ExitClass::Clean, |armed| {
        ExitClass::from_u8(armed.exit_class.load(Ordering::Relaxed))
    })
}

/// Final flush, then stop the writer thread.
///
/// Returns [`FlushOutcome::Empty`] when unarmed.
pub fn shutdown_blocking(deadline: Duration) -> FlushOutcome {
    ARMED
        .get()
        .map_or(FlushOutcome::Empty, |armed| armed.handle.shutdown(deadline))
}

/// Persist every queued event locally, then stop the writer without networking.
///
/// With an explicitly empty endpoint this finalizes the local dry-run batch.
/// With a configured endpoint it leaves events in the pending buffer for the
/// next full flush. Returns [`FlushOutcome::Empty`] when unarmed.
///
/// This joins the writer instead of racing it (#6269). The local path seals
/// at most a consent re-check, a tombstone probe, and one fsync append —
/// bounded disk work with no network in it. A deadline here buys nothing the
/// rest of the CLI does not already forgo: startup reads config from the
/// same disk with no timeout either. The writer always acknowledges, even on
/// panic, so the only fail-open outcome is a writer that is already gone.
/// Only the network flush keeps a deadline.
#[must_use]
pub fn persist_local_blocking() -> FlushOutcome {
    ARMED
        .get()
        .map_or(FlushOutcome::Empty, |armed| armed.handle.persist_local())
}

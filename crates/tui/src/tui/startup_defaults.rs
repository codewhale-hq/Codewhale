//! The single owner for mode, reasoning and pet-view defaults written back by interactive
//! TUI selectors. Provider/model choices persist through `config_persistence`.
//!
//! Before this module there were three unrelated writers for the same
//! `settings.toml` keys: the model picker's combined model+effort apply, the
//! effort-only picker apply, and — for `default_mode` — nothing at all, so
//! cycling into Operate reverted to Act on the next launch while the preset
//! and `/config` surfaces persisted correctly. Routing every interactive
//! selector through [`StartupDefaults`] keeps one load/normalize/save
//! transaction per user action and one place where the write can be audited.
//!
//! Every write goes through one per-`App` owner, [`StartupDefaultsWriter`],
//! which is what makes the two application shapes safe to mix:
//!
//! - [`StartupDefaultsWriter::apply_blocking`] is synchronous. The model picker
//!   uses it because it must know whether the write landed before it records a
//!   provider/model setup receipt.
//! - [`StartupDefaultsWriter::spawn`] is non-blocking. Mode cycling and
//!   reasoning cycling are keystroke-rate actions, so they must never gate a
//!   redraw on disk I/O. Failures are still truthful: they land in a
//!   [`StartupDefaultFailures`] mailbox that the event loop drains into a
//!   warning toast, rather than being swallowed.
//!
//! ## Why the ordering is not left to the scheduler
//!
//! Each write is a load / modify / save transaction against one
//! `settings.toml`. Handing each keystroke its own blocking task lets two of
//! those transactions interleave — both load the same bytes, and the one that
//! saves last wins regardless of which selection the user made last. That loses
//! the newer selection for the same field, and it can also resurrect a stale
//! value for a *different* field, because each save writes the whole file.
//!
//! So ordering comes from the enqueue, never from task scheduling:
//!
//! 1. Every producer is the TUI event loop thread, which processes user actions
//!    one at a time. `spawn` pushes onto a FIFO queue *synchronously* on that
//!    thread, so queue order is exactly user-action order.
//! 2. A single `write` mutex is held across a whole drain — pop, load, modify,
//!    save, repeat — so no two transactions ever overlap, no matter how many
//!    blocking tasks are in flight. Extra tasks simply find the queue empty.
//! 3. `apply_blocking` takes the same mutex and first drains everything queued
//!    ahead of it, then applies its own update. It cannot be overtaken by a
//!    later action, because a later action can only be enqueued by the event
//!    loop thread that is currently blocked inside this call.
//!
//! Together: last user action wins for a field, and because each transaction
//! only sets the fields its own [`StartupDefaults`] carries, disjoint fields
//! never clobber each other.
//!
//! ## Why this writer is not the whole story
//!
//! The `write` mutex above only serializes transactions *this writer* owns, and
//! `settings.toml` has other writers in the same process — most sharply, the
//! Shift+Tab permission-posture write on the same event loop. Two writers that
//! each do their own load / modify / save can still lose a field to each other,
//! and locking `save` would not help because the stale read already happened.
//!
//! So the atomicity of a single load/modify/save belongs one level down, in
//! [`Settings::transact`], which holds a per-settings-path process mutex *and*
//! a cross-process advisory lock on an adjacent `settings.toml.lock` across the
//! whole cycle — the second one because a user can easily have two Codewhale
//! processes open on the same home directory. Every reachable settings writer
//! goes through it. What stays here is the part `transact` cannot provide:
//! **ordering**. A lock makes concurrent transactions safe but says nothing
//! about which one runs first, and for keystroke-rate actions "last user action
//! wins" is the behavior users can actually perceive.
//!
//! ## The no-deadlock contract
//!
//! `write` is held across disk I/O, so it is the *outermost* lock of every
//! transaction. `queue` is only ever taken for a single push or pop and never
//! across I/O, so it can never be the lock someone waits behind. That gives one
//! rule, and it is the rule this module has to keep true:
//!
//! > **No thread may block on `write` while holding a lock that a settings
//! > transaction needs.** Doing so parks the drainer (which holds `write` and
//! > wants that lock) against the waiter (which holds that lock and wants
//! > `write`).
//!
//! In production nothing violates it: a settings transaction takes only its own
//! two locks (the settings process mutex and the settings file lock), and
//! nothing that holds either ever asks for `write`. Under `cfg(test)` a third
//! lock joins the order — settings path
//! resolution goes through `test_support::with_test_env_lock`, and a
//! sealed-`HOME` test holds that lock for its entire body. So a test thread
//! calling [`StartupDefaultsWriter::flush`] or
//! [`StartupDefaultsWriter::apply_blocking`] would wait on a background drainer
//! that is itself parked on the test's own env lock. That inversion is the
//! deadlock this module was first shipped with.
//!
//! [`StartupDefaultsWriter::spawn`] closes it at the source, in two parts:
//!
//! 1. The background drain is enrolled in the *spawning test's* env scope
//!    (`test_support::join_env_scope`), so the drain never blocks on a lock its
//!    own test holds. The env barrier still applies to genuinely foreign
//!    readers; it just stops treating this test's own writer thread as one of
//!    them.
//! 2. Permission to write at all is keyed to a specific env-scope generation
//!    (see `spawn_writes_permitted`), not to a process-global flag. A test
//!    that is not inside an authorized scope enqueues nothing and spawns
//!    nothing, so it can never become a thread that holds `write` while parked
//!    on a *foreign* test's env lock. Outstanding-drain accounting is keyed the
//!    same way, so a closing gate only ever waits for the drains it authorized.
//!
//! `lock_write` and `Settings::transact` additionally carry test-only deadlines,
//! so if the rule is ever broken again the offending test fails with a
//! diagnostic instead of hanging CI.
//!
//! What this module does *not* own: the effective per-turn policy. Session
//! restore and preset application call `App::set_mode` directly, which changes
//! the live session only. Only a user-facing selection writes a startup
//! default.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use codewhale_config::AppMode;
use codewhale_runtime::settings::Settings;

/// One user selection's worth of startup-default writes.
///
/// Fields left `None` are untouched on disk, so mode and thinking updates
/// preserve each other's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupDefaults {
    /// `settings.default_mode` — the mode a fresh session starts in.
    mode: Option<&'static str>,
    /// `settings.reasoning_effort` — normalized for the active route by the
    /// caller, because only the caller knows the route.
    reasoning_effort: Option<String>,
    /// `settings.pet_mode` — the main view a fresh session starts in.
    pet_mode: Option<bool>,
}

impl StartupDefaults {
    /// Persist `mode` as the startup default.
    ///
    /// `AppMode::as_setting` already collapses the legacy `Yolo` alias to
    /// `agent`, which is the mode `App::set_mode` actually installs — so the
    /// persisted value matches the live session rather than a label the user
    /// never lands in.
    #[must_use]
    pub fn mode(mode: AppMode) -> Self {
        Self {
            mode: Some(mode.as_setting()),
            ..Self::default()
        }
    }

    /// Persist a route-normalized reasoning-effort setting.
    #[must_use]
    pub fn reasoning_effort(setting: impl Into<String>) -> Self {
        Self {
            reasoning_effort: Some(setting.into()),
            ..Self::default()
        }
    }

    /// Persist the selected main view without delaying the event loop.
    #[must_use]
    pub fn pet_mode(enabled: bool) -> Self {
        Self {
            pet_mode: Some(enabled),
            ..Self::default()
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: &str) -> Self {
        self.reasoning_effort = Some(effort.to_string());
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mode.is_none() && self.reasoning_effort.is_none() && self.pet_mode.is_none()
    }

    /// Which user-facing settings this update touches, as typed subjects.
    ///
    /// Deliberately *not* an English string. This module runs on a blocking
    /// pool with no access to the user's locale, and a failure it prebuilt in
    /// English would be untranslatable by the time `App` shows it. Callers get
    /// the enum and translate at the locale boundary (see
    /// `App::drain_startup_default_failures`).
    #[must_use]
    fn subjects(&self) -> Vec<StartupDefaultSubject> {
        let mut subjects = Vec::new();
        if self.mode.is_some() {
            subjects.push(StartupDefaultSubject::Mode);
        }
        if self.reasoning_effort.is_some() {
            subjects.push(StartupDefaultSubject::Thinking);
        }
        if self.pet_mode.is_some() {
            subjects.push(StartupDefaultSubject::PetMode);
        }
        subjects
    }

    /// Load, update, and save `settings.toml` in one transaction.
    ///
    /// Private on purpose: callers go through [`StartupDefaultsWriter`], which
    /// is what serializes these transactions against each other. Calling this
    /// directly would reintroduce the interleaving described at the top of the
    /// module.
    ///
    /// Every key goes through `Settings::set`, so the same normalization and
    /// validation the `/config` surface uses applies here too.
    fn apply(&self) -> anyhow::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        Settings::transact(|settings| {
            if let Some(mode) = self.mode {
                settings.set("default_mode", mode)?;
            }
            if let Some(effort) = self.reasoning_effort.as_deref() {
                settings.set("reasoning_effort", effort)?;
            }
            if let Some(enabled) = self.pet_mode {
                settings.set("pet_mode", if enabled { "true" } else { "false" })?;
            }
            Ok(())
        })
    }

    fn apply_reporting(&self, failures: &StartupDefaultFailures) {
        if let Err(err) = self.apply() {
            let subjects = self.subjects();
            // The log line may carry the full chain — it goes to the user's own
            // log file, not to the screen.
            tracing::warn!(
                target: "settings",
                subjects = ?subjects,
                error = ?err,
                "startup default was not persisted"
            );
            failures.record(StartupDefaultFailure {
                subjects,
                detail: safe_error_detail(&err),
            });
        }
    }
}

/// What a failed startup-default write was trying to save.
///
/// `App` maps each variant to a `MessageId`, so the same failure reads in the
/// user's language rather than in whatever language the writer thread happened
/// to be compiled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDefaultSubject {
    /// `settings.default_mode`.
    Mode,
    /// `settings.reasoning_effort`.
    Thinking,
    /// `settings.pet_mode`.
    PetMode,
}

/// One startup-default write that did not land.
///
/// Typed subjects plus a **short, path-free** error detail. Nothing here is UI
/// copy: the sentence around it is assembled by `App` from a `MessageId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupDefaultFailure {
    /// Empty only for the unreachable "empty update failed" case.
    pub subjects: Vec<StartupDefaultSubject>,
    /// Cause, safe to render: the root cause's own message with any path-like
    /// token replaced. See [`safe_error_detail`].
    pub detail: String,
}

/// Reduce an error chain to one short line that is safe to put on screen.
///
/// Two rules, both load-bearing:
///
/// 1. Only the **root cause** is used. Our own `with_context` strings are the
///    ones that interpolate `settings.toml`'s absolute path; the underlying
///    `io::Error` ("Permission denied (os error 13)") carries the part the user
///    actually needs.
/// 2. Anything that still looks like a path is replaced. A home directory can
///    contain a real name, and a status toast is the one place in the TUI that
///    ends up in screenshots and bug reports. Truncation keeps a pathological
///    error from taking over the footer.
fn safe_error_detail(err: &anyhow::Error) -> String {
    const MAX: usize = 160;
    let raw = err.root_cause().to_string();
    let scrubbed = raw
        .split_whitespace()
        .map(|token| {
            let looks_like_path = token.contains('/')
                || token.contains('\\')
                || (token.len() > 2 && token.as_bytes()[1] == b':');
            if looks_like_path { "<path>" } else { token }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if scrubbed.chars().count() > MAX {
        let truncated: String = scrubbed.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        scrubbed
    }
}

/// The single serialized owner of startup-default writes for one `App`.
///
/// Cheap to clone; every clone shares the same queue, write mutex, and failure
/// mailbox. See the module docs for why ordering comes from the enqueue rather
/// than from task scheduling.
#[derive(Debug, Clone, Default)]
pub struct StartupDefaultsWriter {
    inner: Arc<WriterInner>,
}

#[derive(Debug, Default)]
struct WriterInner {
    /// Pending updates in user-action order. Only ever popped while `write` is
    /// held, so a drain is a strict prefix of this queue.
    queue: Mutex<VecDeque<StartupDefaults>>,
    /// Held across an entire load / modify / save, so transactions never
    /// interleave.
    write: Mutex<()>,
    failures: StartupDefaultFailures,
}

impl StartupDefaultsWriter {
    /// Queue `update` and persist it off the event loop.
    ///
    /// Returns immediately: the queue push is the only work done on the calling
    /// thread, so keystroke-rate actions never wait on disk. When no tokio
    /// runtime is running (unit tests, and the non-async construction paths
    /// that mirror `file_picker`'s scan fallback) the drain happens inline so
    /// behavior stays observable and deterministic.
    pub fn spawn(&self, update: StartupDefaults) {
        if update.is_empty() {
            return;
        }
        // Checked *before* the enqueue: an unauthorized test must leave no trace
        // at all, so a later authorized drain cannot inherit its work and so it
        // never takes out a claim another test would have to wait on. Always
        // true in production.
        if !spawn_writes_permitted() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            // No runtime (unit tests, and the non-async construction paths that
            // mirror `file_picker`'s scan fallback): drain inline so behavior
            // stays observable and deterministic.
            self.lock_queue().push_back(update);
            self.drain_pending();
            return;
        }
        // Captured on the *calling* thread, which under `cfg(test)` is the
        // sealed-`HOME` test thread. It carries that test's env scope into the
        // blocking pool and keeps that scope's write gate open until the drain is
        // finished. See the module docs for why both matter.
        #[cfg(test)]
        let ticket = match TestDrainTicket::capture() {
            Some(ticket) => ticket,
            None => {
                // Authorized to write, but not the env scope's *owner*, so this
                // thread cannot hand the scope to a worker. Handing the write to
                // an unenrolled background thread would park it on the sealing
                // test's env lock; this thread is already inside the sealed
                // environment, so write here instead of skipping.
                self.lock_queue().push_back(update);
                self.drain_pending();
                return;
            }
        };
        self.lock_queue().push_back(update);
        let writer = self.clone();
        codewhale_util::utils::spawn_blocking_supervised("startup-defaults-persist", move || {
            #[cfg(test)]
            let _scope = ticket.enter();
            writer.drain_pending();
        });
    }

    /// Persist `update` on the calling thread and report whether it landed.
    ///
    /// Used by the model and effort pickers, which record a setup receipt whose
    /// honesty depends on knowing the write succeeded. Anything the user did
    /// *before* this call is already queued and is applied first, under the
    /// same lock, so this cannot silently overwrite a newer selection.
    pub fn apply_blocking(&self, update: StartupDefaults) -> anyhow::Result<()> {
        let _write = self.lock_write();
        self.drain_locked();
        if update.is_empty() {
            return Ok(());
        }
        update.apply()
    }

    /// Block until the queue is empty and no transaction is in flight.
    ///
    /// Available in production, not only in tests: at shutdown the last thing
    /// the user did may still be sitting in the queue, and the process is about
    /// to exit. Taking the write lock and draining here is a join — a
    /// background task that already holds the lock finishes first, and anything
    /// still queued is applied on this thread.
    ///
    /// Never call this from a thread that holds a settings transaction; see the
    /// no-deadlock contract in the module docs.
    pub fn flush(&self) {
        let _write = self.lock_write();
        self.drain_locked();
    }

    /// Flush at process shutdown and hand back everything that failed.
    ///
    /// Returns the failures instead of toasting them because the caller is past
    /// the last redraw: the TUI's toast surface will never be painted again, so
    /// the only honest way to report is on the restored terminal after the
    /// alternate screen is gone.
    #[must_use]
    pub fn shutdown(&self) -> Vec<StartupDefaultFailure> {
        self.flush();
        self.drain_failures()
    }

    /// How many updates are queued but not yet applied. Tests use it to prove
    /// an unauthorized caller enqueued *nothing*, rather than enqueuing work a
    /// later drain could inherit.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.lock_queue().len()
    }

    /// Drain any pending failures for display.
    pub fn drain_failures(&self) -> Vec<StartupDefaultFailure> {
        self.inner.failures.drain()
    }

    fn drain_pending(&self) {
        let _write = self.lock_write();
        self.drain_locked();
    }

    /// Apply every queued update in FIFO order. Caller holds the write lock.
    fn drain_locked(&self) {
        loop {
            let Some(update) = self.lock_queue().pop_front() else {
                return;
            };
            // Re-check the test gate at apply time, not just at enqueue time.
            // `TestWriteGuard` now waits for outstanding drains, so a straggler
            // from a *gated* test can no longer reach here after its `HOME`
            // guard is gone. This stays as the backstop for the untracked
            // paths — an inline drain reached from a later, ungated test, or a
            // queue entry that survived a panicking transaction.
            if !spawn_writes_permitted() {
                continue;
            }
            update.apply_reporting(&self.inner.failures);
        }
    }

    /// Take the transaction lock.
    ///
    /// A panic inside one transaction must not wedge persistence for the rest
    /// of the session; the mutex protects ordering, not invariants, so a
    /// poisoned guard is recovered rather than propagated.
    #[cfg(not(test))]
    fn lock_write(&self) -> MutexGuard<'_, ()> {
        self.inner
            .write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Test build of `lock_write`, with a watchdog.
    ///
    /// Production blocks indefinitely, which is correct: the only thing ahead
    /// of it is a bounded settings transaction. In a test binary an indefinite
    /// wait is indistinguishable from the lock-order inversion described in the
    /// module docs, and a hung test job reports nothing. Polling with a deadline
    /// is not a synchronization device — every acquisition below is expected to
    /// succeed on the first `try_lock` or shortly after — it exists purely so a
    /// regression fails loudly.
    #[cfg(test)]
    fn lock_write(&self) -> MutexGuard<'_, ()> {
        use std::sync::TryLockError;

        let deadline = std::time::Instant::now() + WRITE_LOCK_TEST_DEADLINE;
        loop {
            match self.inner.write.try_lock() {
                Ok(guard) => return guard,
                Err(TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {}
            }
            assert!(
                std::time::Instant::now() < deadline,
                "startup-defaults write lock was not released within {WRITE_LOCK_TEST_DEADLINE:?}. \
                 Some thread is holding it across a settings transaction that cannot finish — \
                 usually because it is blocked on a lock this test already holds (see the \
                 no-deadlock contract in tui::startup_defaults)."
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn lock_queue(&self) -> MutexGuard<'_, VecDeque<StartupDefaults>> {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Whether the fire-and-forget [`StartupDefaultsWriter::spawn`] path may touch
/// disk *on the calling thread's behalf*.
///
/// Mode and thinking cycling happen inside a great many `App` unit tests that do
/// not seal `HOME`. Those tests predate this write and must not start rewriting
/// the developer's real `~/.codewhale/settings.toml`, so under `cfg(test)` the
/// background write is inert unless the caller is inside a sealed env scope that
/// opted in with `allow_writes_in_tests`.
///
/// **This is deliberately not a process-global flag.** A global bool is true for
/// as long as *any* test has opted in, so an unrelated test running in parallel
/// would pass the gate, resolve no env scope of its own, and then block on the
/// sealed test's env lock inside path resolution — while the sealed test's guard
/// waited for that very drain to finish. Authorization is therefore keyed to the
/// concrete env-scope generation the write belongs to, and only a thread that is
/// actually inside that scope (its owner, or a worker it adopted) is permitted.
///
/// In production this is unconditionally true: a real write is never skipped.
///
/// The synchronous [`StartupDefaultsWriter::apply_blocking`] path is not gated:
/// its callers (the model/effort pickers) seal `HOME` themselves and need to
/// know whether the write landed.
fn spawn_writes_permitted() -> bool {
    #[cfg(test)]
    {
        authorized_test_write_generation().is_some()
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// The env-scope generation the calling thread is authorized to write for, if
/// any: it must be inside a live env scope *and* that scope must be the one a
/// live [`TestWriteGuard`] opened.
#[cfg(test)]
fn authorized_test_write_generation() -> Option<u64> {
    let generation = crate::test_support::current_env_scope_generation()?;
    let scopes = lock_test_write_scopes();
    scopes
        .authorized
        .contains(&generation)
        .then_some(generation)
}

/// How long a test may wait for the transaction lock, or for outstanding
/// background drains, before it is treated as wedged. Every real wait here is
/// sub-millisecond; this only has to be shorter than a CI job timeout and
/// longer than any honest settings write.
#[cfg(test)]
const WRITE_LOCK_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Which env-scope generations may write, and how many background drains each
/// one still has outstanding.
///
/// Keyed by generation so a guard only ever waits for the drains *it* authorized.
/// Sharing one count across all tests is what let a sealed test's `drop` block
/// on a foreign test's queued work.
#[cfg(test)]
#[derive(Default)]
struct TestWriteScopes {
    authorized: Vec<u64>,
    outstanding: Vec<(u64, usize)>,
}

#[cfg(test)]
impl TestWriteScopes {
    fn outstanding_for(&self, generation: u64) -> usize {
        self.outstanding
            .iter()
            .find(|(scope, _)| *scope == generation)
            .map_or(0, |(_, count)| *count)
    }

    fn adjust(&mut self, generation: u64, delta: isize) {
        if let Some(entry) = self
            .outstanding
            .iter_mut()
            .find(|(scope, _)| *scope == generation)
        {
            entry.1 = entry.1.saturating_add_signed(delta);
            if entry.1 == 0 {
                self.outstanding.retain(|(scope, _)| *scope != generation);
            }
        } else if delta > 0 {
            self.outstanding.push((generation, delta as usize));
        }
    }
}

#[cfg(test)]
fn test_write_scopes() -> &'static (Mutex<TestWriteScopes>, std::sync::Condvar) {
    static SCOPES: std::sync::OnceLock<(Mutex<TestWriteScopes>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    SCOPES.get_or_init(|| {
        (
            Mutex::new(TestWriteScopes::default()),
            std::sync::Condvar::new(),
        )
    })
}

#[cfg(test)]
fn lock_test_write_scopes() -> MutexGuard<'static, TestWriteScopes> {
    test_write_scopes()
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Opt the calling test's sealed env scope into real background startup-default
/// writes.
///
/// Callers must already hold `test_support::lock_test_env()` and have sealed
/// `HOME`. Panics otherwise, because an ungated opt-in would authorize writes
/// into the developer's real settings file.
#[cfg(test)]
pub(crate) fn allow_writes_in_tests() -> TestWriteGuard {
    let generation = crate::test_support::current_env_scope_generation().expect(
        "allow_writes_in_tests() requires the calling thread to hold \
         test_support::lock_test_env() with a sealed HOME",
    );
    let mut scopes = lock_test_write_scopes();
    if !scopes.authorized.contains(&generation) {
        scopes.authorized.push(generation);
    }
    drop(scopes);
    TestWriteGuard { generation }
}

#[cfg(test)]
pub(crate) struct TestWriteGuard {
    generation: u64,
}

#[cfg(test)]
impl Drop for TestWriteGuard {
    /// Close this scope's gate only once every drain *this scope* handed to the
    /// blocking pool has finished.
    ///
    /// The drain re-checks authorization per item, which stops a straggler from
    /// writing after the gate closes — but it does so by *discarding* the item,
    /// which is a silent hole in whatever the test just asserted, and it does not
    /// stop a straggler that is already mid-write. Waiting here makes the gate's
    /// lifetime cover the writes it authorized.
    ///
    /// It cannot deadlock on a foreign test: the wait is scoped to this
    /// generation, and every drain counted under this generation is enrolled in
    /// this test's env scope, so none of them can be parked on a lock this
    /// thread holds.
    fn drop(&mut self) {
        let drained = wait_for_outstanding_test_drains(self.generation);
        let mut scopes = lock_test_write_scopes();
        scopes.authorized.retain(|scope| *scope != self.generation);
        drop(scopes);
        // Report the timeout as a failure, but never as a second panic during
        // an unwind — that aborts the whole test binary and hides the real
        // assertion that started it.
        assert!(
            drained || std::thread::panicking(),
            "background startup-default drain(s) for env scope {} did not finish within \
             {WRITE_LOCK_TEST_DEADLINE:?}; see the no-deadlock contract in \
             tui::startup_defaults",
            self.generation
        );
    }
}

/// Wait for every drain authorized by `generation`, returning `false` if the
/// deadline expired first. Never panics: the caller decides how to report a
/// timeout.
#[cfg(test)]
fn wait_for_outstanding_test_drains(generation: u64) -> bool {
    let (scopes, done) = test_write_scopes();
    let mut guard = scopes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let deadline = std::time::Instant::now() + WRITE_LOCK_TEST_DEADLINE;
    while guard.outstanding_for(generation) > 0 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let (next, _timeout) = done
            .wait_timeout(guard, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard = next;
    }
    true
}

/// One background drain's claim on the enclosing test: its env scope, and its
/// slot in that scope's outstanding-drain count.
///
/// Minted only when the spawning thread is authorized, so an unauthorized test
/// never enqueues work and never takes out a claim it would then have to be
/// waited on for. Captured on the spawning thread and moved into the blocking
/// task, so the count is decremented whether the task ran or the runtime shut
/// down and dropped it un-run.
#[cfg(test)]
struct TestDrainTicket {
    env: crate::test_support::EnvScopeTicket,
}

#[cfg(test)]
impl TestDrainTicket {
    /// `None` when the caller is not the owner of an authorized env scope — the
    /// caller must then not enqueue anything.
    fn capture() -> Option<Self> {
        let generation = authorized_test_write_generation()?;
        let env = crate::test_support::env_scope_ticket()?;
        // Only the scope owner can hand its scope to a worker, and the ticket
        // must describe the same generation we just authorized.
        if env.generation() != generation {
            return None;
        }
        lock_test_write_scopes().adjust(generation, 1);
        Some(Self { env })
    }

    fn enter(&self) -> Option<crate::test_support::EnvScopeMembership> {
        crate::test_support::join_env_scope(Some(self.env))
    }
}

#[cfg(test)]
impl Drop for TestDrainTicket {
    fn drop(&mut self) {
        let (scopes, done) = test_write_scopes();
        let mut guard = scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.adjust(self.env.generation(), -1);
        drop(guard);
        done.notify_all();
    }
}

/// Mailbox for non-blocking startup-default write failures.
///
/// The event loop drains this every iteration so a failed write becomes a
/// visible warning instead of a silent revert on the next launch, and shutdown
/// drains it one last time so a write that failed after the final redraw is
/// still reported.
#[derive(Debug, Clone, Default)]
pub struct StartupDefaultFailures(Arc<Mutex<Vec<StartupDefaultFailure>>>);

impl StartupDefaultFailures {
    fn record(&self, failure: StartupDefaultFailure) {
        // A poisoned mailbox must not take down the writer thread; the write
        // itself already happened (or failed) and was logged.
        if let Ok(mut guard) = self.0.lock() {
            guard.push(failure);
        }
    }

    /// Take every pending failure, leaving the mailbox empty.
    pub fn drain(&self) -> Vec<StartupDefaultFailure> {
        self.0
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_update_only_targets_default_mode() {
        let update = StartupDefaults::mode(AppMode::Operate);
        assert_eq!(update.mode, Some("operate"));
        assert!(update.reasoning_effort.is_none());
        assert_eq!(update.subjects(), vec![StartupDefaultSubject::Mode]);
    }

    #[test]
    fn subjects_stay_typed_for_a_combined_mode_and_thinking_update() {
        let update = StartupDefaults::mode(AppMode::Operate).with_reasoning_effort("high");
        assert_eq!(
            update.subjects(),
            vec![StartupDefaultSubject::Mode, StartupDefaultSubject::Thinking,]
        );
    }

    /// A failure toast is one of the few strings that reliably ends up in a
    /// screenshot, so the detail must not carry the settings path (which
    /// contains the user's home directory, and often their real name).
    #[test]
    fn safe_error_detail_keeps_the_cause_and_drops_the_path() {
        let err = anyhow::anyhow!("Permission denied (os error 13)")
            .context("Failed to write settings to /Users/real-name/.codewhale/settings.toml");
        let detail = safe_error_detail(&err);
        assert_eq!(detail, "Permission denied (os error 13)");
        assert!(!detail.contains("real-name"));
        assert!(!detail.contains(".codewhale"));

        // Even when the root cause itself names a path, nothing path-shaped
        // survives.
        let rooted = anyhow::anyhow!("cannot open /Users/real-name/.codewhale/settings.toml");
        let scrubbed = safe_error_detail(&rooted);
        assert_eq!(scrubbed, "cannot open <path>");
    }

    #[test]
    fn empty_update_is_a_no_op() {
        assert!(StartupDefaults::default().is_empty());
        StartupDefaults::default()
            .apply()
            .expect("empty update must not touch disk");
    }

    #[test]
    fn failure_mailbox_drains_once() {
        let failures = StartupDefaultFailures::default();
        let failure = StartupDefaultFailure {
            subjects: vec![StartupDefaultSubject::Mode],
            detail: "boom".to_string(),
        };
        failures.record(failure.clone());
        assert_eq!(failures.drain(), vec![failure]);
        assert!(failures.drain().is_empty());
    }

    /// The deadlock this module shipped with, reduced to its two threads.
    ///
    /// A worker takes the transaction lock and runs a settings transaction
    /// while the test thread — which holds the process-wide env lock for its
    /// whole body — waits for that worker. Before `spawn` enrolled its drain in
    /// the test's env scope, the worker parked inside
    /// `settings_path_candidates` holding `write`, the test thread parked on
    /// `write`, and the test hung instead of failing.
    ///
    /// The channel is the barrier: a regression makes `recv_timeout` expire and
    /// the test *fails*, with no thread left for CI to wait on.
    #[test]
    fn a_worker_enrolled_in_the_test_env_scope_completes_a_transaction() {
        use std::sync::mpsc;
        use std::time::Duration;

        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _user_profile = crate::test_support::EnvVarGuard::set("USERPROFILE", tmp.path());
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path().join(".codewhale"));
        let _deepseek_config = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");
        let _codewhale_config = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _writes = allow_writes_in_tests();

        let writer = StartupDefaultsWriter::default();
        let ticket = crate::test_support::env_scope_ticket();
        assert!(
            ticket.is_some(),
            "the thread holding lock_test_env must be able to mint a scope ticket"
        );

        let (done_tx, done_rx) = mpsc::channel();
        let worker = writer.clone();
        let handle = std::thread::spawn(move || {
            let _membership = crate::test_support::join_env_scope(ticket);
            let result = worker.apply_blocking(StartupDefaults::mode(AppMode::Operate));
            done_tx.send(result).ok();
        });

        let result = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("an enrolled worker must not block on the env lock its own test holds");
        result.expect("the transaction must land");
        handle.join().expect("worker thread");

        // Reachable from the env-lock holder for the same reason.
        writer.flush();

        // Proof the worker resolved the *sealed* settings path rather than
        // falling back to the isolated root a foreign reader would get.
        assert_eq!(
            Settings::load_persisted()
                .expect("reload settings")
                .default_mode,
            "operate"
        );
        assert!(tmp.path().join(".codewhale/settings.toml").exists());
    }

    /// Seal `HOME`/`CODEWHALE_HOME` onto `tmp`. Caller must already hold
    /// `lock_test_env()`.
    fn seal_home(tmp: &std::path::Path) -> Vec<crate::test_support::EnvVarGuard> {
        use crate::test_support::EnvVarGuard;
        vec![
            EnvVarGuard::set("HOME", tmp),
            EnvVarGuard::set("USERPROFILE", tmp),
            EnvVarGuard::set("CODEWHALE_HOME", tmp.join(".codewhale")),
            EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH"),
            EnvVarGuard::remove("CODEWHALE_CONFIG_PATH"),
        ]
    }

    /// The second deadlock the scoped gate exists to prevent.
    ///
    /// A process-global "writes enabled" bool is true for as long as *any* test
    /// has opted in. An unrelated test running in parallel — no env lock of its
    /// own, so no sealed `HOME` — would therefore pass the gate, enqueue an
    /// update, and hand it to a blocking thread that could mint no env-scope
    /// ticket. That thread then parked inside `settings_path_candidates` waiting
    /// for the env lock *this* test holds, while this test's `TestWriteGuard`
    /// drop waited for that same drain to finish: a 15-second mutual wait ending
    /// in a failure that pointed at the wrong test.
    ///
    /// Two things are asserted, both of which used to be false:
    ///
    /// 1. The unauthorized thread returns promptly instead of blocking. The
    ///    channel is the barrier — a regression expires `recv_timeout` and the
    ///    test *fails* rather than hanging CI.
    /// 2. It leaves nothing behind: no queue entry (which a later authorized
    ///    drain would inherit and write), no outstanding-drain claim, and no
    ///    settings file — in particular not the developer's real one.
    #[test]
    fn an_unauthorized_thread_neither_writes_nor_waits_for_a_sealed_scope() {
        use std::sync::mpsc;
        use std::time::Duration;

        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _env = seal_home(tmp.path());
        let _writes = allow_writes_in_tests();

        let writer = StartupDefaultsWriter::default();
        let foreign = writer.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            // Deliberately *not* enrolled: this is the shape of an unrelated
            // `App` test calling `select_mode` while another test is sealed.
            foreign.spawn(StartupDefaults::mode(AppMode::Plan));
            done_tx.send(()).ok();
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("an unauthorized writer must return immediately, never block on the env lock");
        handle.join().expect("foreign thread");

        assert_eq!(
            writer.pending_len(),
            0,
            "an unauthorized caller must not enqueue work an authorized drain could inherit"
        );
        assert!(
            !tmp.path().join(".codewhale/settings.toml").exists(),
            "an unauthorized caller must not write any settings file"
        );

        // The sealed scope itself is unaffected: its own write still lands, and
        // its guard has nothing foreign to wait for.
        writer
            .apply_blocking(StartupDefaults::mode(AppMode::Operate))
            .expect("the sealed scope's own write must land");
        assert_eq!(
            Settings::load_persisted()
                .expect("reload settings")
                .default_mode,
            "operate"
        );
    }

    /// Two sealed scopes must not be able to authorize, or wait for, each
    /// other's work.
    ///
    /// The env mutex means two sealed bodies never overlap, but their *drains*
    /// can: a straggler handed to the blocking pool by scope N can still be
    /// alive when scope N+1 opens. With one global flag and one global
    /// outstanding count, scope N+1 inherited both — it could write under scope
    /// N's authorization, and either guard could block on the other's work.
    /// Authorization and drain accounting are keyed to the env-scope generation
    /// so neither is possible.
    #[test]
    fn two_sealed_scopes_share_neither_write_authorization_nor_drain_accounting() {
        let first_generation;
        let stale_ticket;

        {
            let _lock = crate::test_support::lock_test_env();
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let _env = seal_home(tmp.path());
            let _writes = allow_writes_in_tests();

            first_generation = crate::test_support::current_env_scope_generation()
                .expect("a sealed scope must have a generation");
            stale_ticket = crate::test_support::env_scope_ticket();
            assert_eq!(
                authorized_test_write_generation(),
                Some(first_generation),
                "the scope that opted in must be the one authorized"
            );

            let writer = StartupDefaultsWriter::default();
            writer
                .apply_blocking(StartupDefaults::mode(AppMode::Plan))
                .expect("first scope's write must land");
            assert_eq!(
                Settings::load_persisted().expect("reload").default_mode,
                "plan"
            );
            assert_eq!(
                lock_test_write_scopes().outstanding_for(first_generation),
                0,
                "the first scope must have no outstanding drain left to wait on"
            );
        }

        {
            let _lock = crate::test_support::lock_test_env();
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let _env = seal_home(tmp.path());

            let second_generation = crate::test_support::current_env_scope_generation()
                .expect("a sealed scope must have a generation");
            assert_ne!(
                second_generation, first_generation,
                "each acquisition must open a fresh generation"
            );
            assert_eq!(
                authorized_test_write_generation(),
                None,
                "a new scope must not inherit the previous scope's opt-in"
            );
            assert!(
                crate::test_support::join_env_scope(stale_ticket).is_none(),
                "a ticket from a closed scope must not enroll a thread in the current one"
            );

            // Without its own opt-in, this scope's background path stays inert
            // and writes nothing — not even into its own sealed HOME.
            let writer = StartupDefaultsWriter::default();
            writer.spawn(StartupDefaults::mode(AppMode::Operate));
            assert_eq!(writer.pending_len(), 0);
            assert!(!tmp.path().join(".codewhale/settings.toml").exists());

            // With its own opt-in it writes into *its* home, keyed to *its*
            // generation.
            let _writes = allow_writes_in_tests();
            assert_eq!(authorized_test_write_generation(), Some(second_generation));
            writer.spawn(StartupDefaults::mode(AppMode::Operate));
            writer.flush();
            assert_eq!(
                Settings::load_persisted().expect("reload").default_mode,
                "operate"
            );
            assert!(tmp.path().join(".codewhale/settings.toml").exists());
        }
    }

    /// The transactional boundary is `Settings::transact`, not this writer's own
    /// mutex.
    ///
    /// A direct settings writer holds the transaction across its whole
    /// load / modify / save. A concurrent startup-default drain must not be able
    /// to slip a save in between — if it could, this test's `save` would write
    /// back a pre-image and silently revert the queued selection, which is
    /// exactly how `default_mode` was lost to a Shift+Tab posture write.
    #[test]
    fn a_direct_writer_holds_the_boundary_against_a_queued_startup_default() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _env = seal_home(tmp.path());
        let _writes = allow_writes_in_tests();

        let writer = StartupDefaultsWriter::default();
        let ticket = crate::test_support::env_scope_ticket();

        let mut handle = None;
        codewhale_runtime::settings::with_settings_transaction(|transaction| {
            let mut direct = transaction.load().expect("load inside the transaction");
            // A field with a non-default value, so the final assertion cannot
            // pass by accident: `max_input_history` defaults to 100.
            direct
                .set("max_history", "321")
                .expect("set an unrelated field");

            // A drain on another thread, enrolled so it resolves the sealed
            // path. It must block on the transaction above rather than
            // interleave.
            let queued = writer.clone();
            handle = Some(std::thread::spawn(move || {
                let _membership = crate::test_support::join_env_scope(ticket);
                queued
                    .apply_blocking(StartupDefaults::mode(AppMode::Plan))
                    .expect("the queued write must land once the boundary is released");
            }));

            // Give the worker a real chance to reach (and be refused by) the
            // boundary. This is not the assertion's synchronization — the
            // assertion is that the value is *still absent*, which a sleep can
            // only make easier to violate.
            std::thread::sleep(std::time::Duration::from_millis(150));
            assert_eq!(
                transaction.load().expect("re-read").default_mode,
                Settings::default().default_mode,
                "no other writer may save while a transaction is open"
            );

            transaction.save(&direct).expect("commit the direct write");
            Ok(())
        })
        .expect("the direct transaction must complete");
        handle
            .expect("worker spawned")
            .join()
            .expect("queued writer thread");

        let settled = Settings::load_persisted().expect("reload");
        assert_eq!(
            settled.max_input_history, 321,
            "the direct write must survive the queued startup-default write"
        );
        assert_eq!(
            settled.default_mode, "plan",
            "the queued startup-default write must survive the direct write"
        );
    }
}

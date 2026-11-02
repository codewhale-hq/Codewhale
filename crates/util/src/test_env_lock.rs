//! Process-wide test-environment barrier.

use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread::ThreadId;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Who currently counts as "inside" the process-wide env lock.
///
/// The owner is the thread holding [`TestEnvLock`]. `adopted` holds helper
/// threads that owner explicitly enrolled with [`join_env_scope`] — see that
/// function for why a worker thread of the current test must not be treated as
/// a foreign reader.
#[derive(Default)]
struct EnvScope {
    /// Bumped on every acquisition, so a ticket minted by an earlier test can
    /// never enroll a thread into a later test's environment.
    generation: u64,
    owner: Option<ThreadId>,
    adopted: Vec<ThreadId>,
}

fn env_scope() -> &'static Mutex<EnvScope> {
    static SCOPE: OnceLock<Mutex<EnvScope>> = OnceLock::new();
    SCOPE.get_or_init(|| Mutex::new(EnvScope::default()))
}

fn lock_env_scope() -> MutexGuard<'static, EnvScope> {
    match env_scope().lock() {
        Ok(scope) => scope,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn open_env_scope() {
    let mut scope = lock_env_scope();
    scope.generation = scope.generation.wrapping_add(1);
    scope.owner = Some(std::thread::current().id());
    scope.adopted.clear();
}

fn current_thread_owns_contended_env_lock() -> bool {
    let scope = lock_env_scope();
    let current = std::thread::current().id();
    scope.owner == Some(current) || scope.adopted.contains(&current)
}

/// Proof that the calling thread belongs to a live [`lock_test_env`] scope, handed to
/// a worker thread so it can join that scope with [`join_env_scope`].
///
/// Owners and adopted workers can pass the same live generation to child
/// workers. Foreign threads cannot mint a ticket for a test they did not join.
#[derive(Clone, Copy, Debug)]
pub struct EnvScopeTicket {
    generation: u64,
}

impl EnvScopeTicket {
    /// Which sealed environment this ticket authorizes. Callers that gate real
    /// disk writes on a live scope key their bookkeeping by this value, so a
    /// straggler from generation N can never be mistaken for work belonging to
    /// generation N+1.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The generation of the env scope the calling thread is currently inside, as
/// owner or as a [`join_env_scope`]-adopted worker; `None` when the thread is a
/// foreign reader with no sealed environment of its own.
///
/// This is the authorization primitive for anything that must only touch disk
/// on behalf of a test that actually sealed `HOME`. A process-global "writes
/// are enabled" flag cannot distinguish unrelated parallel tests.
pub fn current_env_scope_generation() -> Option<u64> {
    let scope = lock_env_scope();
    let current = std::thread::current().id();
    if scope.owner == Some(current) || scope.adopted.contains(&current) {
        Some(scope.generation)
    } else {
        None
    }
}

pub fn env_scope_ticket() -> Option<EnvScopeTicket> {
    let scope = lock_env_scope();
    let current = std::thread::current().id();
    (scope.owner == Some(current) || scope.adopted.contains(&current)).then_some(EnvScopeTicket {
        generation: scope.generation,
    })
}

/// Enroll the calling thread in the ticket's env scope for as long as the
/// returned guard lives.
///
/// [`with_test_env_lock`] stops a foreign test from resolving another test's
/// temporary `HOME`. A helper thread doing work for the sealing test must see
/// that same environment without blocking on the mutex its owner holds.
pub fn join_env_scope(ticket: Option<EnvScopeTicket>) -> Option<EnvScopeMembership> {
    let ticket = ticket?;
    let mut scope = lock_env_scope();
    if scope.owner.is_none() || scope.generation != ticket.generation {
        return None;
    }
    let thread = std::thread::current().id();
    if !scope.adopted.contains(&thread) {
        scope.adopted.push(thread);
    }
    Some(EnvScopeMembership {
        generation: ticket.generation,
        thread,
    })
}

pub struct EnvScopeMembership {
    generation: u64,
    thread: ThreadId,
}

impl Drop for EnvScopeMembership {
    fn drop(&mut self) {
        let mut scope = lock_env_scope();
        if scope.generation == self.generation {
            scope.adopted.retain(|thread| *thread != self.thread);
        }
    }
}

/// Owned process-wide test-environment lock.
///
/// Clearing the owner before the underlying mutex unlocks keeps re-entrant
/// reader detection exact. Closing the scope also evicts adopted workers, so
/// enrollment cannot outlive the test that granted it.
pub struct TestEnvLock {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for TestEnvLock {
    fn drop(&mut self) {
        let mut scope = lock_env_scope();
        if scope.owner == Some(std::thread::current().id()) {
            scope.owner = None;
            scope.adopted.clear();
        }
    }
}

/// Acquire the process-wide env-var mutex.
///
/// If a prior test panicked while holding the lock, recover the guard instead
/// of cascading failures across unrelated tests.
pub fn lock_test_env() -> TestEnvLock {
    let guard = match env_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    open_env_scope();
    TestEnvLock { _guard: guard }
}

/// Read process-global test environment while respecting [`lock_test_env`].
///
/// The owner check makes the barrier re-entrant for a test that reads its own
/// guarded override.
pub fn with_test_env_lock<T>(read: impl FnOnce() -> T) -> T {
    if current_thread_owns_contended_env_lock() {
        return read();
    }

    let _guard = lock_test_env();
    read()
}

/// [`with_test_env_lock`] for readers that run inside a `LazyLock`/`OnceLock`
/// initializer: takes the barrier when it is free, and reads *without* it
/// rather than waiting when another thread holds it.
///
/// Blocking is not an option there. The initializer's own lock is held for the
/// duration, so a second thread that already owns the env barrier and then
/// touches the same lazy value deadlocks against this one: A waits for the
/// initializer, B (this thread) waits for A's barrier. Both threads wedge, and
/// libtest has no per-test timeout, so the whole test binary hangs instead of
/// failing — which is how a single wedged run burns an entire CI job.
///
/// The trade is deliberate and small: an uncontended read is still fully
/// serialized, and a contended one reads a variable mid-mutation at worst.
/// Callers must therefore only use this for values whose staleness is
/// tolerable, never to gate a disk write.
pub fn with_test_env_lock_if_uncontended<T>(read: impl FnOnce() -> T) -> T {
    if current_thread_owns_contended_env_lock() {
        return read();
    }

    match env_lock().try_lock() {
        Ok(_guard) => read(),
        Err(TryLockError::Poisoned(poisoned)) => {
            let _guard = poisoned.into_inner();
            read()
        }
        Err(TryLockError::WouldBlock) => read(),
    }
}

pub fn current_thread_holds_test_env_lock() -> bool {
    match env_lock().try_lock() {
        Ok(guard) => {
            drop(guard);
            false
        }
        Err(TryLockError::Poisoned(poisoned)) => {
            drop(poisoned.into_inner());
            false
        }
        Err(TryLockError::WouldBlock) => current_thread_owns_contended_env_lock(),
    }
}

#[cfg(test)]
mod tests {
    use super::{lock_test_env, with_test_env_lock_if_uncontended};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn adopted_worker_can_pass_only_its_live_environment_generation() {
        let owner = lock_test_env();
        let ticket = super::env_scope_ticket().unwrap();
        let nested = std::thread::spawn(move || {
            assert!(
                super::env_scope_ticket().is_none(),
                "foreign thread has no authority"
            );
            let _membership = super::join_env_scope(Some(ticket)).unwrap();
            let child_ticket =
                super::env_scope_ticket().expect("adopted worker can enroll its child");
            std::thread::spawn(move || {
                let _membership = super::join_env_scope(Some(child_ticket)).unwrap();
                assert_eq!(
                    super::current_env_scope_generation(),
                    Some(child_ticket.generation())
                );
                assert_eq!(super::with_test_env_lock(|| 7), 7);
                child_ticket
            })
            .join()
            .unwrap()
        })
        .join()
        .unwrap();
        drop(owner);
        let _next = lock_test_env();
        assert!(
            super::join_env_scope(Some(nested)).is_none(),
            "old generation cannot join the next test"
        );
    }

    /// The lock-order inversion this helper exists to break.
    ///
    /// A reader running inside a `LazyLock`/`OnceLock` initializer must not wait
    /// for the env barrier: the thread holding the barrier may itself be waiting
    /// on that initializer. Both threads then wedge, and libtest has no per-test
    /// timeout, so the entire test binary hangs — reproduced locally as
    /// `cargo test -p codewhale-tui --lib -- shell` never returning.
    #[test]
    fn uncontended_read_returns_while_another_thread_holds_the_barrier() {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = lock_test_env();
            held_tx.send(()).expect("announce that the barrier is held");
            // Outlive the assertion below without depending on timing.
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
        });
        held_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("holder thread must take the barrier");

        let start = Instant::now();
        let value = with_test_env_lock_if_uncontended(|| 7);
        let elapsed = start.elapsed();

        release_tx.send(()).ok();
        holder.join().expect("holder thread");

        assert_eq!(value, 7, "the read still runs, just unsynchronized");
        assert!(
            elapsed < Duration::from_secs(1),
            "a contended read must not block; waited {elapsed:?}"
        );
    }
}

use std::sync::{Mutex, OnceLock};

use codewhale_config::WorkshopConfig;

static ACTIVE_WORKSHOP: OnceLock<Mutex<WorkshopConfig>> = OnceLock::new();

#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
static ACTIVE_WORKSHOP_TEST_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
std::thread_local! {
    static ACTIVE_WORKSHOP_TEST_SERIAL_HELD: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

/// Holds every test-side workshop activation behind one process-wide gate.
/// The thread-local marker lets the owning current-thread test call
/// `install_active` without trying to acquire its own non-reentrant lock.
///
/// The guard also owns the process-wide test env barrier, because holding this
/// gate across a `reload_config` is holding it across `Settings::load` and
/// `Config`'s env reads, which take that barrier (#6306). Field order is the
/// release order: the serial gate first, then the barrier.
#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
pub struct ActiveWorkshopTestGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    _env: Option<crate::test_support::TestEnvLock>,
}

#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
impl Drop for ActiveWorkshopTestGuard {
    fn drop(&mut self) {
        ACTIVE_WORKSHOP_TEST_SERIAL_HELD.with(|held| held.set(false));
    }
}

/// Take the workshop gate, and the env barrier under it, in that order (#6306).
///
/// A holder of this gate keeps it across whole product calls — `reload_config`
/// installs the budgets and then loads settings — and those calls read the
/// environment through `test_support::with_test_env_lock`. A test that sealed
/// the environment first and then reaches `install_active` through the very
/// same `reload_config` takes the two locks in the opposite order, and the
/// pair wedges the whole binary: `cargo test -p codewhale-tui --lib config`
/// never returned, with dozens of unrelated tests queued on the barrier.
/// Acquiring the barrier here, before the gate, makes that inversion
/// impossible for every caller at once — the same shape as
/// `provider_lake::lock_live_snapshot`.
///
/// Known limitation: this orders these two locks and nothing else. A future
/// process-wide test gate held across product code has to join the same order
/// rather than invent a third one.
#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
pub fn active_workshop_test_guard() -> ActiveWorkshopTestGuard {
    assert!(
        !ACTIVE_WORKSHOP_TEST_SERIAL_HELD.with(std::cell::Cell::get),
        "active workshop test guard is not reentrant"
    );
    let env = if crate::test_support::current_thread_holds_test_env_lock() {
        None
    } else {
        Some(crate::test_support::lock_test_env())
    };
    let serial = ACTIVE_WORKSHOP_TEST_SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ACTIVE_WORKSHOP_TEST_SERIAL_HELD.with(|held| held.set(true));
    ActiveWorkshopTestGuard {
        _serial: serial,
        _env: env,
    }
}

fn active_workshop_slot() -> &'static Mutex<WorkshopConfig> {
    ACTIVE_WORKSHOP.get_or_init(|| Mutex::new(WorkshopConfig::default()))
}

/// Install the process-wide workshop budgets used by read/tool compactors.
///
/// The returned immutable receipt is the snapshot written while the
/// singleton lock was held. Callers that need evidence of their own
/// activation can inspect it without racing a later process-wide update.
pub fn install_active_workshop(config: Option<&WorkshopConfig>) -> WorkshopConfig {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    let _test_serial = if ACTIVE_WORKSHOP_TEST_SERIAL_HELD.with(std::cell::Cell::get) {
        None
    } else {
        Some(
            ACTIVE_WORKSHOP_TEST_SERIAL
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    };
    let snapshot = config.cloned().unwrap_or_default();
    let mut slot = active_workshop_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = snapshot;
    slot.clone()
}

/// Optional model-visible read budget, when the user opted in (#5367).
#[must_use]
pub fn active_read_result_max_bytes() -> Option<usize> {
    active_workshop_slot()
        .lock()
        .ok()
        .and_then(|cfg| cfg.read_result_max_bytes.filter(|n| *n > 0))
}

/// Optional model-visible tool-result budget, when the user opted in (#5367).
#[must_use]
pub fn active_tool_result_max_bytes() -> Option<usize> {
    active_workshop_slot()
        .lock()
        .ok()
        .and_then(|cfg| cfg.tool_result_max_bytes.filter(|n| *n > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_byte_budgets_raise_floor_only() {
        let _guard = active_workshop_test_guard();
        let installed = install_active_workshop(Some(&WorkshopConfig {
            large_output_threshold_tokens: None,
            per_tool_thresholds: None,
            read_result_max_bytes: Some(102_400),
            tool_result_max_bytes: Some(80_000),
        }));
        assert_eq!(installed.read_result_max_bytes, Some(102_400));
        assert_eq!(installed.tool_result_max_bytes, Some(80_000));
        assert_eq!(active_read_result_max_bytes(), Some(102_400));
        assert_eq!(active_tool_result_max_bytes(), Some(80_000));
        let cleared = install_active_workshop(None);
        assert_eq!(cleared.read_result_max_bytes, None);
        assert_eq!(cleared.tool_result_max_bytes, None);
        assert_eq!(active_read_result_max_bytes(), None);
        assert_eq!(active_tool_result_max_bytes(), None);
    }
}

//! Lightweight verbose logging helpers for the CLI.

use std::sync::atomic::{AtomicBool, Ordering};

use colored::Colorize;

use codewhale_palette as palette;
static VERBOSE: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static VERBOSE_SNAPSHOT: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose logging output.
pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::SeqCst);
}

/// Capture the current verbose state so the TUI can restore it after
/// temporarily suppressing Windows alt-screen output.
#[cfg(windows)]
pub fn snapshot_verbose_state() {
    VERBOSE_SNAPSHOT.store(is_verbose(), Ordering::SeqCst);
}

/// Restore the last captured verbose state.
#[cfg(windows)]
pub fn restore_verbose_state() {
    set_verbose(VERBOSE_SNAPSHOT.load(Ordering::SeqCst));
}

/// Return true when `CODEWHALE_LOG_LEVEL` (legacy alias: `DEEPSEEK_LOG_LEVEL`)
/// requests verbose output.
///
/// Note: `RUST_LOG` is intentionally NOT checked here — it controls the
/// `tracing` subscriber filter in `runtime_log.rs` (file logging) and
/// should not gate CLI verbose output. On Windows, where stderr is not
/// redirected to the log file, coupling the two causes tracing log
/// messages to leak into the TUI alt-screen.
#[must_use]
pub fn env_requests_verbose_logging() -> bool {
    std::env::var("CODEWHALE_LOG_LEVEL")
        .or_else(|_| std::env::var("DEEPSEEK_LOG_LEVEL"))
        .ok()
        .is_some_and(|value| log_value_enables_verbose(&value))
}

fn log_value_enables_verbose(value: &str) -> bool {
    value.split(',').any(|directive| {
        let level = directive
            .rsplit('=')
            .next()
            .unwrap_or(directive)
            .trim()
            .to_ascii_lowercase();
        matches!(level.as_str(), "trace" | "debug" | "info")
    })
}

/// Check whether verbose logging is enabled.
#[must_use]
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}

/// Emit a verbose info message (no-op when verbosity is disabled).
pub fn info(message: impl AsRef<str>) {
    if is_verbose() {
        let (r, g, b) = palette::WHALE_ACTION_RGB;
        eprintln!("{} {}", "info".truecolor(r, g, b).bold(), message.as_ref());
    }
}

/// Emit a verbose warning message (no-op when verbosity is disabled).
pub fn warn(message: impl AsRef<str>) {
    if is_verbose() {
        let (r, g, b) = palette::WHALE_ACTION_RGB;
        eprintln!("{} {}", "warn".truecolor(r, g, b).bold(), message.as_ref());
    }
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn log_value_parser_accepts_common_rust_log_directives() {
        assert!(log_value_enables_verbose("debug"));
        assert!(log_value_enables_verbose("codewhale_cli=debug"));
        assert!(log_value_enables_verbose(
            "warn,codewhale_runtime::client=trace"
        ));
        assert!(!log_value_enables_verbose("warn"));
        assert!(!log_value_enables_verbose("codewhale_runtime=off"));
    }

    #[test]
    fn snapshot_and_restore_verbose_state_round_trip() {
        let _guard = TEST_GUARD.lock().unwrap_or_else(|err| err.into_inner());

        set_verbose(false);
        snapshot_verbose_state();
        set_verbose(true);
        restore_verbose_state();
        assert!(!is_verbose());

        set_verbose(true);
        snapshot_verbose_state();
        set_verbose(false);
        restore_verbose_state();
        assert!(is_verbose());

        set_verbose(false);
    }

    #[test]
    fn restore_keeps_cli_verbose_state_even_when_env_is_not_verbose() {
        let _guard = TEST_GUARD.lock().unwrap_or_else(|err| err.into_inner());

        set_verbose(true);
        snapshot_verbose_state();

        // Simulate the Windows alt-screen suppression path. The restore must
        // bring back the pre-suppression CLI state without depending on the
        // environment.
        set_verbose(false);
        restore_verbose_state();

        assert!(is_verbose());
        set_verbose(false);
    }
}

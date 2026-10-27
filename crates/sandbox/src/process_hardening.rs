//! Process hardening for Linux sandbox defense-in-depth (#2183).
//!
//! This module applies kernel-level restrictions to the codewhale-tui process
//! itself. These hardening measures protect the *parent* TUI process and its
//! descendants from information leaks and privilege-escalation vectors; they
//! are not a filesystem or network sandbox for child commands.
//!
//! # Ordering constraints
//!
//! `apply_process_hardening()` MUST be called **before** the Tokio runtime is
//! booted and **before** any worker threads are spawned. The reasons:
//!
//! 1. `PR_SET_DUMPABLE` — once set to 0, the process cannot be ptraced and
//!    `/proc/self/` becomes root-owned. This must happen before any threads
//!    exist, because the kernel applies dumpable state per-thread-group and
//!    changing it after threads are live can race with `/proc` lookups.
//!
//! 2. `PR_SET_NO_NEW_PRIVS` — prevents the process and all descendants from
//!    ever gaining new privileges via setuid/setgid/fscaps. This is
//!    irreversible and must be applied before executing any helper binaries or
//!    subprocesses that might (incorrectly) rely on privilege boundaries.
//!    Because this also blocks intentional privilege gains — `sudo`, `su`,
//!    setuid helpers — a user who runs Codewhale as a wheel/wheel-equivalent
//!    administrator and wants the model to be able to escalate can opt out of
//!    exactly this one measure with `CODEWHALE_NO_NEW_PRIVS=0` (#5413), and a
//!    session whose startup sandbox mode resolves to `danger-full-access`
//!    skips it by default so full access means what it says (#5723). The
//!    other two measures stay on in every posture.
//!
//! 3. `RLIMIT_CORE` — disables core dumps so that sensitive in-memory data
//!    (API keys, tokens, prompt content) is never written to disk on a crash.
//!    Setting this before any data is loaded into memory is the safest posture.
//!
//! # Platform support
//!
//! These hardening measures are Linux-only (they use `prctl` and `setrlimit`
//! from the `libc` crate). On non-Linux platforms, `apply_process_hardening()`
//! is a no-op that logs a debug-level message.

/// Environment variable carrying the explicit no-new-privileges decision.
///
/// `PR_SET_NO_NEW_PRIVS` is an irreversible, inherited-by-children kernel
/// flag, so applying it by default breaks workflows where the user *wants*
/// Codewhale to be able to escalate: `sudo`, `su`, setuid/fscaps helpers run
/// by a wheel-group administrator (#5413). Setting this variable to any
/// falsey value (`0`, `false`, `no`, `off`, `disabled`, or empty) skips
/// exactly this one measure; any other set value forces it on, and leaving it
/// unset lets the startup sandbox posture decide (#5723). `PR_SET_DUMPABLE`
/// and `RLIMIT_CORE` are never skipped.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
pub(crate) const NO_NEW_PRIVS_ENV: &str = "CODEWHALE_NO_NEW_PRIVS";

/// Whether a `CODEWHALE_NO_NEW_PRIVS` value requests skipping the
/// no-new-privileges flag. Falsey per the workspace env convention — the same
/// value set (`""`, `0`, `false`, `no`, `off`, `disabled`) that
/// `docs/CONFIGURATION.md` treats as "not set".
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
fn is_no_new_privs_opt_out(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off" | "disabled"
    )
}

/// The explicit `CODEWHALE_NO_NEW_PRIVS` decision as the tri-state
/// `should_apply_no_new_privs` consumes: `Some(false)` opts out (falsey
/// value), `Some(true)` forces the flag on (any other set value), and `None`
/// leaves the decision to the startup posture.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
fn no_new_privs_env_override() -> Option<bool> {
    std::env::var_os(NO_NEW_PRIVS_ENV)
        .map(|value| !is_no_new_privs_opt_out(&value.to_string_lossy()))
}

/// Whether startup should set the kernel's irreversible no-new-privileges
/// flag, given the resolved startup sandbox mode and the explicit
/// `CODEWHALE_NO_NEW_PRIVS` override.
///
/// Precedence (#5723):
///
/// 1. An explicit override wins in both directions: a falsey value skips the
///    flag (#5413); a truthy value forces it on even under
///    `danger-full-access`.
/// 2. Otherwise a startup resolved to `danger-full-access` skips the flag so
///    "full access (sandbox disabled)" means it: `sudo`/`su`/setuid helpers
///    keep working from the agent shell.
/// 3. Every other — or unreadable — posture keeps the flag. Defense-in-depth
///    stays the default (#5413) and the decision is fail-closed.
///
/// The mode string is normalized the same way config validation normalizes
/// `sandbox_mode` (`Config::validate`): trimmed, ASCII case-insensitive. An
/// unrecognized value keeps the flag on — it cannot silently relax into a
/// posture the runtime would reject.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
pub(crate) fn should_apply_no_new_privs(
    resolved_startup_mode: Option<&str>,
    env_override: Option<bool>,
) -> bool {
    if let Some(apply) = env_override {
        return apply;
    }
    !resolved_startup_mode
        .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("danger-full-access"))
}

/// Live state of the kernel's no-new-privileges flag for this process tree.
///
/// Returns `Some(true)`/`Some(false)` on Linux, where `PR_GET_NO_NEW_PRIVS`
/// reads back the irreversible flag set (or deliberately skipped) at startup,
/// and `None` on platforms where the flag does not exist or the query fails.
/// Status and denial surfaces use this to disclose the residual setuid block
/// truthfully instead of replaying the startup decision (#5723).
pub fn no_new_privs_active() -> Option<bool> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        // Safety: PR_GET_NO_NEW_PRIVS only reads the calling process's flag.
        let result = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0i64, 0i64, 0i64, 0i64) };
        if result < 0 { None } else { Some(result == 1) }
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        None
    }
}

/// Apply process-level hardening measures.
///
/// On Linux, this:
/// - Sets `PR_SET_DUMPABLE` to 0 (prevents ptrace, core dumps)
/// - Sets `PR_SET_NO_NEW_PRIVS` to 1 (irreversible no-new-privileges), unless
///   `should_apply_no_new_privs` skips it: an explicit falsey
///   `CODEWHALE_NO_NEW_PRIVS` (#5413), or a startup sandbox mode resolved to
///   `danger-full-access` (#5723)
/// - Sets `RLIMIT_CORE` to 0 (disables core dumps)
///
/// On non-Linux platforms this is a no-op.
///
/// `resolved_startup_mode` is the narrow pre-parse read of the startup sandbox
/// mode (`CODEWHALE_SANDBOX_MODE`, else the config file's `sandbox_mode` key);
/// see `run_with_args` in the `codewhale-tui` crate root for the seam and its limits.
///
/// # Panics
///
/// Does NOT panic. Failures are logged via `tracing::warn` because the
/// hardening is defense-in-depth. A failure does not abort startup or change
/// whether a separately configured Seatbelt/bubblewrap command wrapper is
/// available.
pub fn apply_process_hardening(resolved_startup_mode: Option<&str>) {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        apply_linux_hardening(resolved_startup_mode);
    }
    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    {
        let _ = resolved_startup_mode;
        tracing::debug!("Process hardening skipped: not on Linux");
    }
}

/// Linux-specific hardening implementation.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn apply_linux_hardening(resolved_startup_mode: Option<&str>) {
    // ── PR_SET_DUMPABLE = 0 ────────────────────────────────────────────────
    //
    // When dumpable is 0:
    // - The process cannot be ptraced by non-root
    // - /proc/<pid>/ becomes owned by root:root (mode 0400)
    // - No core dumps are produced
    //
    // Pattern from openai/codex codex-rs/codex-sandbox/src/linux.rs; reimplemented.
    //
    // Safety: prctl with PR_SET_DUMPABLE modifies only the calling process.
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0i64, 0i64, 0i64, 0i64) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            "PR_SET_DUMPABLE failed ({}); continuing without this hardening",
            err
        );
    } else {
        tracing::debug!("PR_SET_DUMPABLE=0 applied");
    }

    // ── PR_SET_NO_NEW_PRIVS = 1 ────────────────────────────────────────────
    //
    // Once set, neither this process nor any descendant can ever gain new
    // privileges via setuid, setgid, file capabilities, or LSMs like SELinux
    // transitions. This is the strongest anti-escalation primitive the kernel
    // offers.
    //
    // That strength is also the flag's one legitimate break: a wheel-group
    // administrator running Codewhale over ssh loses `sudo`/`su`/setuid
    // helpers for the whole process tree (#5413). Two startup-level paths skip
    // exactly this measure, before any thread exists — the same point in
    // startup where the flag itself is applied:
    //
    // - `CODEWHALE_NO_NEW_PRIVS` with a falsey value (#5413). A truthy value
    //   is the opposite explicit decision: it forces the flag on even under a
    //   `danger-full-access` startup.
    // - A startup sandbox mode resolved to `danger-full-access` (#5723):
    //   "full access (sandbox disabled)" must mean it, so the agent shell's
    //   `sudo`/setuid workflows keep working. Every narrower posture keeps
    //   the flag as defense-in-depth.
    //
    // Pattern from openai/codex codex-rs/codex-sandbox/src/linux.rs; reimplemented.
    //
    // Safety: prctl with PR_SET_NO_NEW_PRIVS modifies only the calling process
    // and its future descendants.
    let env_override = no_new_privs_env_override();
    if should_apply_no_new_privs(resolved_startup_mode, env_override) {
        let result = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1i64, 0i64, 0i64, 0i64) };
        if result != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(
                "PR_SET_NO_NEW_PRIVS failed ({}); continuing without this hardening",
                err
            );
        } else {
            tracing::debug!("PR_SET_NO_NEW_PRIVS=1 applied");
        }
    } else if env_override == Some(false) {
        tracing::info!(
            target: "sandbox",
            "PR_SET_NO_NEW_PRIVS skipped via {NO_NEW_PRIVS_ENV}: setuid/sudo escalation is \
             allowed for this process tree"
        );
    } else {
        tracing::info!(
            target: "sandbox",
            "PR_SET_NO_NEW_PRIVS skipped: startup sandbox mode resolved to danger-full-access \
             (#5723); setuid/sudo escalation is allowed for this process tree"
        );
    }

    // ── RLIMIT_CORE = 0 ────────────────────────────────────────────────────
    //
    // Disables core dumps at the rlimit level. In combination with
    // PR_SET_DUMPABLE=0, this provides a belt-and-suspenders guarantee that
    // no core file will ever be written.
    //
    // Safety: setrlimit modifies resource limits for the calling process only.
    let rlim_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let result = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raw const rlim_core) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            "RLIMIT_CORE failed ({}); continuing without this hardening",
            err
        );
    } else {
        tracing::debug!("RLIMIT_CORE=0 applied");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_process_hardening_does_not_panic() {
        // This test exists to ensure the function can be called without
        // panicking, even on platforms where hardening is a no-op.
        apply_process_hardening(None);
    }

    #[test]
    fn no_new_privs_opt_out_accepts_exactly_the_falsey_values() {
        // The workspace env convention: "", 0, false, no, off, disabled.
        for value in ["", "0", "false", "no", "off", "disabled"] {
            assert!(
                is_no_new_privs_opt_out(value),
                "{value:?} should opt out of PR_SET_NO_NEW_PRIVS"
            );
            // Case and surrounding whitespace must not change the answer.
            assert!(is_no_new_privs_opt_out(&format!(
                " {} ",
                value.to_uppercase()
            )));
        }
    }

    #[test]
    fn no_new_privs_opt_out_rejects_truthy_and_garbage_values() {
        // Anything else — including a typo — keeps the hardening on, so the
        // opt-out can never be entered by accident.
        for value in [
            "1",
            "true",
            "yes",
            "on",
            "enabled",
            "maybe",
            "0x0",
            "false-ish",
            "off!",
        ] {
            assert!(
                !is_no_new_privs_opt_out(value),
                "{value:?} should NOT opt out of PR_SET_NO_NEW_PRIVS"
            );
        }
    }

    #[test]
    fn no_new_privs_env_wiring_reads_the_documented_variable() {
        // The env is process-global and tests run in parallel threads, so the
        // wiring is asserted structurally: the constant is the documented
        // name, and the reader is a pure projection of var_os over that
        // constant. Setting/removing the variable here would race every other
        // test in the process.
        assert_eq!(NO_NEW_PRIVS_ENV, "CODEWHALE_NO_NEW_PRIVS");
        let projected = std::env::var_os(NO_NEW_PRIVS_ENV)
            .map(|value| !is_no_new_privs_opt_out(&value.to_string_lossy()));
        assert_eq!(projected, no_new_privs_env_override());
    }

    #[test]
    fn should_apply_no_new_privs_defaults_on_without_full_access_startup() {
        // No posture information at all, and every narrower posture, keeps
        // the defense-in-depth default (#5413).
        for mode in [
            None,
            Some("read-only"),
            Some("workspace-write"),
            Some("external-sandbox"),
        ] {
            assert!(
                should_apply_no_new_privs(mode, None),
                "{mode:?} must keep PR_SET_NO_NEW_PRIVS"
            );
        }
    }

    #[test]
    fn should_apply_no_new_privs_skips_for_danger_full_access_startup() {
        // The product decision behind #5723: "full access (sandbox disabled)"
        // must mean it, so the irreversible setuid block is relaxed when the
        // startup posture resolves to danger-full-access.
        assert!(!should_apply_no_new_privs(Some("danger-full-access"), None));
        // Mode strings are normalized the way config validation normalizes
        // them: surrounding whitespace and ASCII case do not matter.
        assert!(!should_apply_no_new_privs(
            Some(" Danger-Full-Access "),
            None
        ));
    }

    #[test]
    fn should_apply_no_new_privs_env_override_wins_over_posture() {
        // Explicit override beats posture in both directions (#5413, #5723):
        // falsey opts out under a narrow posture, truthy forces the flag on
        // under full access.
        assert!(!should_apply_no_new_privs(
            Some("workspace-write"),
            Some(false)
        ));
        assert!(!should_apply_no_new_privs(None, Some(false)));
        assert!(should_apply_no_new_privs(
            Some("danger-full-access"),
            Some(true)
        ));
        assert!(should_apply_no_new_privs(
            Some("workspace-write"),
            Some(true)
        ));
    }

    #[test]
    fn should_apply_no_new_privs_is_fail_closed_for_unknown_modes() {
        // A mode string the runtime would reject as invalid must never relax
        // the flag by accident.
        for mode in ["danger_full_access", "full-access", "yolo", "", "danger"] {
            assert!(
                should_apply_no_new_privs(Some(mode), None),
                "{mode:?} must keep PR_SET_NO_NEW_PRIVS"
            );
        }
    }
}

/// Kernel-flag proof. `PR_SET_NO_NEW_PRIVS` is irreversible and inherited by
/// descendants, so the decision can only be observed end-to-end in a fresh
/// child process: the child applies hardening under one posture and reports
/// the kernel's own record through `PR_GET_NO_NEW_PRIVS`. These tests exist only
/// on Linux; on other hosts `apply_process_hardening` is a no-op and the flag
/// does not exist.
#[cfg(all(test, target_os = "linux", not(target_env = "ohos")))]
mod linux_flag_tests {
    use super::*;

    const CHILD_ENV: &str = "CODEWHALE_NNP_TEST_CHILD";
    const CHILD_MODE_ENV: &str = "CODEWHALE_NNP_TEST_MODE";

    /// Child-process entry point. Run directly (child env marker unset) it is
    /// a trivial pass; run under the parent tests below it applies hardening
    /// with the posture named by `CODEWHALE_NNP_TEST_MODE` and prints the
    /// kernel-recorded flag as `NNP=0|1`.
    #[test]
    fn no_new_privs_child_reports_kernel_flag() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let mode = std::env::var(CHILD_MODE_ENV).ok();
        apply_process_hardening(mode.as_deref());
        // Read the flag back through PR_GET_NO_NEW_PRIVS, not
        // /proc/self/status: no_new_privs is per-task and /proc/self/status
        // shows the *main* thread's flag, while libtest runs this test on a
        // spawned thread — the /proc read always reports 0 here.
        let flag = no_new_privs_active().expect("PR_GET_NO_NEW_PRIVS reads the flag on Linux");
        println!("NNP={}", if flag { "1" } else { "0" });
    }

    fn run_child(mode: Option<&str>, env_override: Option<&str>) -> String {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["no_new_privs_child_reports_kernel_flag", "--nocapture"])
            .env(CHILD_ENV, "1")
            // The ambient developer/CI environment must not leak into the
            // matrix: the child decides from exactly the inputs passed here.
            .env_remove(NO_NEW_PRIVS_ENV)
            .env_remove(CHILD_MODE_ENV)
            .envs(mode.map(|m| (CHILD_MODE_ENV, m)))
            .envs(env_override.map(|v| (NO_NEW_PRIVS_ENV, v)))
            .output()
            .expect("spawn child test process");
        assert!(
            output.status.success(),
            "child failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn ratchet_expectation(expected: &'static str, row: &str) -> &'static str {
        if expected == "0" && no_new_privs_active() == Some(true) {
            eprintln!(
                "{row}: parent already holds NoNewPrivs, which children cannot clear; \
                 asserting the child keeps NNP=1 instead of NNP=0"
            );
            return "1";
        }
        expected
    }

    #[test]
    fn no_new_privs_flag_tracks_startup_posture_in_child_processes() {
        // Never recurse when this test binary is itself the spawned child.
        if std::env::var_os(CHILD_ENV).is_some() {
            return;
        }
        for (mode, expected) in [
            (None, "1"),
            (Some("workspace-write"), "1"),
            (Some("read-only"), "1"),
            (Some("external-sandbox"), "1"),
            (Some("danger-full-access"), "0"),
        ] {
            let expected = ratchet_expectation(expected, &format!("mode {mode:?}"));
            let stdout = run_child(mode, None);
            assert!(
                stdout.contains(&format!("NNP={expected}")),
                "mode {mode:?}: expected NoNewPrivs={expected}, got:\n{stdout}"
            );
        }
    }

    #[test]
    fn no_new_privs_env_override_beats_posture_in_child_processes() {
        if std::env::var_os(CHILD_ENV).is_some() {
            return;
        }
        for (mode, override_value, expected) in [
            // Explicit truthy forces the flag on even under full access.
            ("danger-full-access", "1", "1"),
            // Explicit falsey opts out under a narrow posture (#5413).
            ("workspace-write", "0", "0"),
        ] {
            let expected = ratchet_expectation(
                expected,
                &format!("mode {mode:?} with {NO_NEW_PRIVS_ENV}={override_value}"),
            );
            let stdout = run_child(Some(mode), Some(override_value));
            assert!(
                stdout.contains(&format!("NNP={expected}")),
                "mode {mode:?} with {NO_NEW_PRIVS_ENV}={override_value}: \
                 expected NoNewPrivs={expected}, got:\n{stdout}"
            );
        }
    }
}

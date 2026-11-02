//! Keep the host from idling into sleep while a turn is in flight.
//!
//! A suspended host cannot run the engine, so nothing here survives a real
//! suspend. A provider request interrupted by sleep retains an unknown outcome
//! and is not automatically replayed. This module prevents an unattended
//! machine from idling into sleep in
//! the middle of a turn, which is how a long turn gets lost with no error at
//! all.
//!
//! Scope, stated so nobody expects more than it does:
//!
//! - It holds the platform's *idle-sleep* assertion only. An explicit `sleep`/
//!   `pmset sleepnow`, a closed lid, or a low battery still wins — refusing
//!   those is the machine owner's call, not a running task's.
//! - It is held for the duration of a turn and released on drop, so the host's
//!   power behaviour outside a turn is untouched.
//! - It follows the same gate as the rest of the host-facing chrome
//!   (`EngineConfig::terminal_chrome_enabled`): an interactive TUI turn holds
//!   it, while headless hosts — `exec`, app-server, CI — never do. A dedicated
//!   `[tui]` opt-out key is not implemented yet; the headless gate is the
//!   escape hatch today.
//! - Windows is not implemented. `SetThreadExecutionState` is thread-affine —
//!   the release has to happen on the thread that set it, which a guard that
//!   travels with a turn cannot promise. Rather than ship an untested holder
//!   that might silently never release, this is a no-op there for now.
//!
//! Release is `Drop` and never cached: a leaked inhibitor would keep a laptop
//! awake forever, which is worse than the problem this solves.
//!
//! The inhibitor is a `tokio::process` child, because the guard lives inside
//! `Engine::run_turn` on the runtime: the spawn registers with the runtime,
//! `kill_on_drop` sends the release signal, and the runtime reaps the child —
//! no `wait` runs inline on a worker (#6149). `hold` therefore has to be
//! called from within a Tokio runtime context.
//!
//! `Drop` never runs when the process is killed, crashes or calls
//! `process::exit`, so each inhibitor is also tied to this process's lifetime
//! independently of the guard: on macOS `caffeinate -w <pid>` exits with the
//! process it watches, and on Linux the pipe below closes with it.
//!
//! On Linux the lock is held by `systemd-inhibit` around a child of its own,
//! and a kill is never forwarded to that grandchild. The command is therefore
//! `cat` reading a pipe this guard holds: dropping the guard closes the pipe,
//! `cat` exits on EOF, and `systemd-inhibit` follows — nothing is left behind.

#[cfg(unix)]
use tokio::process::Child;
// Only the macOS and Linux inhibitors spawn anything; every other Unix
// (Android, the BSDs, illumos) is a no-op and would see these as dead.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Stdio;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tokio::process::Command;

/// An idle-sleep assertion held for as long as this value lives.
pub struct SleepGuard {
    /// The platform inhibitor process, when one was started. `None` means the
    /// platform has no implementation, or the process could not be started —
    /// keeping the host awake is best-effort and must never fail a turn.
    /// Dropping it is the release: the child is spawned with `kill_on_drop`.
    #[cfg(unix)]
    child: Option<Child>,
}

impl SleepGuard {
    /// Hold the host awake until the returned guard drops. Call it from the
    /// Tokio runtime: the inhibitor is a `tokio::process` child.
    #[must_use]
    pub fn hold() -> Self {
        #[cfg(unix)]
        {
            Self {
                child: start_inhibitor(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// The inhibitor's process id, for diagnostics and tests. Absent when the
    /// platform is a no-op or the process did not start.
    #[cfg(all(test, unix))]
    pub(crate) fn inhibitor_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }
}

#[cfg(unix)]
impl Drop for SleepGuard {
    fn drop(&mut self) {
        // Releasing is dropping the child: `kill_on_drop` sends the signal
        // here, synchronously, and the runtime reaps the process afterwards.
        // Explicit so the field's purpose is code rather than a lint waiver.
        drop(self.child.take());
    }
}

/// `-i` prevents idle sleep. `Drop` kills caffeinate, and macOS releases the
/// assertion with the process. `-w` watches this process too: a crash, kill
/// or `process::exit` skips `Drop`, and the reparented caffeinate would
/// otherwise keep the host awake indefinitely.
#[cfg(target_os = "macos")]
fn start_inhibitor() -> Option<Child> {
    let pid = std::process::id().to_string();
    spawn("caffeinate", &caffeinate_args(&pid))
}

#[cfg(target_os = "macos")]
fn caffeinate_args(watched_pid: &str) -> [&str; 3] {
    ["-i", "-w", watched_pid]
}

/// `--what=idle` only: an explicit suspend or a closed lid is still honoured.
/// `cat` on the guard's pipe is the command whose lifetime holds the block
/// open: it exits on EOF when the guard drops, which no signal sent to
/// `systemd-inhibit` could make a `sleep infinity` grandchild do.
#[cfg(target_os = "linux")]
fn start_inhibitor() -> Option<Child> {
    spawn(
        "systemd-inhibit",
        &[
            "--what=idle",
            "--why=Codewhale turn in flight",
            "--mode=block",
            "cat",
        ],
    )
}

/// Everything else Unix (BSD, illumos, …) has no inhibitor this module knows.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn start_inhibitor() -> Option<Child> {
    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn spawn(program: &str, args: &[&str]) -> Option<Child> {
    Command::new(program)
        .args(args)
        // The pipe is never written to: closing it when the guard drops is
        // what ends an inhibitor's own child (see the Linux inhibitor).
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Killing the inhibitor is what releases the assertion; the runtime
        // reaps the child afterwards, so nothing here waits inline.
        .kill_on_drop(true)
        .spawn()
        .ok()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Whether the process is still there. `kill(pid, 0)` asks the kernel
    /// without touching the process, so this cannot perturb the guard.
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs the permission/existence check only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// The kill is sent on drop and the runtime reaps the child on the next
    /// `SIGCHLD`, so "gone" is a short poll rather than an instant fact. If
    /// the pid were reused by a new process in that window the test would be
    /// racing itself, which is why callers assert on the guard's own child.
    async fn released(pid: u32) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while alive(pid) {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        true
    }

    #[tokio::test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn the_inhibitor_lives_exactly_as_long_as_the_guard() {
        let guard = SleepGuard::hold();
        let pid = guard
            .inhibitor_pid()
            .expect("this platform starts an inhibitor");
        assert!(alive(pid), "the inhibitor must be running while held");

        drop(guard);

        assert!(
            released(pid).await,
            "a released guard must not leave an inhibitor keeping the host awake"
        );
    }

    /// Linux: `systemd-inhibit` holds the lock around a child of its own and a
    /// kill never reaches that grandchild — the guard's pipe is what ends it.
    /// Without logind the inhibitor exits at once and the list is empty, so
    /// this proves something only where an inhibitor really runs.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn a_released_guard_leaves_no_grandchild_behind() {
        let guard = SleepGuard::hold();
        let pid = guard
            .inhibitor_pid()
            .expect("this platform starts an inhibitor");
        // Give the inhibitor a moment to fork its command.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let grandchildren: Vec<u32> =
            tokio::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .await
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|child| child.parse().ok())
                .collect();

        drop(guard);

        assert!(released(pid).await, "the inhibitor itself must be gone");
        for grandchild in grandchildren {
            assert!(
                released(grandchild).await,
                "process {grandchild} outlived the guard: the inhibitor's command must end with the guard's pipe"
            );
        }
    }

    /// macOS: when the owning process dies without running `Drop`, the
    /// inhibitor must exit on its own. A stand-in owner is killed here, since
    /// the test cannot kill its own process.
    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn the_inhibitor_exits_when_its_owner_dies_without_dropping_the_guard() {
        let mut owner = Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("start a stand-in owner process");
        let owner_pid = owner.id().expect("owner pid").to_string();
        let mut inhibitor = spawn("caffeinate", &caffeinate_args(&owner_pid))
            .expect("start caffeinate watching the stand-in owner");
        let pid = inhibitor.id().expect("inhibitor pid");
        assert!(
            alive(pid),
            "the inhibitor must be running while its owner lives"
        );

        // The owner dies without anything killing the inhibitor, as when the
        // process is SIGKILLed and the guard's `Drop` never runs.
        owner.kill().await.expect("kill the stand-in owner");

        // Waiting (rather than probing the pid) also reaps the child; if it
        // never exits, dropping it at the end of the test kills it.
        let exited =
            tokio::time::timeout(std::time::Duration::from_secs(5), inhibitor.wait()).await;
        assert!(
            exited.is_ok(),
            "an inhibitor whose owner died must not keep the host awake"
        );
    }

    /// macOS: the guard a turn really takes must watch this process. The
    /// test above proves `-w` ends caffeinate with a stand-in owner; this one
    /// proves `hold` passes it, and passes our own pid.
    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn a_held_guard_watches_the_process_that_holds_it() {
        let guard = SleepGuard::hold();
        let pid = guard
            .inhibitor_pid()
            .expect("this platform starts an inhibitor");
        let ps = std::process::Command::new("ps")
            .args(["-o", "args=", "-p", &pid.to_string()])
            .output()
            .expect("run ps");
        let args = String::from_utf8_lossy(&ps.stdout);
        assert_eq!(
            args.trim(),
            format!("caffeinate -i -w {}", std::process::id()),
            "the inhibitor must end with the process whose turn it holds"
        );
    }

    #[tokio::test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn holding_twice_holds_two_independent_inhibitors() {
        // Turns are serialized, but nothing here should assume it: two guards
        // must not share one process, or the first drop would release both.
        let first = SleepGuard::hold();
        let second = SleepGuard::hold();
        let (a, b) = (
            first.inhibitor_pid().expect("first inhibitor"),
            second.inhibitor_pid().expect("second inhibitor"),
        );
        assert_ne!(a, b, "each guard owns its own inhibitor process");
        drop(first);
        assert!(released(a).await, "the first guard released only its own");
        assert!(alive(b), "the second guard still holds the host awake");
    }
}

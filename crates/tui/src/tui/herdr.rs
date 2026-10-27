//! Herdr agent-state reporting for the Codewhale TUI.
//!
//! Inside a Herdr pane (`HERDR_ENV=1` plus `HERDR_PANE_ID`, `HERDR_BIN_PATH`
//! and `HERDR_SOCKET_PATH`) Codewhale reports its state (`idle` / `working` /
//! `blocked`), the command that reopens the current session, and releases the
//! pane when the user quits. See <https://herdr.dev/docs/add-herdr-support/>.
//!
//! Reports run on one dedicated background thread that keeps only the latest
//! pending report and bounds every CLI call, so Herdr can never slow down or
//! break the TUI. Outside Herdr every entry point is a no-op.
//!
//! Known limitation: a hard abort (panic hook or fatal-signal path) skips
//! [`release`]; Herdr's own idle-prompt safety net then clears the pane.

use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::tui::underwater::ShellPhase;

/// Stable integration id. Must not start with `herdr:`.
const SOURCE: &str = "codewhale";
/// Agent name shown in Herdr's sidebar and `herdr agent list`.
const AGENT: &str = "codewhale";
/// Upper bound on a single Herdr CLI call.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// Herdr's resume argv limits.
const MAX_RESUME_ARGS: usize = 64;
const MAX_RESUME_BYTES: usize = 8 * 1024;
/// Exit code clap-based CLIs such as Herdr use for argument/usage errors.
const USAGE_ERROR_EXIT_CODE: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
}

impl AgentState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

/// Map the TUI's perceptual phase onto Herdr's semantic state. `Typing` is the
/// user drafting while the agent waits, so it reports `idle`.
#[must_use]
pub fn state_for_phase(phase: ShellPhase) -> AgentState {
    match phase {
        ShellPhase::Approval | ShellPhase::Waiting => AgentState::Blocked,
        ShellPhase::Working | ShellPhase::Verifying => AgentState::Working,
        ShellPhase::Idle | ShellPhase::Typing | ShellPhase::Done | ShellPhase::Failed => {
            AgentState::Idle
        }
    }
}

/// A state report plus, when known, the session that `resume_argv` reopens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StateReport {
    state: AgentState,
    session_id: Option<String>,
    resume_argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HerdrEnv {
    bin: String,
    pane_id: String,
}

impl HerdrEnv {
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let non_empty = |key: &str| get(key).filter(|value| !value.trim().is_empty());
        if non_empty("HERDR_ENV").as_deref() != Some("1") {
            return None;
        }
        // The socket must exist for the CLI to reach Herdr; require it as the
        // docs do, even though we talk to Herdr through its binary.
        non_empty("HERDR_SOCKET_PATH")?;
        Some(Self {
            bin: non_empty("HERDR_BIN_PATH")?,
            pane_id: non_empty("HERDR_PANE_ID")?,
        })
    }
}

enum Job {
    Report(StateReport),
    Release,
}

#[derive(Default)]
struct Slot {
    pending: Option<Job>,
    released: bool,
    done: bool,
}

struct Reporter {
    slot: Arc<(Mutex<Slot>, Condvar)>,
    last_submitted: Mutex<Option<StateReport>>,
}

static REPORTER: OnceLock<Option<Reporter>> = OnceLock::new();
static LAST_SEQ: Mutex<u64> = Mutex::new(0);
/// Set once Herdr rejects a report carrying a resume command. Herdr before
/// 0.9.2 fails the whole report with a usage error on the `--` resume
/// separator, so after that we keep reporting state without it.
static RESUME_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn reporter() -> Option<&'static Reporter> {
    REPORTER
        .get_or_init(|| {
            let env = HerdrEnv::from_lookup(|key| std::env::var(key).ok())?;
            let slot = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
            let worker_slot = Arc::clone(&slot);
            let spawned = std::thread::Builder::new()
                .name("herdr-reporter".into())
                .spawn(move || {
                    // The CLI call is defined inside the dedicated thread's
                    // closure so its blocking wait never lands on a runtime
                    // worker (scripts/check-blocking-calls-budget.py).
                    let run = |args: &[String]| -> Option<ExitStatus> {
                        let child = Command::new(&env.bin)
                            .args(args)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                        let mut child = match child {
                            Ok(child) => child,
                            Err(error) => {
                                tracing::warn!(target: "herdr", %error, "could not run the Herdr binary");
                                return None;
                            }
                        };
                        let deadline = Instant::now() + COMMAND_TIMEOUT;
                        loop {
                            match child.try_wait() {
                                Ok(Some(status)) => return Some(status),
                                Ok(None) if Instant::now() < deadline => {
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                _ => {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    tracing::warn!(target: "herdr", "the Herdr report timed out");
                                    return None;
                                }
                            }
                        }
                    };
                    worker_loop(&env, &worker_slot, &run);
                });
            if let Err(error) = spawned {
                tracing::warn!(target: "herdr", %error, "could not start the Herdr reporter thread");
                return None;
            }
            Some(Reporter {
                slot,
                last_submitted: Mutex::new(None),
            })
        })
        .as_ref()
}

/// Report the current phase and session. Cheap enough for every UI tick:
/// outside Herdr it returns immediately, and unchanged state is dropped
/// without allocating. A newer report replaces one that has not been sent.
pub fn sync(phase: ShellPhase, session_id: Option<&str>) {
    let Some(reporter) = reporter() else {
        return;
    };
    let state = state_for_phase(phase);
    let report = {
        let mut last = lock(&reporter.last_submitted);
        if last
            .as_ref()
            .is_some_and(|last| last.state == state && last.session_id.as_deref() == session_id)
        {
            return;
        }
        let report = StateReport {
            state,
            session_id: session_id.map(str::to_owned),
            resume_argv: session_id.and_then(resume_argv_for_session),
        };
        *last = Some(report.clone());
        report
    };
    let (slot_lock, wake) = &*reporter.slot;
    let mut slot = lock(slot_lock);
    if slot.released {
        return;
    }
    slot.pending = Some(Job::Report(report));
    wake.notify_one();
}

/// Release the pane because the user is quitting. The worker sends the release
/// and this waits for it off the runtime, bounded by [`COMMAND_TIMEOUT`]. Do
/// not call this when Codewhale is about to replace itself in the pane, since
/// the same agent keeps running there.
pub async fn release() {
    let Some(reporter) = reporter() else {
        return;
    };
    let slot = Arc::clone(&reporter.slot);
    {
        let (slot_lock, wake) = &*slot;
        let mut guard = lock(slot_lock);
        if guard.released {
            return;
        }
        guard.released = true;
        guard.pending = Some(Job::Release);
        wake.notify_one();
    }
    let _ = tokio::task::spawn_blocking(move || {
        let (slot_lock, wake) = &*slot;
        let mut guard = lock(slot_lock);
        while !guard.done {
            let (next, timeout) = wake
                .wait_timeout(guard, COMMAND_TIMEOUT)
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
            if timeout.timed_out() {
                return;
            }
        }
    })
    .await;
}

fn worker_loop(
    env: &HerdrEnv,
    slot: &Arc<(Mutex<Slot>, Condvar)>,
    run: &impl Fn(&[String]) -> Option<ExitStatus>,
) {
    let (slot_lock, wake) = &**slot;
    loop {
        let job = {
            let mut guard = lock(slot_lock);
            loop {
                if guard.done {
                    return;
                }
                if let Some(job) = guard.pending.take() {
                    break job;
                }
                guard = wake.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        };
        match job {
            Job::Report(mut report) => {
                if RESUME_UNSUPPORTED.load(Ordering::Relaxed) {
                    report.resume_argv = None;
                }
                let status = run(&report_args(&env.pane_id, &report, next_seq()));
                // Only a usage error on a report that carried a resume command
                // means "this Herdr predates resume". Timeouts and other
                // failures are retried naturally by the next state change and
                // must not disable resume.
                if !(is_usage_error(status) && resume_command(&report).is_some()) {
                    continue;
                }
                report.resume_argv = None;
                let retry = run(&report_args(&env.pane_id, &report, next_seq()));
                if retry.is_some_and(|status| status.success()) {
                    RESUME_UNSUPPORTED.store(true, Ordering::Relaxed);
                    tracing::info!(
                        target: "herdr",
                        "this Herdr rejects resume commands (needs 0.9.2+); reporting state only"
                    );
                }
            }
            Job::Release => {
                run(&release_args(&env.pane_id, next_seq()));
                lock(slot_lock).done = true;
                wake.notify_all();
                return;
            }
        }
    }
}

fn is_usage_error(status: Option<ExitStatus>) -> bool {
    status.and_then(|status| status.code()) == Some(USAGE_ERROR_EXIT_CODE)
}

/// The resume command [`report_args`] sends after `--`, if any. Herdr only
/// accepts one alongside a session id, and only when it passes validation.
fn resume_command(report: &StateReport) -> Option<&Vec<String>> {
    report.session_id.as_ref()?;
    report
        .resume_argv
        .as_ref()
        .filter(|argv| valid_resume_argv(argv))
}

/// Strictly increasing across reports, sessions, and restarts: wall-clock
/// microseconds, bumped past the previous value if the clock stalls.
fn next_seq() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let mut last = lock(&LAST_SEQ);
    *last = now.max(last.saturating_add(1));
    *last
}

fn report_args(pane_id: &str, report: &StateReport, seq: u64) -> Vec<String> {
    let mut args: Vec<String> = [
        "pane",
        "report-agent",
        pane_id,
        "--source",
        SOURCE,
        "--agent",
        AGENT,
        "--state",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(report.state.as_str().to_string());
    args.push("--seq".to_string());
    args.push(seq.to_string());
    if let Some(session_id) = report.session_id.as_deref() {
        args.push("--agent-session-id".to_string());
        args.push(session_id.to_string());
        if let Some(argv) = resume_command(report) {
            args.push("--".to_string());
            args.extend(argv.iter().cloned());
        }
    }
    args
}

fn release_args(pane_id: &str, seq: u64) -> Vec<String> {
    [
        "pane",
        "release-agent",
        pane_id,
        "--source",
        SOURCE,
        "--agent",
        AGENT,
        "--seq",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(seq.to_string()))
    .collect()
}

/// Herdr rejects resume commands whose first word is a path, that contain an
/// apostrophe or control character, or that exceed 64 args / 8 KiB.
fn valid_resume_argv(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return false;
    }
    if argv.len() > MAX_RESUME_ARGS {
        return false;
    }
    let total: usize = argv.iter().map(|arg| arg.len()).sum::<usize>() + argv.len();
    if total > MAX_RESUME_BYTES {
        return false;
    }
    argv.iter()
        .all(|arg| !arg.contains('\'') && !arg.chars().any(char::is_control))
}

/// Command that reopens `session_id` from the pane's working directory. The
/// model, provider and permission posture live with the saved session.
fn resume_argv_for_session(session_id: &str) -> Option<Vec<String>> {
    let argv = vec![
        "codewhale".to_string(),
        "resume".to_string(),
        session_id.to_string(),
    ];
    valid_resume_argv(&argv).then_some(argv)
}

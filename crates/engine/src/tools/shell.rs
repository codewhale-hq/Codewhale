//! Advanced shell execution with background process support and sandboxing.
//!
//! Provides:
//! - Synchronous command execution with timeout
//! - Background process execution
//! - Process output retrieval
//! - Process termination
//! - Sandbox support (macOS Seatbelt and opt-in Linux bubblewrap)
//! - Streaming output (future)

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use uuid::Uuid;
use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::io::FromRawHandle;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(not(target_env = "ohos"))]
use portable_pty::{PtySize, native_pty_system};

mod guidance;
mod output;

use super::shell_output::{
    kill_outcome_note, output_phase_hint, summarize_output, truncate_with_meta,
};
use crate::tools::resource_admission::{
    CommandExpense, HeavyCommandPermit, MemoryPressure, acquire_heavy_command_permit,
    infer_command_expense,
};
use crate::work_graph::{
    EvidenceKind, EvidenceRef, OperationIntent, OperationOwnerSnapshot, OwnerState,
    SharedWorkRuntime,
};
use codewhale_agent_contract::worker_profile::ShellPolicy;
use codewhale_runtime::child_env;
use codewhale_sandbox::{
    CommandSpec, ExecEnv, SandboxManager, SandboxPolicy as ExecutionSandboxPolicy, SandboxType,
};
use output::{
    BoundedOutputAccumulator, BoundedOutputSnapshot, RAW_STREAM_SETTLED_TAIL_BYTES,
    RawOutputBuffer, SharedRawOutput, new_shared_raw_output, tail_from_buffer, tail_text,
    take_delta_from_buffer,
};

const READONLY_ENV_MARKER: &str = "CODEWHALE_INTERNAL_READONLY_ARGV";

#[cfg(unix)]
static PENDING_PERSISTENT_PROCESS_GROUPS: std::sync::OnceLock<
    Mutex<std::collections::HashSet<u32>>,
> = std::sync::OnceLock::new();

#[cfg(unix)]
fn pending_persistent_process_groups() -> &'static Mutex<std::collections::HashSet<u32>> {
    PENDING_PERSISTENT_PROCESS_GROUPS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

#[cfg(unix)]
fn register_pending_persistent_process_group(process_group_id: u32) {
    let mut groups = pending_persistent_process_groups()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    groups.insert(process_group_id);
}

#[cfg(unix)]
fn unregister_pending_persistent_process_group(process_group_id: u32) {
    let mut groups = pending_persistent_process_groups()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    groups.remove(&process_group_id);
}

/// Kill services that were staged for ownership transfer but have not yet
/// been released. The process-wide signal path calls this immediately before
/// `process::exit`, where Rust destructors cannot run.
#[cfg(unix)]
pub fn abort_pending_persistent_process_groups_for_exit() {
    let groups = {
        let mut groups = pending_persistent_process_groups()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        groups.drain().collect::<Vec<_>>()
    };
    for process_group_id in groups {
        if let Ok(process_group_id) = i32::try_from(process_group_id) {
            // SAFETY: the id was captured from a child spawned with
            // `process_group(0)`. A negative pid targets that child's process
            // group, never Codewhale's own group.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

fn validate_shell_working_dir(path: &Path, inherited_session_workspace: bool) -> Result<()> {
    let metadata = std::fs::metadata(path).with_context(|| {
        let source = if inherited_session_workspace {
            "saved session workspace"
        } else {
            "requested working directory"
        };
        format!(
            "{source} is unavailable: {}. Restore or remap that directory, resume/fork the session from an existing workspace, or pass an explicit `working_dir`/`cwd` to exec_shell",
            path.display()
        )
    })?;
    if !metadata.is_dir() {
        let source = if inherited_session_workspace {
            "saved session workspace"
        } else {
            "requested working directory"
        };
        return Err(anyhow!(
            "{source} is not a directory: {}. Resume/fork from an existing workspace or pass an explicit `working_dir`/`cwd`",
            path.display()
        ));
    }
    Ok(())
}

/// Status of a shell process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShellStatus {
    Running,
    Completed,
    Failed,
    Killed,
    TimedOut,
}

/// Result from a shell command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellResult {
    pub task_id: Option<String>,
    pub status: ShellStatus,
    /// Lossless process exit status. Windows exception/NTSTATUS values use
    /// the full unsigned 32-bit range, so an i32 would corrupt them.
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    /// Original stdout length in bytes.
    #[serde(default)]
    pub stdout_len: usize,
    /// Original stderr length in bytes.
    #[serde(default)]
    pub stderr_len: usize,
    /// Bytes omitted from stdout due to truncation.
    #[serde(default)]
    pub stdout_omitted: usize,
    /// Bytes omitted from stderr due to truncation.
    #[serde(default)]
    pub stderr_omitted: usize,
    /// Whether stdout was truncated.
    #[serde(default)]
    pub stdout_truncated: bool,
    /// Whether stderr was truncated.
    #[serde(default)]
    pub stderr_truncated: bool,
    /// Whether the command was executed in a sandbox.
    #[serde(default)]
    pub sandboxed: bool,
    /// Type of sandbox used (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_type: Option<String>,
    /// Whether the command was blocked by sandbox restrictions.
    #[serde(default)]
    pub sandbox_denied: bool,
}

/// Compact, UI-oriented view of a tracked background shell job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellJobSnapshot {
    pub id: String,
    pub job_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub status: ShellStatus,
    /// Explicitly backgrounded or detached from the foreground waiter.
    #[serde(default)]
    pub background: bool,
    /// First observed terminal time; legacy snapshots cannot trigger notices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub exit_code: Option<i64>,
    pub elapsed_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub stdin_available: bool,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_since_output_ms: Option<u64>,
    pub linked_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_turn_id: Option<String>,
    /// Immutable root session that launched the job. Empty legacy records are
    /// intentionally hidden from session-scoped completion drains.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_session_id: String,
}

/// Once-only completion event for a tracked background shell job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellCompletionEvent {
    pub task_id: String,
    pub command: String,
    pub status: ShellStatus,
    pub exit_code: Option<i64>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    #[serde(default)]
    pub stdout_len: usize,
    #[serde(default)]
    pub stderr_len: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    pub linked_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_session_id: String,
}

/// Byte evidence captured alongside a bounded completion event. Exact unless
/// the stream exceeded the in-memory retention ceiling, in which case the
/// omission is declared per stream rather than presented as complete.
#[derive(Debug, Clone)]
pub struct ShellCompletionEvidence {
    pub event: ShellCompletionEvent,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_omitted: usize,
    stderr_omitted: usize,
}

impl ShellCompletionEvidence {
    /// Encode each stream losslessly. UTF-8 remains readable; arbitrary bytes
    /// use base64 so `retrieve_tool_result` can still recover exact output.
    pub fn artifact_bytes(&self) -> Vec<u8> {
        fn stream(bytes: &[u8], omitted: usize) -> serde_json::Value {
            let mut value = match std::str::from_utf8(bytes) {
                Ok(content) => serde_json::json!({
                    "encoding": "utf-8",
                    "byte_length": bytes.len(),
                    "content": content,
                }),
                Err(_) => serde_json::json!({
                    "encoding": "base64",
                    "byte_length": bytes.len(),
                    "content": base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
            };
            // Additive and only present when something was actually dropped, so
            // the common case stays byte-identical to the v1 artifact readers
            // already parse.
            if omitted > 0
                && let Some(object) = value.as_object_mut()
            {
                object.insert("leading_bytes_omitted".into(), omitted.into());
                object.insert(
                    "total_byte_length".into(),
                    bytes.len().saturating_add(omitted).into(),
                );
            }
            value
        }

        serde_json::json!({
            "schema": "codewhale.shell_completion.evidence.v1",
            "task_id": self.event.task_id,
            "command": self.event.command,
            "status": format!("{:?}", self.event.status),
            "exit_code": self.event.exit_code,
            "duration_ms": self.event.duration_ms,
            "origin_tool_call_id": self.event.origin_tool_call_id,
            "origin_turn_id": self.event.origin_turn_id,
            "stdout": stream(&self.stdout, self.stdout_omitted),
            "stderr": stream(&self.stderr, self.stderr_omitted),
        })
        .to_string()
        .into_bytes()
    }
}

// Keep the two inline streams at a 2 KiB combined hard ceiling. The durable
// artifact carries the exact bytes beyond these diagnostic tails.
const SHELL_COMPLETION_TAIL_BYTES: usize = 1_024;

/// How long a finished shell record stays listed in `/jobs`.
const FINISHED_SHELL_MAX_AGE: Duration = Duration::from_secs(3600);
/// Ceiling on finished records kept for the jobs panel. A long automation run
/// makes hundreds of `Bash` calls per hour; the panel is only useful for the
/// recent ones (#5472).
const MAX_FINISHED_SHELL_RECORDS: usize = 128;
/// Ceiling on bytes still held across all finished records. Each settled record
/// releases down to a 64 KiB tail, so this only binds when many large outputs
/// finish inside the same window.
const MAX_FINISHED_SHELL_BYTES: usize = 8 * 1024 * 1024;

fn bounded_completion_tail(buffer: &SharedRawOutput, max_bytes: usize) -> (usize, String) {
    let (total, candidate) = tail_from_buffer(buffer, max_bytes);
    if candidate.len() <= max_bytes {
        return (total, candidate);
    }
    let content_budget = max_bytes.saturating_sub(3);
    let mut start = candidate.len().saturating_sub(content_budget);
    while start < candidate.len() && !candidate.is_char_boundary(start) {
        start += 1;
    }
    (total, format!("...{}", &candidate[start..]))
}

/// Optional owner attribution for background shell work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellJobOwner {
    pub agent_id: String,
    pub agent_name: String,
}

/// Full output view used by `/jobs show <id>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellJobDetail {
    pub snapshot: ShellJobSnapshot,
    pub stdout: String,
    pub stderr: String,
}

pub struct ShellDeltaResult {
    pub command: String,
    pub result: ShellResult,
    pub stdout_total_len: usize,
    pub stderr_total_len: usize,
}

/// Which of a job's raw output streams to read. Stderr is a separate stream
/// only for piped jobs; PTY and merged modes fold it into stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutputStream {
    Stdout,
    Stderr,
}

/// A non-consuming window of a job's raw output stream at absolute byte
/// offsets. Unlike [`ShellManager::get_output_delta`], reading a chunk never
/// advances anyone else's cursor, so several HTTP clients can follow the same
/// job without splitting the stream.
pub struct ShellOutputChunk {
    /// Absolute offset of `bytes[0]`. Exceeds the requested cursor when the
    /// bounded buffer already discarded that prefix — the gap is reported via
    /// `dropped`, never silently re-sent.
    pub offset: usize,
    /// Raw stream bytes. Output is arbitrary bytes, not guaranteed UTF-8.
    pub bytes: Vec<u8>,
    /// Absolute offset just past the last returned byte; the next cursor.
    pub next_offset: usize,
    /// Total bytes this stream has produced, including discarded bytes.
    pub total: usize,
    /// Leading bytes permanently discarded by the in-flight bound.
    pub dropped: usize,
    pub status: ShellStatus,
    pub exit_code: Option<i64>,
}

enum ShellChild {
    Process(Child),
    #[cfg(not(target_env = "ohos"))]
    Pty(Box<dyn portable_pty::Child + Send>),
}
#[cfg(unix)]
impl ShellChild {
    fn process_id(&self) -> Option<u32> {
        match self {
            Self::Process(child) => Some(child.id()),
            #[cfg(not(target_env = "ohos"))]
            Self::Pty(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellOwnership {
    Managed,
    PersistPending,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentServiceReceipt {
    pub task_id: String,
    pub pid: u32,
    pub process_group_id: u32,
    pub ownership: String,
}

#[cfg(unix)]
fn signal_child_process_group(child: &Child, signal: libc::c_int) -> std::io::Result<()> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return Ok(());
    }

    // SAFETY: kill(2) dereferences no pointers.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // The group is already gone (or never formed); nothing to signal.
            Ok(())
        } else {
            Err(err)
        }
    }
}

#[cfg(unix)]
fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return child.kill();
    }

    signal_child_process_group(child, libc::SIGKILL).or_else(|_| child.kill())
}

/// Bounded wait for the direct child to exit. Returns true once the child was
/// reaped (or the wait errored), false when the grace elapsed first. Unlike
/// `Child::wait`, this can never wedge the caller behind a child stuck in
/// uninterruptible sleep.
#[cfg(unix)]
fn wait_child_bounded(child: &mut Child, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Terminate a shell's whole process group with a bounded SIGTERM → SIGKILL
/// escalation (#52). The previous kill path SIGKILLed only the direct child
/// and then joined output-reader threads with no timeout, so the tool
/// returned whenever the command's descendants felt like exiting — observed
/// as a 120s foreground timeout returning after 300s. Every step here is
/// bounded: the tool returns at ~timeout + grace.
#[cfg(unix)]
fn terminate_child_process_group(child: &mut Child) -> std::io::Result<()> {
    // Cooperative stop first so shells and their children can run traps and
    // clean up; bounded so a SIGTERM-ignoring command cannot stall the caller.
    let _ = signal_child_process_group(child, libc::SIGTERM);
    if wait_child_bounded(child, KILL_TERM_GRACE) {
        // The leader exited on SIGTERM; descendants may linger, so SIGKILL
        // the rest of the group (ESRCH when it is already empty).
        kill_child_process_group(child)?;
        return Ok(());
    }
    kill_child_process_group(child)?;
    let _ = wait_child_bounded(child, KILL_REAP_GRACE);
    Ok(())
}

/// Configure parent-death signaling so shell-spawned children are reaped when
/// the TUI dies abnormally (#421). On Linux this installs
/// `PR_SET_PDEATHSIG(SIGTERM)` via `pre_exec` — the kernel then sends SIGTERM
/// to the child the moment the parent process exits, even on SIGKILL of the
/// TUI. The cancellation path already SIGKILLs the whole process group, so
/// this only fires when the parent dies without running its drop / cleanup
/// code (panic during shutdown, OOM, hardware crash, etc.).
///
/// On macOS / Windows there's no kernel equivalent. The existing graceful
/// path (`kill_child_process_group` from the cancellation token) still
/// handles normal shutdown; abnormal exit can leak children — tracked as a
/// follow-up watchdog item per the original issue's acceptance criteria.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn install_parent_death_signal(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Captured before the fork so the child can tell whether the TUI already
    // died in the fork→prctl window, where the signal would never arrive.
    let parent_pid = std::process::id() as libc::pid_t;
    // SAFETY: `pre_exec` runs in the child between fork and exec. The closure
    // only calls `libc::prctl` / `libc::getppid` with stack-allocated
    // arguments and does not touch heap memory or the parent's locks. Both
    // requirements (async-signal-safe + no allocation in the post-fork
    // window) are met.
    unsafe {
        cmd.pre_exec(move || {
            let result = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            if result == -1 {
                // Surface the errno but do not abort the spawn — the child
                // will simply lose the parent-death cleanup safety net.
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                // The TUI exited before the signal was armed: do not exec an
                // orphan nobody will ever reap.
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Watcher script for [`watch_process_group_for_parent_death`]. `$1` is the
/// process group id. The outer `sh` only forks the watcher and exits, so the
/// watcher is reparented away from the TUI and never needs reaping. The
/// watcher blocks reading the pipe the TUI holds the other end of: the read
/// ends at EOF when every write end closes, which happens however the TUI
/// dies. It then SIGTERMs the group, waits briefly and SIGKILLs it. It sits in
/// the job's own process group, so the group id cannot be reused while it
/// waits, and the TUI's normal group kills take it down with the job.
#[cfg(unix)]
const PROCESS_GROUP_WATCHER_SCRIPT: &str = r#"exec 3<&0
(
  trap '' TERM
  read -r _ <&3 || {
    kill -TERM -"$1" 2>/dev/null
    sleep 2
    kill -KILL -"$1" 2>/dev/null
  }
) >/dev/null 2>&1 &"#;

/// Kill a `Managed` background shell's whole process group when the TUI dies
/// (#6654).
///
/// `PR_SET_PDEATHSIG` is not enough here: it signals only the direct child
/// (the `sh -c` wrapper), a fork clears it in grandchildren, so a command that
/// does not `exec` its last step (`cd app && npm run dev; echo done`,
/// `a | b`) keeps its real workload alive; it also fires when the forking
/// *thread* exits, and background spawns run on threads that retire. So a
/// small watcher joins the job's process group and blocks on a pipe whose
/// write end only the TUI holds (it is close-on-exec, so no child inherits
/// it). The returned write end lives in the `BackgroundShell`; closing it —
/// by dropping the shell or by the TUI dying in any way, SIGKILL included —
/// makes the watcher terminate the group. This works on Linux and macOS alike.
///
/// Best effort: if the watcher cannot start, the job still runs, just
/// without this cleanup. A TUI that dies between the job spawn and the
/// watcher spawn also leaves the job running.
#[cfg(unix)]
fn watch_process_group_for_parent_death(
    process_group_id: u32,
) -> std::io::Result<std::io::PipeWriter> {
    let (reader, writer) = std::io::pipe()?;
    let pgid = i32::try_from(process_group_id)
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut cmd = Command::new("/bin/sh");
    cmd.args([
        "-c",
        PROCESS_GROUP_WATCHER_SCRIPT,
        "codewhale-bg-watch",
        &process_group_id.to_string(),
    ])
    .stdin(Stdio::from(reader))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(pgid);
    // The outer `sh` exits as soon as it has forked the watcher.
    let status = cmd.spawn()?.wait()?;
    if status.success() {
        Ok(writer)
    } else {
        Err(std::io::Error::other(format!(
            "process-group watcher exited with {status}"
        )))
    }
}

/// Attach `args` to a `std::process::Command`, honoring shell-quoting on
/// Windows.
///
/// Issue #1691: on Windows the shell command is invoked as
/// `cmd /C "chcp 65001 >NUL & <command>"`. Rust's `Command::arg` applies
/// MSVCRT (`CommandLineToArgvW`) escaping, turning the embedded `"` in a
/// quoted argument (e.g. `git commit -m "feat: complete sub-pages"`) into
/// `\"`. `cmd.exe` does NOT use MSVCRT parsing — it treats `\` literally and
/// `"` as a bare quote toggle — so the escaped payload is mis-tokenized and
/// `git` receives `feat:`, `complete`, `sub-pages"` as separate pathspecs
/// (the reported `pathspec 'sub-pages"' did not match` symptom). Passing the
/// `cmd /C` payload through `CommandExt::raw_arg` suppresses std's escaping so
/// the string reaches `cmd.exe` verbatim, exactly as a terminal would.
#[cfg(windows)]
fn push_shell_args(cmd: &mut Command, program: &str, args: &[String]) {
    use std::os::windows::process::CommandExt;
    // The `cmd /C <payload>` shape is the only place std's per-arg escaping
    // corrupts a quoted command. Pass `/C` and the payload raw so the quotes
    // survive; any other program keeps normal (correct) escaping. Match `cmd`
    // by file stem so a full path (`C:\Windows\System32\cmd.exe`) or `.exe`
    // suffix still triggers the raw-arg path.
    let is_cmd = std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("cmd"))
        .unwrap_or(false);
    if is_cmd && args.len() == 2 && args[0].eq_ignore_ascii_case("/C") {
        cmd.raw_arg(&args[0]);
        cmd.raw_arg(&args[1]);
    } else {
        cmd.args(args);
    }
}

#[cfg(not(windows))]
fn push_shell_args(cmd: &mut Command, _program: &str, args: &[String]) {
    // Unix delegates tokenization entirely to `sh -c <command>`; the command
    // string is passed as a single argv entry and never split by us.
    cmd.args(args);
}

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
fn install_parent_death_signal(_cmd: &mut Command) {
    // No kernel-level equivalent on macOS / Windows. The cooperative
    // cancellation + process_group SIGKILL path covers normal shutdown;
    // abnormal exit (panic without unwind, SIGKILL of the TUI) can still
    // leak children on those platforms — tracked as a follow-up. Pipe-backed
    // background shells are covered on every platform: Unix by the
    // process-group watcher (`watch_process_group_for_parent_death`), Windows
    // by their KILL_ON_JOB_CLOSE job object.
    //
    // Known limitations on every platform (#6654): `tty: true` background
    // shells spawn through `portable_pty`, which never goes through
    // `std::process::Command`, so they get no watcher (or job object); and
    // staged persistent services (`persist_pending`) stay deliberately
    // unwatched because the user can take ownership of the service.
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsJob {
    handle: HANDLE,
}

#[cfg(windows)]
// SAFETY: Windows job handles are process-wide kernel handles. Moving the
// wrapper between threads does not invalidate the handle, and access is
// externally synchronized by ShellManager's mutex.
unsafe impl Send for WindowsJob {}
#[cfg(windows)]
// SAFETY: The wrapper exposes only terminate/drop operations around a kernel
// handle; concurrent use is guarded by ShellManager.
unsafe impl Sync for WindowsJob {}

#[cfg(windows)]
impl WindowsJob {
    fn attach_to_child(child: &Child) -> std::io::Result<Self> {
        // SAFETY: returned handle is owned by the new wrapper.
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()).map_err(windows_io_error)? };
        let job = Self { handle };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `limits` is live with matching size; both handles are live.
        unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(windows_io_error)?;

            let process_handle = HANDLE(child.as_raw_handle());
            AssignProcessToJobObject(job.handle, process_handle).map_err(windows_io_error)?;
        }

        Ok(job)
    }

    fn terminate(&self) -> std::io::Result<()> {
        // SAFETY: `self.handle` is a live owned job handle.
        unsafe { TerminateJobObject(self.handle, 1).map_err(windows_io_error) }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is owned here; Drop runs once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn windows_io_error(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error)
}

#[cfg(windows)]
fn terminate_windows_job(job: Option<&WindowsJob>, child: &mut Child) -> std::io::Result<()> {
    if let Some(job) = job {
        match job.terminate() {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "failed to terminate Windows job object; falling back to immediate child kill"
                );
            }
        }
    }
    child.kill()
}

#[cfg(windows)]
fn terminate_and_close_windows_job(windows_job: Option<WindowsJob>) {
    if let Some(job) = windows_job.as_ref()
        && let Err(err) = job.terminate()
    {
        tracing::warn!(
            ?err,
            "failed to terminate Windows shell job before closing job handle"
        );
    }
    drop(windows_job);
}

#[cfg(windows)]
fn terminate_child_and_close_windows_job(
    windows_job: Option<WindowsJob>,
    child: &mut Child,
) -> std::io::Result<()> {
    let result = terminate_windows_job(windows_job.as_ref(), child);
    drop(windows_job);
    result
}

#[cfg(windows)]
fn attach_windows_job(child: &Child, command: &str) -> Option<WindowsJob> {
    match WindowsJob::attach_to_child(child) {
        Ok(job) => Some(job),
        Err(error) => {
            tracing::warn!(
                ?error,
                command,
                "failed to attach Windows shell process to job object; descendant cleanup degraded"
            );
            None
        }
    }
}

#[cfg(windows)]
fn terminate_unregistered_process(child: &mut Child, job: Option<&WindowsJob>) {
    let _ = terminate_windows_job(job, child);
    let _ = child.wait();
}

#[cfg(not(windows))]
fn terminate_unregistered_process(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = kill_child_process_group(child);
        let _ = wait_child_bounded(child, KILL_REAP_GRACE);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[derive(Clone, Copy, Debug)]
struct ShellExitStatus {
    code: Option<i64>,
    success: bool,
}

impl ShellExitStatus {
    fn from_std(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code().map(std_exit_code_i64),
            success: status.success(),
        }
    }

    #[cfg(not(target_env = "ohos"))]
    fn from_pty(status: portable_pty::ExitStatus) -> Self {
        Self {
            code: Some(i64::from(status.exit_code())),
            success: status.success(),
        }
    }
}

#[cfg(windows)]
fn std_exit_code_i64(code: i32) -> i64 {
    // std exposes Windows DWORD process statuses through i32. Reinterpret
    // negative values as their original unsigned bit pattern so codes such
    // as 0xC0000005 survive JSON, persistence, and diagnostics unchanged.
    i64::from(code as u32)
}

#[cfg(not(windows))]
fn std_exit_code_i64(code: i32) -> i64 {
    i64::from(code)
}

impl ShellChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ShellExitStatus>> {
        match self {
            ShellChild::Process(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_std)),
            #[cfg(not(target_env = "ohos"))]
            ShellChild::Pty(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_pty)),
        }
    }

    #[cfg(not(windows))]
    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            ShellChild::Process(child) => kill_child_process_group(child),
            #[cfg(not(unix))]
            ShellChild::Process(child) => child.kill(),
            #[cfg(not(target_env = "ohos"))]
            ShellChild::Pty(child) => child.kill(),
        }
    }
}

enum StdinWriter {
    Pipe(ChildStdin),
    #[cfg(not(target_env = "ohos"))]
    Pty(Box<dyn Write + Send>),
}

impl StdinWriter {
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.write_all(data),
            #[cfg(not(target_env = "ohos"))]
            StdinWriter::Pty(writer) => writer.write_all(data),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.flush(),
            #[cfg(not(target_env = "ohos"))]
            StdinWriter::Pty(writer) => writer.flush(),
        }
    }
}

fn spawn_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
    buffer: SharedRawOutput,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    // `RawOutputBuffer::append` enforces the in-flight ceiling
                    // here, at the only writer, so a chatty command cannot grow
                    // the process without bound while it runs (#5472). It
                    // returns false once the stream has been abandoned, which
                    // is this thread's only exit when a descendant holds the
                    // pipe open and EOF never arrives.
                    let keep_reading = buffer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .append(&chunk[..n]);
                    if !keep_reading {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn spawn_bounded_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
    output: Arc<Mutex<BoundedOutputAccumulator>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut guard = output.lock().unwrap_or_else(|error| error.into_inner());
                    if let Err(error) = guard.append(&chunk[..n]) {
                        guard.record_error(&error);
                        return;
                    }
                }
                Err(error) => {
                    output
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .record_error(&error);
                    return;
                }
            }
        }
        let mut guard = output.lock().unwrap_or_else(|error| error.into_inner());
        if let Err(error) = guard.finish() {
            guard.record_error(&error);
        }
    })
}

#[cfg(unix)]
fn shared_output_pipe() -> io::Result<(File, File, File)> {
    let mut descriptors = [0; 2];
    // SAFETY: `pipe` initializes both descriptors on success. Each descriptor
    // is immediately transferred into exactly one owned `File`.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful `pipe` returned two live, uniquely owned descriptors.
    let reader = unsafe { File::from_raw_fd(descriptors[0]) };
    let writer = unsafe { File::from_raw_fd(descriptors[1]) };
    let stderr_writer = writer.try_clone()?;
    Ok((reader, writer, stderr_writer))
}

#[cfg(windows)]
fn shared_output_pipe() -> io::Result<(File, File, File)> {
    let mut read_handle = std::ptr::null_mut();
    let mut write_handle = std::ptr::null_mut();
    // SAFETY: CreatePipe initializes both handles on success; ownership is
    // transferred to `File` immediately below.
    if unsafe {
        windows_sys::Win32::System::Pipes::CreatePipe(
            &mut read_handle,
            &mut write_handle,
            std::ptr::null(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreatePipe returned two live, uniquely owned handles.
    let reader = unsafe { File::from_raw_handle(read_handle.cast()) };
    let writer = unsafe { File::from_raw_handle(write_handle.cast()) };
    let stderr_writer = writer.try_clone()?;
    Ok((reader, writer, stderr_writer))
}

const SYNC_READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const QUIET_NO_OUTPUT_AFTER: Duration = Duration::from_secs(60);

/// Grace between SIGTERM and SIGKILL on the shell kill path (timeout,
/// cancel, drop). Bounded so a SIGTERM-ignoring command is force-killed
/// instead of stalling the tool (#52).
#[cfg(unix)]
const KILL_TERM_GRACE: Duration = Duration::from_millis(500);
/// Bounded reap wait after SIGKILL; a child stuck in uninterruptible sleep
/// must not wedge the caller behind an unbounded `wait`.
#[cfg(unix)]
const KILL_REAP_GRACE: Duration = Duration::from_millis(1_000);
/// Bounded join for output-reader threads after the process group is killed.
/// A descendant that escaped the group (its own session/process group) keeps
/// its inherited pipe write-end open, so the reader cannot see EOF until that
/// descendant exits on its own — an unbounded join held the shell-manager
/// lock for minutes and overshot the tool timeout (#52).
const READER_JOIN_GRACE: Duration = Duration::from_millis(2_000);

fn spawn_sync_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Bounded, unlike the `read_to_end` this replaces (#5472 finding 2).
        // `recv_sync_reader_output` gives up after 5 s, but the thread lives as
        // long as the pipe does — an interactive command that keeps printing
        // grew this Vec without limit, for a result nobody was still waiting
        // for. The tail is what the caller renders, so keep the tail.
        let mut buf = RawOutputBuffer::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if !buf.append(&chunk[..n]) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        tx.send(buf.retained().to_vec()).ok();
    });
    rx
}

fn recv_sync_reader_output(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    rx.recv_timeout(SYNC_READER_DRAIN_TIMEOUT)
        .unwrap_or_default()
}

/// Cell dimensions accepted by the existing PTY owner. Pixel sizes remain
/// unspecified; callers must not allocate an unbounded terminal grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PtyDimensions {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtyDimensions {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl PtyDimensions {
    pub fn validate(self) -> Result<Self> {
        anyhow::ensure!(
            (1..=1000).contains(&self.rows) && (1..=1000).contains(&self.cols),
            "PTY rows and columns must each be between 1 and 1000"
        );
        Ok(self)
    }
}

/// A background shell process being tracked
pub struct BackgroundShell {
    pub id: String,
    pub command: String,
    pub working_dir: PathBuf,
    pub status: ShellStatus,
    background: bool,
    pub exit_code: Option<i64>,
    pub started_at: Instant,
    /// When the job reached a terminal status. A finished job reports the
    /// duration it finished with; without this, `started_at.elapsed()` kept
    /// growing and `/jobs` showed "2m 07s" for a 12-second command (#5478).
    finished_at: Option<Instant>,
    finished_at_utc: Option<chrono::DateTime<chrono::Utc>>,
    last_output_at: Instant,
    last_observed_output_len: usize,
    pub sandbox_type: SandboxType,
    pub linked_task_id: Option<String>,
    pub owner_agent: Option<ShellJobOwner>,
    owner_session_id: String,
    origin_tool_call_id: Option<String>,
    origin_turn_id: Option<String>,
    ownership: ShellOwnership,
    stdout_buffer: SharedRawOutput,
    stderr_buffer: Option<SharedRawOutput>,
    /// Lowercase `bash` streams one combined process pipe through a bounded
    /// small-contract-compatible accumulator while persisting the complete output.
    bounded_output: Option<Arc<Mutex<BoundedOutputAccumulator>>>,
    heavy_permit: Option<HeavyCommandPermit>,
    stdout_cursor: usize,
    stderr_cursor: usize,
    completion_reported: bool,
    stdin: Option<StdinWriter>,
    /// Retain the existing PTY owner for resize; never create another session.
    #[cfg(not(target_env = "ohos"))]
    pty_master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    terminal_size: Option<PtyDimensions>,
    child: Option<ShellChild>,
    #[cfg(windows)]
    windows_job: Option<WindowsJob>,
    /// Write end of the process-group watcher's pipe for `Managed` shells;
    /// closing it (drop, or the TUI dying) kills the job's group (#6654).
    #[cfg(unix)]
    #[allow(dead_code, reason = "held only so its Drop closes the pipe")]
    parent_death_watch: Option<std::io::PipeWriter>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
    work_lifecycle: Option<ShellWorkLifecycle>,
    lifecycle_seq: u64,
    last_lifecycle_status: Option<ShellStatus>,
    last_lifecycle_bytes: usize,
    /// The terminal status came from a failed `try_wait`, not an observed
    /// exit or signal. Such a run gets no execution receipt (#6689).
    wait_failed: bool,
}

#[derive(Clone)]
struct ShellWorkLifecycle {
    work: SharedWorkRuntime,
    session_id: String,
}

impl ShellWorkLifecycle {
    fn register(&self, id: &str, command: &str) -> Result<()> {
        self.work
            .register_operation(
                &self.session_id,
                OperationIntent::new(
                    format!("shell:{id}"),
                    format!("Shell · {command}"),
                    false,
                    "exec_shell",
                    id,
                ),
            )
            .map(|_| ())
            .map_err(anyhow::Error::msg)
    }

    fn observe(&self, id: &str, status: &ShellStatus, seq: u64, raw_bytes: usize) -> Result<()> {
        let owner_state = match status {
            ShellStatus::Running => OwnerState::Running,
            ShellStatus::Completed => OwnerState::Completed,
            ShellStatus::Failed | ShellStatus::TimedOut => OwnerState::Failed,
            ShellStatus::Killed => OwnerState::Cancelled,
        };
        let raw_bytes = u64::try_from(raw_bytes).unwrap_or(u64::MAX);
        let output = EvidenceRef::new(
            EvidenceKind::Receipt {
                owner: "shell".to_string(),
            },
            format!("shell:{id}:output"),
            Some(raw_bytes),
            false,
        )
        .map_err(|err| anyhow!(err.to_string()))?;
        self.work
            .reconcile_operation(
                &self.session_id,
                OperationOwnerSnapshot::new(
                    format!("shell:{id}"),
                    owner_state,
                    seq,
                    lifecycle_now_ms(),
                )
                .with_output(output),
            )
            .map(|_| ())
            .map_err(anyhow::Error::msg)
    }
}

struct ShellSpawnIntentGuard {
    lifecycle: Option<ShellWorkLifecycle>,
    id: String,
    armed: bool,
}

struct ShellSpawnContext {
    owner_agent: Option<ShellJobOwner>,
    owner_session_id: String,
    origin_tool_call_id: Option<String>,
    origin_turn_id: Option<String>,
    work_lifecycle: Option<ShellWorkLifecycle>,
}

impl ShellSpawnIntentGuard {
    /// Register the spawn intent with the Work graph.
    ///
    /// Registration is observability bookkeeping — the same subsystem already
    /// treats the `observe` half as best-effort (a graph-write failure must
    /// not relabel a completed command) — so a transiently busy To-do/Plan
    /// state must not veto the command itself. Live sessions hit this: a
    /// shell call issued right after another tool call failed outright with
    /// "To-do state is busy; operation was not registered" because
    /// `register_operation` gives the lock only a short try-lock spin.
    ///
    /// On failure the guard goes inert: the shell still runs, and no later
    /// `observe` pretends the operation was bound.
    fn new(lifecycle: Option<ShellWorkLifecycle>, id: &str, command: &str) -> Self {
        let lifecycle = lifecycle.and_then(|lifecycle| match lifecycle.register(id, command) {
            Ok(()) => Some(lifecycle),
            Err(err) => {
                tracing::warn!(
                    shell_id = %id,
                    error = %err,
                    "shell work-graph registration skipped; running without a bound operation"
                );
                None
            }
        });
        Self {
            lifecycle,
            id: id.to_string(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ShellSpawnIntentGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(lifecycle) = self.lifecycle.as_ref()
            && let Err(err) = lifecycle.observe(&self.id, &ShellStatus::Failed, 1, 0)
        {
            tracing::warn!(shell_id = %self.id, error = %err, "failed to record shell spawn failure");
        }
    }
}

impl BackgroundShell {
    /// Wall time to report: elapsed while running, frozen once finished.
    fn wall_duration(&self) -> Duration {
        self.finished_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started_at)
    }

    fn wall_millis(&self) -> u64 {
        u64::try_from(self.wall_duration().as_millis()).unwrap_or(u64::MAX)
    }

    /// Stamp the finish instant the first time a terminal status is observed.
    /// Idempotent: a later poll must not restate when the job ended.
    fn mark_finished(&mut self) {
        if self.finished_at.is_none() && self.status != ShellStatus::Running {
            self.finished_at = Some(Instant::now());
            self.finished_at_utc = Some(chrono::Utc::now());
        }
    }

    /// Check if the process has completed and update status
    fn poll(&mut self) -> bool {
        self.refresh_output_activity();
        if self.status != ShellStatus::Running {
            self.mark_finished();
            self.publish_lifecycle_best_effort();
            return true;
        }

        #[cfg(unix)]
        let pending_process_group = (self.ownership == ShellOwnership::PersistPending)
            .then(|| self.child.as_ref().and_then(ShellChild::process_id))
            .flatten();
        let completed = if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_code = status.code;
                    self.status = if status.success {
                        ShellStatus::Completed
                    } else {
                        ShellStatus::Failed
                    };
                    self.heavy_permit.take();
                    self.collect_output();
                    true
                }
                Ok(None) => false, // Still running
                Err(_) => {
                    self.status = ShellStatus::Failed;
                    self.wait_failed = true;
                    self.heavy_permit.take();
                    self.collect_output();
                    true
                }
            }
        } else {
            true
        };
        #[cfg(unix)]
        if completed && let Some(process_group_id) = pending_process_group {
            unregister_pending_persistent_process_group(process_group_id);
        }
        self.mark_finished();
        self.publish_lifecycle_best_effort();
        completed
    }

    fn publish_lifecycle(&mut self) -> Result<()> {
        let bytes = self.observed_output_len();
        if self.last_lifecycle_status.as_ref() == Some(&self.status)
            && self.last_lifecycle_bytes == bytes
        {
            return Ok(());
        }
        let next_seq = self.lifecycle_seq.saturating_add(1);
        if let Some(lifecycle) = self.work_lifecycle.as_ref() {
            lifecycle.observe(&self.id, &self.status, next_seq, bytes)?;
        }
        self.lifecycle_seq = next_seq;
        self.last_lifecycle_status = Some(self.status.clone());
        self.last_lifecycle_bytes = bytes;
        Ok(())
    }

    fn publish_lifecycle_best_effort(&mut self) {
        if let Err(err) = self.publish_lifecycle() {
            tracing::warn!(shell_id = %self.id, error = %err, "failed to reconcile shell lifecycle");
        }
    }

    fn refresh_output_activity(&mut self) {
        let observed_len = self.observed_output_len();
        if observed_len != self.last_observed_output_len {
            self.last_observed_output_len = observed_len;
            self.last_output_at = Instant::now();
        }
    }

    fn observed_output_len(&self) -> usize {
        if let Some(output) = self.bounded_output.as_ref() {
            return output
                .lock()
                .map(|output| output.total_bytes())
                .unwrap_or(0);
        }
        let stdout_len = self
            .stdout_buffer
            .lock()
            .map(|data| data.total_len())
            .unwrap_or(0);
        let stderr_len = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| buffer.lock().ok().map(|data| data.total_len()))
            .unwrap_or(0);
        stdout_len.saturating_add(stderr_len)
    }

    /// Drop everything but a bounded tail of both raw streams.
    ///
    /// Called only once a job is terminal **and** its bytes have already been
    /// delivered — either returned as the foreground tool result, or written to
    /// its durable completion artifact. Before that the full bytes are a
    /// contract; after it they are pure residency for the up-to-1 h the
    /// finished record stays listed, which is what took the owner's host to
    /// 11 GB of swap (#5472 finding 1).
    fn release_delivered_output(&mut self) {
        if self.status == ShellStatus::Running {
            return;
        }
        for buffer in [Some(&self.stdout_buffer), self.stderr_buffer.as_ref()]
            .into_iter()
            .flatten()
        {
            buffer
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .release_to_tail(RAW_STREAM_SETTLED_TAIL_BYTES);
        }
    }

    /// Bytes still held in memory for this job, for the eviction accounting in
    /// [`ShellManager::cleanup`] and for tests that assert the bound.
    fn retained_output_bytes(&self) -> usize {
        let stdout = self
            .stdout_buffer
            .lock()
            .map(|data| data.retained().len())
            .unwrap_or(0);
        let stderr = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| buffer.lock().ok().map(|data| data.retained().len()))
            .unwrap_or(0);
        stdout.saturating_add(stderr)
    }

    /// Collect output from the background threads
    fn collect_output(&mut self) {
        // Kill the whole process group before joining reader threads.
        // When the shell spawned persistent background jobs (e.g. `nohup curl`),
        // those subprocesses keep the pipe write-ends open after the shell exits.
        // Without this kill, the reader join would block until the descendant
        // exits, freezing the UI event loop that calls list_jobs() → poll() →
        // collect_output(). The joins themselves are additionally bounded
        // (READER_JOIN_GRACE) because a descendant in its own session/process
        // group escapes even the group kill (#52).
        #[cfg(unix)]
        if let Some(child) = self.child.as_mut() {
            match child {
                ShellChild::Process(proc) => {
                    let _ = kill_child_process_group(proc);
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(_) => {}
            }
        }
        #[cfg(windows)]
        terminate_and_close_windows_job(self.windows_job.take());
        if let Some(handle) = self.stdout_thread.take() {
            finish_background_reader(handle, &self.status, Some(&self.stdout_buffer));
        }
        if let Some(handle) = self.stderr_thread.take() {
            finish_background_reader(handle, &self.status, self.stderr_buffer.as_ref());
        }
        self.stdin = None;
        #[cfg(not(target_env = "ohos"))]
        {
            self.pty_master = None;
        }
        self.child = None;
    }

    fn write_stdin(&mut self, input: &str, close: bool) -> Result<()> {
        self.write_stdin_bytes(input.as_bytes(), close)
    }

    fn write_stdin_bytes(&mut self, input: &[u8], close: bool) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            if !input.is_empty() {
                stdin.write_all(input).context("Failed to write to stdin")?;
                stdin.flush().context("Failed to flush stdin")?;
            }
            if close {
                self.stdin = None;
            }
            return Ok(());
        }

        if input.is_empty() && close {
            return Ok(());
        }

        Err(anyhow!("stdin is not available for task {}", self.id))
    }

    fn full_output(&self) -> (String, String, usize, usize) {
        if let Some(snapshot) = self.bounded_output_snapshot(false).ok().flatten() {
            return (snapshot.content, String::new(), snapshot.total_bytes, 0);
        }
        let (stdout_bytes, stderr_bytes, stdout_omitted, stderr_omitted) =
            self.retained_output_bytes_with_omissions();
        // Report what the stream produced, not what is still held.
        let stdout_len = stdout_bytes.len().saturating_add(stdout_omitted);
        let stderr_len = stderr_bytes.len().saturating_add(stderr_omitted);

        (
            String::from_utf8_lossy(&stdout_bytes).to_string(),
            String::from_utf8_lossy(&stderr_bytes).to_string(),
            stdout_len,
            stderr_len,
        )
    }

    /// Retained bytes for both streams plus how many leading bytes the memory
    /// bound discarded. Callers that publish these bytes as evidence must
    /// declare the omission rather than presenting a clipped stream as exact.
    fn retained_output_bytes_with_omissions(&self) -> (Vec<u8>, Vec<u8>, usize, usize) {
        if let Some(snapshot) = self.bounded_output_snapshot(false).ok().flatten() {
            let omitted = snapshot.total_bytes.saturating_sub(snapshot.retained_bytes);
            return (snapshot.content.into_bytes(), Vec::new(), omitted, 0);
        }
        let (stdout_bytes, stdout_omitted) = self
            .stdout_buffer
            .lock()
            .map(|data| (data.retained().to_vec(), data.dropped()))
            .unwrap_or_default();
        let (stderr_bytes, stderr_omitted) = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| {
                buffer
                    .lock()
                    .ok()
                    .map(|data| (data.retained().to_vec(), data.dropped()))
            })
            .unwrap_or_default();
        (stdout_bytes, stderr_bytes, stdout_omitted, stderr_omitted)
    }

    fn take_delta(&mut self) -> (String, String, usize, usize, usize, usize) {
        // Only the bytes after the cursor: returning the whole retained tail
        // made every `wait` poll repeat output the caller already holds.
        let bounded_delta = self.bounded_output.as_ref().and_then(|output| {
            let output = output.lock().unwrap_or_else(|error| error.into_inner());
            let total = output.total_bytes();
            output
                .delta_since(self.stdout_cursor)
                .ok()
                .map(|delta| (delta, total))
        });
        if let Some(((mut delta, omitted), total_bytes)) = bounded_delta {
            let delta_len = total_bytes.saturating_sub(self.stdout_cursor);
            self.stdout_cursor = total_bytes;
            if delta_len == 0 {
                return (String::new(), String::new(), 0, 0, total_bytes, 0);
            }
            self.last_output_at = Instant::now();
            self.last_observed_output_len = total_bytes;
            if omitted > 0
                && let Some(output) = self.bounded_output.as_ref()
            {
                let notice = output
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .omitted_notice(omitted);
                delta.insert_str(0, &notice);
            }
            return (delta, String::new(), delta_len, 0, total_bytes, 0);
        }
        let (stdout_delta, stdout_total) =
            take_delta_from_buffer(&self.stdout_buffer, &mut self.stdout_cursor);
        let (stderr_delta, stderr_total) = if let Some(buffer) = self.stderr_buffer.as_ref() {
            take_delta_from_buffer(buffer, &mut self.stderr_cursor)
        } else {
            (Vec::new(), 0)
        };

        let stdout_delta_len = stdout_delta.len();
        let stderr_delta_len = stderr_delta.len();

        if stdout_delta_len > 0 || stderr_delta_len > 0 {
            self.last_output_at = Instant::now();
            self.last_observed_output_len = stdout_total.saturating_add(stderr_total);
        }

        (
            String::from_utf8_lossy(&stdout_delta).to_string(),
            String::from_utf8_lossy(&stderr_delta).to_string(),
            stdout_delta_len,
            stderr_delta_len,
            stdout_total,
            stderr_total,
        )
    }

    fn sandbox_denied(&self) -> bool {
        if matches!(self.status, ShellStatus::Running) {
            return false;
        }
        let (_, stderr_full, _, _) = self.full_output();
        SandboxManager::was_denied(
            self.sandbox_type,
            self.exit_code
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(-1),
            &stderr_full,
        )
    }

    /// Kill the process
    fn kill(&mut self) -> Result<()> {
        if self.poll() && self.status != ShellStatus::Running {
            return Ok(());
        }
        #[cfg(unix)]
        if self.ownership == ShellOwnership::PersistPending
            && let Some(process_group_id) = self.child.as_ref().and_then(ShellChild::process_id)
        {
            unregister_pending_persistent_process_group(process_group_id);
        }
        if let Some(ref mut child) = self.child {
            match child {
                ShellChild::Process(proc) => {
                    #[cfg(windows)]
                    {
                        terminate_windows_job(self.windows_job.as_ref(), proc)
                            .context("Failed to kill process tree")?;
                        let _ = proc.wait();
                    }
                    #[cfg(all(not(windows), unix))]
                    {
                        // Bounded SIGTERM → SIGKILL escalation against the
                        // whole process group; returns within ~grace even if
                        // the command ignores SIGTERM (#52).
                        terminate_child_process_group(proc).context("Failed to kill process")?;
                    }
                    #[cfg(all(not(windows), not(unix)))]
                    {
                        proc.kill().context("Failed to kill process")?;
                        let _ = proc.wait();
                    }
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(child) => {
                    child.kill().context("Failed to kill process")?;
                    let _ = child.wait();
                }
            }
        }
        self.status = ShellStatus::Killed;
        self.mark_finished();
        self.heavy_permit.take();
        self.collect_output();
        self.publish_lifecycle_best_effort();
        Ok(())
    }

    /// Get a snapshot of the current state
    pub fn snapshot(&self) -> Result<ShellResult> {
        let sandboxed = !matches!(self.sandbox_type, SandboxType::None);
        if let Some(snapshot) = self.bounded_output_snapshot(self.status != ShellStatus::Running)? {
            return Ok(ShellResult {
                task_id: Some(self.id.clone()),
                status: self.status.clone(),
                exit_code: self.exit_code,
                stdout: snapshot.content,
                stderr: String::new(),
                duration_ms: self.wall_millis(),
                stdout_len: snapshot.total_bytes,
                stderr_len: 0,
                stdout_omitted: snapshot.total_bytes.saturating_sub(snapshot.retained_bytes),
                stderr_omitted: 0,
                stdout_truncated: snapshot.truncated,
                stderr_truncated: false,
                sandboxed,
                sandbox_type: sandboxed.then(|| self.sandbox_type.to_string()),
                sandbox_denied: false,
            });
        }
        let (stdout_full, stderr_full, stdout_total, stderr_total) = self.full_output();
        let (stdout, stdout_meta) = truncate_with_meta(&stdout_full);
        let (stderr, stderr_meta) = truncate_with_meta(&stderr_full);
        // `truncate_with_meta` can only see the bytes still held. Fold in what
        // the in-memory bound dropped so a >16 MiB stream reports its real
        // length and its real omission instead of silently shrinking (#5472).
        let stdout_dropped = stdout_total.saturating_sub(stdout_meta.original_len);
        let stderr_dropped = stderr_total.saturating_sub(stderr_meta.original_len);
        Ok(ShellResult {
            task_id: Some(self.id.clone()),
            status: self.status.clone(),
            exit_code: self.exit_code,
            stdout,
            stderr,
            duration_ms: self.wall_millis(),
            stdout_len: stdout_total,
            stderr_len: stderr_total,
            stdout_omitted: stdout_meta.omitted.saturating_add(stdout_dropped),
            stderr_omitted: stderr_meta.omitted.saturating_add(stderr_dropped),
            stdout_truncated: stdout_meta.truncated || stdout_dropped > 0,
            stderr_truncated: stderr_meta.truncated || stderr_dropped > 0,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(self.sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: self.sandbox_denied(),
        })
    }

    fn bounded_output_snapshot(&self, finalize: bool) -> Result<Option<BoundedOutputSnapshot>> {
        self.bounded_output
            .as_ref()
            .map(|output| {
                output
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .snapshot(finalize)
                    .map_err(anyhow::Error::from)
            })
            .transpose()
    }

    fn job_snapshot(&self) -> ShellJobSnapshot {
        // Use tail_from_buffer instead of full_output so we never clone the
        // entire accumulated stdout/stderr for display purposes.  full_output
        // is O(total_bytes_written), which caused the ShellManager mutex to be
        // held for an arbitrarily long time during list_jobs() calls from the
        // TUI event loop — freezing input handling on long automation runs.
        let (stdout_len, stdout_tail) =
            if let Some(snapshot) = self.bounded_output_snapshot(false).ok().flatten() {
                (snapshot.total_bytes, tail_text(&snapshot.content, 1_200))
            } else {
                tail_from_buffer(&self.stdout_buffer, 1200)
            };
        let (stderr_len, stderr_tail) = self
            .stderr_buffer
            .as_ref()
            .map(|buf| tail_from_buffer(buf, 1200))
            .unwrap_or((0, String::new()));
        let elapsed_since_output_ms = (self.status == ShellStatus::Running)
            .then(|| u64::try_from(self.last_output_at.elapsed().as_millis()).unwrap_or(u64::MAX));
        ShellJobSnapshot {
            id: self.id.clone(),
            job_id: self.id.clone(),
            command: self.command.clone(),
            cwd: self.working_dir.clone(),
            status: self.status.clone(),
            background: self.background,
            finished_at: self.finished_at_utc,
            exit_code: self.exit_code,
            elapsed_ms: self.wall_millis(),
            stdout_tail,
            stderr_tail,
            stdout_len,
            stderr_len,
            stdin_available: self.stdin.is_some() && self.status == ShellStatus::Running,
            stale: false,
            elapsed_since_output_ms,
            linked_task_id: self.linked_task_id.clone(),
            owner_agent_id: self
                .owner_agent
                .as_ref()
                .map(|owner| owner.agent_id.clone()),
            owner_agent_name: self
                .owner_agent
                .as_ref()
                .map(|owner| owner.agent_name.clone()),
            origin_tool_call_id: self.origin_tool_call_id.clone(),
            origin_turn_id: self.origin_turn_id.clone(),
            owner_session_id: self.owner_session_id.clone(),
        }
    }

    fn completion_event(&self) -> ShellCompletionEvent {
        let snapshot = self.job_snapshot();
        let (stdout_len, stdout_tail) =
            if let Some(output) = self.bounded_output_snapshot(false).ok().flatten() {
                (
                    output.total_bytes,
                    tail_text(&output.content, SHELL_COMPLETION_TAIL_BYTES),
                )
            } else {
                bounded_completion_tail(&self.stdout_buffer, SHELL_COMPLETION_TAIL_BYTES)
            };
        let (stderr_len, stderr_tail) = self
            .stderr_buffer
            .as_ref()
            .map(|buffer| bounded_completion_tail(buffer, SHELL_COMPLETION_TAIL_BYTES))
            .unwrap_or((0, String::new()));
        ShellCompletionEvent {
            task_id: snapshot.id,
            command: snapshot.command,
            status: snapshot.status,
            exit_code: snapshot.exit_code,
            duration_ms: snapshot.elapsed_ms,
            stdout_tail,
            stderr_tail,
            stdout_len,
            stderr_len,
            evidence_ref: None,
            linked_task_id: snapshot.linked_task_id,
            owner_agent_id: snapshot.owner_agent_id,
            owner_agent_name: snapshot.owner_agent_name,
            origin_tool_call_id: snapshot.origin_tool_call_id,
            origin_turn_id: snapshot.origin_turn_id,
            owner_session_id: snapshot.owner_session_id,
        }
    }

    fn completion_evidence(&self) -> ShellCompletionEvidence {
        let event = self.completion_event();
        let (stdout, stderr, stdout_omitted, stderr_omitted) =
            self.retained_output_bytes_with_omissions();
        ShellCompletionEvidence {
            event,
            stdout,
            stderr,
            stdout_omitted,
            stderr_omitted,
        }
    }

    fn job_detail(&self) -> ShellJobDetail {
        let (stdout, stderr, _, _) = self.full_output();
        ShellJobDetail {
            snapshot: self.job_snapshot(),
            stdout,
            stderr,
        }
    }
}

fn finish_background_reader(
    handle: std::thread::JoinHandle<()>,
    status: &ShellStatus,
    buffer: Option<&SharedRawOutput>,
) {
    // A killed Windows process can leave a pipe reader blocked even after its
    // Job Object has been closed. Cancellation must return promptly instead of
    // waiting for that reader to observe EOF. Other terminal states still join
    // so their final output is collected before the shell is discarded.
    #[cfg(windows)]
    if *status == ShellStatus::Killed {
        drop(handle);
        return;
    }

    #[cfg(not(windows))]
    let _ = status;

    // Bounded join (#52): after the process group is killed the reader
    // normally sees EOF immediately, but a descendant that escaped the group
    // (its own session/process group) keeps its inherited pipe write-end
    // open, so the reader stays blocked until that descendant exits on its
    // own. Joining unboundedly froze the foreground shell — and, through the
    // shell-manager lock, every other shell — for minutes. On timeout the
    // join is handed to a helper thread and we return; the reader thread
    // still finishes on its own once the pipe finally closes.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
    });
    if done_rx.recv_timeout(READER_JOIN_GRACE).is_ok() {
        return;
    }
    // The reader is still blocked in `read()` on a pipe a descendant refuses to
    // close. Previously both it and the helper thread above stayed alive for the
    // life of the process, the reader still appending into a buffer nobody would
    // ever read (#5472 finding 2). Abandoning the stream releases what it holds
    // and gives the reader an exit on its next wakeup, which also lets the
    // helper's `join` return.
    if let Some(buffer) = buffer {
        buffer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .abandon();
    }
}

impl Drop for BackgroundShell {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.ownership == ShellOwnership::PersistPending
            && let Some(process_group_id) = self.child.as_ref().and_then(ShellChild::process_id)
        {
            unregister_pending_persistent_process_group(process_group_id);
        }
        if self.ownership != ShellOwnership::Released
            && self.status == ShellStatus::Running
            && let Some(ref mut child) = self.child
        {
            #[cfg(windows)]
            match child {
                ShellChild::Process(proc) => {
                    let _ = terminate_windows_job(self.windows_job.as_ref(), proc);
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(child) => {
                    let _ = child.kill();
                }
            }
            #[cfg(all(not(windows), unix))]
            {
                let _ = child.kill();
                match child {
                    ShellChild::Process(proc) => {
                        let _ = wait_child_bounded(proc, KILL_REAP_GRACE);
                    }
                    #[cfg(not(target_env = "ohos"))]
                    ShellChild::Pty(child) => {
                        let _ = child.wait();
                    }
                }
            }
            #[cfg(all(not(windows), not(unix)))]
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[cfg(all(unix, not(target_env = "ohos")))]
pub fn inherited_interactive_terminal_refusal() -> Option<&'static str> {
    Some(
        "Inherited interactive terminal takeover is unavailable on Unix because foreground TTY \
         ownership cannot be transferred safely. Use Bash with `background: true, tty: true`, \
         then continue it with `action: \"interact\"` and the returned `task_id`; alternatively \
         use `terminal/run` and `terminal/send`, or launch the command in a new terminal. For \
         non-interactive work, omit `interactive: true`.",
    )
}

#[cfg(all(unix, target_env = "ohos"))]
pub fn inherited_interactive_terminal_refusal() -> Option<&'static str> {
    Some(
        "Inherited interactive terminal takeover is unavailable on Unix because foreground TTY \
         ownership cannot be transferred safely. Launch the command in a new terminal, or omit \
         `interactive: true` for non-interactive work.",
    )
}

#[cfg(not(unix))]
pub fn inherited_interactive_terminal_refusal() -> Option<&'static str> {
    None
}

/// Manages background shell processes with optional sandboxing.
pub struct ShellManager {
    processes: HashMap<String, BackgroundShell>,
    stale_jobs: HashMap<String, ShellJobSnapshot>,
    default_workspace: PathBuf,
    sandbox_manager: SandboxManager,
    sandbox_policy: ExecutionSandboxPolicy,
    /// Only live wait futures hold strong references. A request cannot survive
    /// completion/cancellation and release a later wait or another session.
    active_waits: Vec<(String, Weak<AtomicBool>)>,
    /// Directory for lowercase-`bash` complete-output spill files
    /// (`None` = process temp dir). Overridable so tests can fault-inject a
    /// missing/unwritable spill location.
    output_spill_dir: Option<PathBuf>,
}

impl std::fmt::Debug for ShellManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellManager")
            .field("processes", &self.processes.len())
            .field("stale_jobs", &self.stale_jobs.len())
            .field("default_workspace", &self.default_workspace)
            .field("sandbox_policy", &self.sandbox_policy)
            .field(
                "active_waits",
                &self
                    .active_waits
                    .iter()
                    .filter(|(_, wait)| wait.strong_count() > 0)
                    .count(),
            )
            .finish()
    }
}

impl ShellManager {
    fn require_session_owner(&self, task_id: &str, active_session_id: &str) -> Result<()> {
        let owned = self.processes.get(task_id).is_some_and(|shell| {
            !active_session_id.is_empty() && shell.owner_session_id == active_session_id
        }) || self.stale_jobs.get(task_id).is_some_and(|job| {
            !active_session_id.is_empty() && job.owner_session_id == active_session_id
        });
        if owned {
            Ok(())
        } else {
            // Do not disclose whether the id exists in another session.
            Err(anyhow!("Job {task_id} not found"))
        }
    }

    /// Create a new `ShellManager` with default (no sandbox) policy.
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            processes: HashMap::new(),
            stale_jobs: HashMap::new(),
            default_workspace: workspace,
            sandbox_manager: SandboxManager::new(),
            sandbox_policy: ExecutionSandboxPolicy::default(),
            active_waits: Vec::new(),
            output_spill_dir: None,
        }
    }

    /// Point lowercase-`bash` complete-output spill files at `dir` instead of
    /// the process temp dir. Tests use a nonexistent dir to simulate a full or
    /// broken temp volume. (unix-only: the regression test that uses it drives
    /// a POSIX shell loop.)
    #[cfg(all(test, unix))]
    pub fn set_output_spill_dir_for_test(&mut self, dir: Option<PathBuf>) {
        self.output_spill_dir = dir;
    }

    /// Insert a finished job without spawning. Count-bound tests would
    /// otherwise pay for 100+ live shells.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_finished_record_for_test(&mut self, id: impl Into<String>, age: Duration) {
        let now = Instant::now();
        let started_at = now.checked_sub(age).unwrap_or(now);
        let id = id.into();
        self.processes.insert(
            id.clone(),
            BackgroundShell {
                id,
                command: String::new(),
                working_dir: self.default_workspace.clone(),
                status: ShellStatus::Completed,
                background: true,
                exit_code: Some(0),
                started_at,
                finished_at: Some(now),
                finished_at_utc: Some(chrono::Utc::now()),
                last_output_at: now,
                last_observed_output_len: 0,
                sandbox_type: SandboxType::None,
                linked_task_id: None,
                owner_agent: None,
                owner_session_id: String::new(),
                origin_tool_call_id: None,
                origin_turn_id: None,
                ownership: ShellOwnership::Managed,
                stdout_buffer: new_shared_raw_output(),
                stderr_buffer: Some(new_shared_raw_output()),
                bounded_output: None,
                heavy_permit: None,
                stdout_cursor: 0,
                stderr_cursor: 0,
                completion_reported: false,
                stdin: None,
                #[cfg(not(target_env = "ohos"))]
                pty_master: None,
                terminal_size: None,
                child: None,
                #[cfg(windows)]
                windows_job: None,
                #[cfg(unix)]
                parent_death_watch: None,
                stdout_thread: None,
                stderr_thread: None,
                work_lifecycle: None,
                lifecycle_seq: 0,
                last_lifecycle_status: None,
                last_lifecycle_bytes: 0,
                wait_failed: false,
            },
        );
    }

    /// Test-only observation of the workspace selected by runtime rebuilds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn default_workspace(&self) -> &Path {
        &self.default_workspace
    }

    /// Enable or disable bubblewrap passthrough (#2184).
    ///
    /// When enabled and `/usr/bin/bwrap` is executable on Linux, exec_shell
    /// commands are routed through bubblewrap for filesystem isolation.
    pub fn set_prefer_bwrap(&mut self, prefer: bool) {
        self.sandbox_manager.set_prefer_bwrap(prefer);
    }

    /// Move the fallback working directory. Callers that pass an explicit
    /// `working_dir` are unaffected; this only keeps `None` honest when a
    /// thread's workspace changes while its jobs are still tracked here.
    pub fn set_default_workspace(&mut self, workspace: PathBuf) {
        self.default_workspace = workspace;
    }

    /// Set user-configured bwrap mount extensions (#5410): extra read-only
    /// roots and writable device nodes such as `/dev/null`.
    pub fn set_bwrap_extensions(&mut self, extensions: codewhale_sandbox::BwrapMountExtensions) {
        self.sandbox_manager.set_bwrap_extensions(extensions);
    }

    /// Forward the opt-in sandbox read deny-list (S1, #5568) to the sandbox
    /// manager; `~` prefixes expand there.
    pub fn set_denied_read_subpaths(&mut self, paths: Vec<std::path::PathBuf>) {
        self.sandbox_manager.set_denied_read_subpaths(paths);
    }

    /// Return the OS sandbox wrapper this shell manager is configured and able
    /// to apply to commands.
    pub fn configured_sandbox_type(&self) -> Option<SandboxType> {
        self.sandbox_manager.configured_sandbox()
    }

    /// Prepare a program for a tool that runs workspace code outside
    /// `exec_shell`, under the policy and sandbox configuration a shell
    /// command would get in this session.
    pub fn prepare_runner(
        &self,
        program: &str,
        args: Vec<String>,
        cwd: &Path,
        timeout: Duration,
        policy_override: Option<ExecutionSandboxPolicy>,
    ) -> ExecEnv {
        let policy = policy_override.unwrap_or_else(|| self.sandbox_policy.clone());
        let spec =
            CommandSpec::program(program, args, cwd.to_path_buf(), timeout).with_policy(policy);
        self.sandbox_manager.prepare(&spec)
    }

    fn register_shell_wait(&mut self, session_id: &str) -> Arc<AtomicBool> {
        self.active_waits
            .retain(|(_, wait)| wait.strong_count() > 0);
        let detach = Arc::new(AtomicBool::new(false));
        self.active_waits
            .push((session_id.to_string(), Arc::downgrade(&detach)));
        detach
    }

    /// Release the owning session's currently active shell waits without
    /// stopping their processes. This never queues a request for future work.
    pub fn request_shell_wait_detach(&mut self, session_id: &str) -> bool {
        let mut requested = false;
        self.active_waits.retain(|(owner, wait)| {
            let Some(wait) = wait.upgrade() else {
                return false;
            };
            if !session_id.is_empty() && owner == session_id {
                wait.store(true, Ordering::Release);
                requested = true;
            }
            true
        });
        requested
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn shell_wait_active_for_test(&self, session_id: &str) -> bool {
        self.active_waits
            .iter()
            .any(|(owner, wait)| owner == session_id && wait.strong_count() > 0)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn register_shell_wait_for_test(&mut self, session_id: &str) -> Arc<AtomicBool> {
        self.register_shell_wait(session_id)
    }

    /// Execute a shell command with stdin/TTY options plus an extra env-var map
    /// that is merged into the spawned process environment. Used by the
    /// `shell_env` hook injection path (#456).
    #[allow(clippy::too_many_arguments)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn execute_with_options_env(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> Result<ShellResult> {
        self.execute_with_options_env_for_owner(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            extra_env,
            None,
        )
    }

    /// Launch a parent-owned job stamped to the immutable root session.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_options_env_for_session(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
        owner_session_id: &str,
    ) -> Result<ShellResult> {
        self.execute_with_options_env_for_owner_and_work(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            extra_env,
            None,
            owner_session_id.to_string(),
            None,
            None,
            None,
            None,
            false,
            (1_000, 600_000),
        )
    }

    /// Same as `execute_with_options_env`, with optional background-job owner
    /// attribution for sub-agent launched jobs.
    #[allow(clippy::too_many_arguments)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn execute_with_options_env_for_owner(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
        owner_agent: Option<ShellJobOwner>,
    ) -> Result<ShellResult> {
        self.execute_with_options_env_for_owner_and_work(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            extra_env,
            owner_agent,
            String::new(),
            None,
            None,
            None,
            None,
            false,
            (1_000, 600_000),
        )
    }

    /// Test-only owner-aware launch with an explicit immutable session owner.
    #[allow(clippy::too_many_arguments)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn execute_with_options_env_for_owner_and_session(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
        owner_agent: Option<ShellJobOwner>,
        owner_session_id: &str,
    ) -> Result<ShellResult> {
        self.execute_with_options_env_for_owner_and_work(
            command,
            working_dir,
            timeout_ms,
            background,
            stdin_data,
            tty,
            policy_override,
            extra_env,
            owner_agent,
            owner_session_id.to_string(),
            None,
            None,
            None,
            None,
            false,
            (1_000, 600_000),
        )
    }

    /// Owner-aware execution with an optional Work Graph lifecycle sink.
    #[allow(clippy::too_many_arguments)]
    fn execute_with_options_env_for_owner_and_work(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        background: bool,
        stdin_data: Option<&str>,
        tty: bool,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
        owner_agent: Option<ShellJobOwner>,
        owner_session_id: String,
        origin_tool_call_id: Option<String>,
        origin_turn_id: Option<String>,
        work_lifecycle: Option<ShellWorkLifecycle>,
        readonly_workspace: Option<&std::path::Path>,
        persist_pending: bool,
        timeout_bounds_ms: (u64, u64),
    ) -> Result<ShellResult> {
        // Log execution via ShellDispatcher when SHELL_DISPATCHER_LOG is set.
        codewhale_sandbox::shell_dispatcher::ShellDispatcher::log_exec(command);

        let work_dir = working_dir.map_or_else(|| self.default_workspace.clone(), PathBuf::from);
        validate_shell_working_dir(&work_dir, working_dir.is_none())?;

        let timeout_ms = timeout_ms.clamp(timeout_bounds_ms.0, timeout_bounds_ms.1);

        // Use override policy if provided, otherwise use the manager's policy
        let policy = policy_override.unwrap_or_else(|| self.sandbox_policy.clone());

        // Create command spec and prepare sandboxed environment
        let spec = if let Some(workspace) = readonly_workspace {
            if readonly_command_needs_shell(command) {
                let script = hardened_readonly_script(command, workspace)?;
                CommandSpec::shell(&script, work_dir.clone(), Duration::from_millis(timeout_ms))
            } else {
                let (program, args) = hardened_readonly_argv(command)?;
                let program = resolve_readonly_program(&program, workspace)?;
                CommandSpec::program(
                    program
                        .to_str()
                        .ok_or_else(|| anyhow!("read-only executable path is not valid UTF-8"))?,
                    args,
                    work_dir.clone(),
                    Duration::from_millis(timeout_ms),
                )
            }
        } else {
            CommandSpec::shell(command, work_dir.clone(), Duration::from_millis(timeout_ms))
        };
        let spec = spec.with_policy(policy).with_env(extra_env);
        let exec_env = self.sandbox_manager.prepare(&spec);
        if matches!(spec.sandbox_policy, ExecutionSandboxPolicy::ReadOnly { .. })
            && readonly_workspace.is_none()
        {
            // Arbitrary code with a read-only policy needs kernel enforcement;
            // only the separately hardened argv subset may run without it.
            require_native_readonly_execution(&exec_env)?;
        }

        if background {
            let bounded_output = timeout_bounds_ms == (1, BASH_MAX_TIMEOUT_MS);
            self.spawn_background_sandboxed(
                command,
                &work_dir,
                &exec_env,
                None,
                stdin_data,
                tty,
                ShellSpawnContext {
                    owner_agent,
                    owner_session_id,
                    origin_tool_call_id,
                    origin_turn_id,
                    work_lifecycle,
                },
                persist_pending,
                bounded_output,
            )
        } else {
            if tty {
                return Err(anyhow!(
                    "TTY mode requires background execution (set background: true)."
                ));
            }
            Self::execute_sync_sandboxed(command, &work_dir, timeout_ms, stdin_data, &exec_env)
        }
    }

    /// Interactive variant that accepts extra env vars (#456 shell_env hook).
    pub fn execute_interactive_with_policy_env(
        &mut self,
        command: &str,
        working_dir: Option<&str>,
        timeout_ms: u64,
        policy_override: Option<ExecutionSandboxPolicy>,
        extra_env: HashMap<String, String>,
    ) -> Result<ShellResult> {
        codewhale_sandbox::shell_dispatcher::ShellDispatcher::log_exec(command);

        // A new Unix process group that inherits the terminal is not its
        // foreground owner. Letting it read stdin triggers SIGTTIN; sharing
        // Codewhale's group instead would make cooked-mode Ctrl+C terminate
        // both parent and child. Until this path owns a complete POSIX job-
        // control lease, fail closed before spawning. Persistent PTY tools
        // already provide a safe interactive lane without taking over the
        // operator's live terminal.
        if let Some(message) = inherited_interactive_terminal_refusal() {
            return Err(anyhow!(message));
        }

        let work_dir = working_dir.map_or_else(|| self.default_workspace.clone(), PathBuf::from);
        validate_shell_working_dir(&work_dir, working_dir.is_none())?;

        let timeout_ms = timeout_ms.clamp(1000, 600_000);
        let policy = policy_override.unwrap_or_else(|| self.sandbox_policy.clone());

        let spec = CommandSpec::shell(command, work_dir.clone(), Duration::from_millis(timeout_ms))
            .with_policy(policy)
            .with_env(extra_env);
        let exec_env = self.sandbox_manager.prepare(&spec);
        if matches!(spec.sandbox_policy, ExecutionSandboxPolicy::ReadOnly { .. }) {
            require_native_readonly_execution(&exec_env)?;
        }

        Self::execute_interactive_sandboxed(command, &work_dir, timeout_ms, &exec_env)
    }

    /// Execute command synchronously with timeout (sandboxed).
    fn execute_sync_sandboxed(
        original_command: &str,
        working_dir: &std::path::Path,
        timeout_ms: u64,
        stdin_data: Option<&str>,
        exec_env: &ExecEnv,
    ) -> Result<ShellResult> {
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = exec_env.is_sandboxed();

        // Build the command from ExecEnv
        let program = exec_env.program();
        let args = exec_env.args();

        let mut cmd = Command::new(program);
        codewhale_util::utils::suppress_console_window(&mut cmd);
        push_shell_args(&mut cmd, program, args);
        cmd.current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        install_parent_death_signal(&mut cmd);

        // Without input, stdin is closed rather than inherited: an unexpected
        // read (`cat`, a prompt) gets EOF instead of blocking on, or reading,
        // the operator's terminal until the timeout.
        cmd.stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));
        remove_readonly_redirect_env(&mut cmd, &exec_env.env);

        // Leave raw mode before spawn; the guard restores it only if raw mode
        // was active on entry (issue #1690).
        let _raw_mode = codewhale_runtime::host_terminal::suspend_raw_mode();

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to execute: {original_command}"))?;
        #[cfg(windows)]
        let windows_job = attach_windows_job(&child, original_command);

        if let Some(input) = stdin_data
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin
                .write_all(input.as_bytes())
                .context("Failed to write to stdin")?;
            stdin.flush().ok();
        }

        let stdout_handle = child.stdout.take().context("Failed to capture stdout")?;
        let stderr_handle = child.stderr.take().context("Failed to capture stderr")?;

        // Spawn threads to read output. Use bounded receives below so a killed
        // or detached descendant that keeps pipe handles open cannot wedge the
        // foreground shell path while the global tool lock is held (#2571).
        let stdout_rx = spawn_sync_reader_thread(stdout_handle);
        let stderr_rx = spawn_sync_reader_thread(stderr_handle);

        // Wait with timeout
        if let Some(status) = child.wait_timeout(timeout)? {
            let status = ShellExitStatus::from_std(status);
            #[cfg(unix)]
            let _ = kill_child_process_group(&mut child);
            #[cfg(windows)]
            terminate_and_close_windows_job(windows_job);
            let stdout = recv_sync_reader_output(&stdout_rx);
            let stderr = recv_sync_reader_output(&stderr_rx);
            let stdout_str = String::from_utf8_lossy(&stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&stderr).to_string();
            let exit_code = status
                .code
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(-1);

            // Check if sandbox denied the operation
            let sandbox_denied = SandboxManager::was_denied(sandbox_type, exit_code, &stderr_str);
            let (stdout, stdout_meta) = truncate_with_meta(&stdout_str);
            let (stderr, stderr_meta) = truncate_with_meta(&stderr_str);

            Ok(ShellResult {
                task_id: None,
                status: if status.success {
                    ShellStatus::Completed
                } else {
                    ShellStatus::Failed
                },
                exit_code: status.code,
                stdout,
                stderr,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: stdout_meta.original_len,
                stderr_len: stderr_meta.original_len,
                stdout_omitted: stdout_meta.omitted,
                stderr_omitted: stderr_meta.omitted,
                stdout_truncated: stdout_meta.truncated,
                stderr_truncated: stderr_meta.truncated,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied,
            })
        } else {
            // Timeout - kill the process
            #[cfg(unix)]
            let _ = kill_child_process_group(&mut child);
            #[cfg(windows)]
            let _ = terminate_child_and_close_windows_job(windows_job, &mut child);
            #[cfg(all(not(unix), not(windows)))]
            let _ = child.kill();
            let status = child.wait().ok();
            let stdout = recv_sync_reader_output(&stdout_rx);
            let stderr = recv_sync_reader_output(&stderr_rx);
            let stdout_str = String::from_utf8_lossy(&stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&stderr).to_string();
            let (stdout, stdout_meta) = truncate_with_meta(&stdout_str);
            let (stderr, stderr_meta) = truncate_with_meta(&stderr_str);

            Ok(ShellResult {
                task_id: None,
                status: ShellStatus::TimedOut,
                exit_code: status
                    .map(ShellExitStatus::from_std)
                    .and_then(|status| status.code),
                stdout,
                stderr,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: stdout_meta.original_len,
                stderr_len: stderr_meta.original_len,
                stdout_omitted: stdout_meta.omitted,
                stderr_omitted: stderr_meta.omitted,
                stdout_truncated: stdout_meta.truncated,
                stderr_truncated: stderr_meta.truncated,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
            })
        }
    }

    /// Execute command interactively with timeout (sandboxed).
    fn execute_interactive_sandboxed(
        original_command: &str,
        working_dir: &std::path::Path,
        timeout_ms: u64,
        exec_env: &ExecEnv,
    ) -> Result<ShellResult> {
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = exec_env.is_sandboxed();

        let program = exec_env.program();
        let args = exec_env.args();

        let mut cmd = Command::new(program);
        codewhale_util::utils::suppress_console_window(&mut cmd);
        push_shell_args(&mut cmd, program, args);
        cmd.current_dir(working_dir)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        install_parent_death_signal(&mut cmd);

        // Leave raw mode before spawn; the guard restores it only if raw mode
        // was active on entry (issue #1690).
        let _raw_mode = codewhale_runtime::host_terminal::suspend_raw_mode();

        child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to execute: {original_command}"))?;
        #[cfg(windows)]
        let windows_job = attach_windows_job(&child, original_command);

        if let Some(status) = child.wait_timeout(timeout)? {
            let status = ShellExitStatus::from_std(status);
            #[cfg(windows)]
            terminate_and_close_windows_job(windows_job);
            Ok(ShellResult {
                task_id: None,
                status: if status.success {
                    ShellStatus::Completed
                } else {
                    ShellStatus::Failed
                },
                exit_code: status.code,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: 0,
                stderr_len: 0,
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
            })
        } else {
            #[cfg(unix)]
            let _ = kill_child_process_group(&mut child);
            #[cfg(windows)]
            let _ = terminate_child_and_close_windows_job(windows_job, &mut child);
            #[cfg(all(not(unix), not(windows)))]
            let _ = child.kill();
            let status = child.wait().ok();

            Ok(ShellResult {
                task_id: None,
                status: ShellStatus::TimedOut,
                exit_code: status
                    .map(ShellExitStatus::from_std)
                    .and_then(|status| status.code),
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                stdout_len: 0,
                stderr_len: 0,
                stdout_omitted: 0,
                stderr_omitted: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                sandboxed,
                sandbox_type: if sandboxed {
                    Some(sandbox_type.to_string())
                } else {
                    None
                },
                sandbox_denied: false,
            })
        }
    }

    /// Spawn a background process (sandboxed).
    #[allow(clippy::too_many_arguments)]
    fn spawn_background_sandboxed(
        &mut self,
        original_command: &str,
        working_dir: &std::path::Path,
        exec_env: &ExecEnv,
        heavy_permit: Option<HeavyCommandPermit>,
        stdin_data: Option<&str>,
        tty: bool,
        spawn_context: ShellSpawnContext,
        persist_pending: bool,
        small_contract_mode: bool,
    ) -> Result<ShellResult> {
        let ShellSpawnContext {
            owner_agent,
            owner_session_id,
            origin_tool_call_id,
            origin_turn_id,
            work_lifecycle,
        } = spawn_context;
        let task_id = format!("shell_{}", &Uuid::new_v4().to_string()[..8]);
        let mut spawn_guard =
            ShellSpawnIntentGuard::new(work_lifecycle, &task_id, original_command);
        // The guard owns the registration outcome: when bookkeeping could not
        // bind this operation, nothing below may publish to it (#6435).
        let work_lifecycle = spawn_guard.lifecycle.clone();
        let started = Instant::now();
        let sandbox_type = exec_env.sandbox_type;
        let sandboxed = exec_env.is_sandboxed();

        // Build the command from ExecEnv
        let program = exec_env.program();
        let args = exec_env.args();

        #[cfg(target_env = "ohos")]
        if tty {
            return Err(anyhow!(
                "TTY shell mode is not supported on HarmonyOS/OpenHarmony yet."
            ));
        }

        let stdout_buffer = new_shared_raw_output();
        let stderr_buffer = if tty || persist_pending || small_contract_mode {
            None
        } else {
            Some(new_shared_raw_output())
        };
        // The spill file is best-effort: a full disk or exhausted descriptor
        // table must not make `echo ok` unrunnable (that is exactly how the
        // owner's session got wedged under swap exhaustion).
        let bounded_output = small_contract_mode.then(|| {
            Arc::new(Mutex::new(BoundedOutputAccumulator::new_in(
                self.output_spill_dir.as_deref(),
            )))
        });

        #[cfg(windows)]
        let mut windows_job = None;
        #[cfg(unix)]
        let mut parent_death_watch = None;

        #[cfg(not(target_env = "ohos"))]
        let mut pty_master = None;
        let (child, stdin, stdout_thread, stderr_thread) = if tty {
            #[cfg(target_env = "ohos")]
            unreachable!("OHOS TTY mode returns before PTY setup");

            #[cfg(not(target_env = "ohos"))]
            {
                let pty_system = native_pty_system();
                let pair = pty_system
                    .openpty(PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("Failed to open PTY")?;

                let mut child = exec_env
                    .spawn_pty(pair.slave, working_dir)
                    .with_context(|| format!("Failed to spawn PTY command: {original_command}"))?;

                let reader = match pair.master.try_clone_reader() {
                    Ok(reader) => reader,
                    Err(err) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(err).context("Failed to clone PTY reader");
                    }
                };
                let writer = match pair.master.take_writer() {
                    Ok(writer) => writer,
                    Err(err) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(err).context("Failed to take PTY writer");
                    }
                };
                let stdout_thread = Some(spawn_reader_thread(reader, Arc::clone(&stdout_buffer)));
                pty_master = Some(pair.master);

                (
                    ShellChild::Pty(child),
                    Some(StdinWriter::Pty(writer)),
                    stdout_thread,
                    None,
                )
            }
        } else if persist_pending {
            let mut cmd = Command::new(program);
            codewhale_util::utils::suppress_console_window(&mut cmd);
            push_shell_args(&mut cmd, program, args);
            cmd.current_dir(working_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                cmd.process_group(0);
            }
            // Deliberately no parent-death cleanup: a staged service can be
            // handed to the user (`ShellOwnership::Released`) and must then
            // outlive the TUI (#6654).

            child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));
            remove_readonly_redirect_env(&mut cmd, &exec_env.env);

            let child = exec_env.spawn_background(cmd).with_context(|| {
                format!("Failed to spawn persistent service: {original_command}")
            })?;
            (ShellChild::Process(child), None, None, None)
        } else {
            let mut cmd = Command::new(program);
            codewhale_util::utils::suppress_console_window(&mut cmd);
            push_shell_args(&mut cmd, program, args);
            cmd.current_dir(working_dir).stdin(Stdio::piped());
            let combined_reader = if small_contract_mode {
                let (reader, stdout, stderr) =
                    shared_output_pipe().context("Failed to create combined shell output pipe")?;
                cmd.stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));
                Some(reader)
            } else {
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                None
            };
            #[cfg(unix)]
            {
                cmd.process_group(0);
            }

            child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));
            remove_readonly_redirect_env(&mut cmd, &exec_env.env);

            let mut child = exec_env
                .spawn_background(cmd)
                .with_context(|| format!("Failed to spawn background: {original_command}"))?;
            // Managed children die with the TUI (#6654); unlike the
            // persistent branch above, nobody can take ownership of them.
            #[cfg(unix)]
            {
                parent_death_watch = watch_process_group_for_parent_death(child.id())
                    .inspect_err(|err| {
                        tracing::warn!(
                            "background shell {task_id} has no parent-death cleanup: {err}"
                        );
                    })
                    .ok();
            }
            #[cfg(windows)]
            {
                windows_job = attach_windows_job(&child, original_command);
            }

            let stdin_handle = child.stdin.take().map(StdinWriter::Pipe);

            let (stdout_thread, stderr_thread) =
                if let (Some(reader), Some(output)) = (combined_reader, bounded_output.as_ref()) {
                    (
                        Some(spawn_bounded_reader_thread(reader, Arc::clone(output))),
                        None,
                    )
                } else {
                    let stdout_handle = child.stdout.take().ok_or_else(|| {
                        #[cfg(windows)]
                        terminate_unregistered_process(&mut child, windows_job.as_ref());
                        #[cfg(not(windows))]
                        terminate_unregistered_process(&mut child);
                        anyhow!("Failed to capture stdout")
                    })?;
                    let stderr_handle = child.stderr.take().ok_or_else(|| {
                        #[cfg(windows)]
                        terminate_unregistered_process(&mut child, windows_job.as_ref());
                        #[cfg(not(windows))]
                        terminate_unregistered_process(&mut child);
                        anyhow!("Failed to capture stderr")
                    })?;
                    (
                        Some(spawn_reader_thread(
                            stdout_handle,
                            Arc::clone(&stdout_buffer),
                        )),
                        stderr_buffer
                            .as_ref()
                            .map(|buffer| spawn_reader_thread(stderr_handle, Arc::clone(buffer))),
                    )
                };

            (
                ShellChild::Process(child),
                stdin_handle,
                stdout_thread,
                stderr_thread,
            )
        };

        let mut bg_shell = BackgroundShell {
            id: task_id.clone(),
            command: original_command.to_string(),
            working_dir: working_dir.to_path_buf(),
            status: ShellStatus::Running,
            background: true,
            exit_code: None,
            started_at: started,
            finished_at: None,
            finished_at_utc: None,
            last_output_at: started,
            last_observed_output_len: 0,
            sandbox_type,
            linked_task_id: None,
            owner_agent,
            owner_session_id,
            origin_tool_call_id,
            origin_turn_id,
            ownership: if persist_pending {
                ShellOwnership::PersistPending
            } else {
                ShellOwnership::Managed
            },
            stdout_buffer,
            stderr_buffer,
            bounded_output,
            heavy_permit,
            stdout_cursor: 0,
            stderr_cursor: 0,
            completion_reported: false,
            stdin,
            #[cfg(not(target_env = "ohos"))]
            pty_master,
            terminal_size: tty.then(PtyDimensions::default),
            child: Some(child),
            #[cfg(windows)]
            windows_job,
            #[cfg(unix)]
            parent_death_watch,
            stdout_thread,
            stderr_thread,
            work_lifecycle,
            lifecycle_seq: 0,
            last_lifecycle_status: None,
            last_lifecycle_bytes: 0,
            wait_failed: false,
        };

        #[cfg(unix)]
        if persist_pending {
            let process_group_id = bg_shell
                .child
                .as_ref()
                .and_then(ShellChild::process_id)
                .ok_or_else(|| anyhow!("Persistent service has no process group id"))?;
            register_pending_persistent_process_group(process_group_id);
        }

        if let Some(input) = stdin_data
            && let Err(err) = bg_shell.write_stdin(input, false)
        {
            let _ = bg_shell.kill();
            return Err(err);
        }

        // Work-graph publication is bookkeeping: a graph write that fails
        // now must not kill a command that already started (#6435).
        bg_shell.publish_lifecycle_best_effort();

        self.processes.insert(task_id.clone(), bg_shell);
        spawn_guard.disarm();
        // Evict here, not only from `list_jobs()`: retention must not depend on
        // the user opening the jobs panel, and every spawn is exactly the moment
        // the previous calls' records became one call staler (#5472).
        self.cleanup(FINISHED_SHELL_MAX_AGE);

        Ok(ShellResult {
            task_id: Some(task_id),
            status: ShellStatus::Running,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            stdout_len: 0,
            stderr_len: 0,
            stdout_omitted: 0,
            stderr_omitted: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: false,
        })
    }

    /// Get output from a background process
    pub fn get_output(
        &mut self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<ShellResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;

        if block && shell.status == ShellStatus::Running {
            let timeout = Duration::from_millis(timeout_ms.clamp(1000, 600_000));
            let deadline = Instant::now() + timeout;

            while shell.status == ShellStatus::Running && Instant::now() < deadline {
                if shell.poll() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            // If still running after timeout
            if shell.status == ShellStatus::Running {
                return shell.snapshot();
            }
        } else {
            shell.poll();
        }

        shell.snapshot()
    }

    /// Poll a job and return only its status.
    ///
    /// The foreground wait loop ticks every 100 ms and discards the snapshot
    /// unless the job is terminal, but `get_output` → `snapshot` clones both
    /// raw buffers to build it. On a command printing 50 MB that was ~1.5 GB of
    /// allocate-and-drop churn per wait, all of it thrown away (#5472 finding 1,
    /// the transient term). Status is what the loop actually needs.
    fn poll_status(&mut self, task_id: &str) -> Result<ShellStatus> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.poll();
        Ok(shell.status.clone())
    }

    /// Write data to stdin of a background process.
    pub fn write_stdin(&mut self, task_id: &str, input: &str, close: bool) -> Result<()> {
        self.write_stdin_bytes(task_id, input.as_bytes(), close)
    }

    /// Exact input bytes from an authenticated client; never round-trip through UTF-8.
    pub fn write_stdin_bytes(&mut self, task_id: &str, input: &[u8], close: bool) -> Result<()> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.write_stdin_bytes(input, close)?;
        Ok(())
    }

    /// Historical evicted records have no terminal-size evidence.
    pub fn job_terminal_size(&self, task_id: &str) -> Option<PtyDimensions> {
        self.processes
            .get(task_id)
            .and_then(|shell| shell.terminal_size)
    }

    pub fn resize_pty(&mut self, task_id: &str, size: PtyDimensions) -> Result<()> {
        let size = size.validate()?;
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Job {task_id} not found"))?;
        shell.poll();
        anyhow::ensure!(
            shell.status == ShellStatus::Running,
            "PTY job is no longer running"
        );
        #[cfg(not(target_env = "ohos"))]
        {
            let master = shell.pty_master.as_ref().context("Job is not a PTY")?;
            master
                .resize(PtySize {
                    rows: size.rows,
                    cols: size.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("Failed to resize PTY")?;
            shell.terminal_size = Some(size);
            Ok(())
        }
        #[cfg(target_env = "ohos")]
        {
            let _ = size;
            Err(anyhow!("PTY resize is unavailable on this platform"))
        }
    }

    pub fn write_stdin_for_session(
        &mut self,
        active_session_id: &str,
        task_id: &str,
        input: &str,
        close: bool,
    ) -> Result<()> {
        self.require_session_owner(task_id, active_session_id)?;
        self.write_stdin(task_id, input, close)
    }

    /// Get incremental output from a background process, consuming any new output.
    fn get_output_delta(
        &mut self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> Result<ShellDeltaResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        wait_while_running(shell, wait, timeout_ms);
        if shell.status != ShellStatus::Running && shell.owner_agent.is_some() {
            shell.completion_reported = true;
        }

        let (
            stdout_delta,
            stderr_delta,
            stdout_delta_len,
            stderr_delta_len,
            stdout_total,
            stderr_total,
        ) = shell.take_delta();
        let (stdout, stdout_meta) = truncate_with_meta(&stdout_delta);
        let (stderr, stderr_meta) = truncate_with_meta(&stderr_delta);
        let sandboxed = !matches!(shell.sandbox_type, SandboxType::None);

        let command = shell.command.clone();
        let result = ShellResult {
            task_id: Some(shell.id.clone()),
            status: shell.status.clone(),
            exit_code: shell.exit_code,
            stdout,
            stderr,
            duration_ms: u64::try_from(shell.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_len: stdout_meta.original_len.max(stdout_delta_len),
            stderr_len: stderr_meta.original_len.max(stderr_delta_len),
            stdout_omitted: stdout_meta.omitted,
            stderr_omitted: stderr_meta.omitted,
            stdout_truncated: stdout_meta.truncated,
            stderr_truncated: stderr_meta.truncated,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(shell.sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: shell.sandbox_denied(),
        };

        Ok(ShellDeltaResult {
            command,
            result,
            stdout_total_len: stdout_total,
            stderr_total_len: stderr_total,
        })
    }

    fn attach_heavy_permit(&mut self, task_id: &str, permit: HeavyCommandPermit) -> Result<()> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.heavy_permit = Some(permit);
        Ok(())
    }

    pub fn last_output_age_ms(&self, task_id: &str) -> Option<u64> {
        let shell = self.processes.get(task_id)?;
        Some(u64::try_from(shell.last_output_at.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    pub fn running_progress(&mut self, task_id: &str) -> Option<String> {
        let shell = self.processes.get_mut(task_id)?;
        shell.poll();
        if shell.status != ShellStatus::Running {
            return None;
        }
        let job = shell.job_snapshot();
        let quiet_secs = shell.last_output_at.elapsed().as_secs();
        Some(format!(
            "Progress: last output {quiet_secs}s ago; {}.",
            output_phase_hint(&format!("{}\n{}", job.stdout_tail, job.stderr_tail))
        ))
    }

    /// Kill a running background process
    pub fn kill(&mut self, task_id: &str) -> Result<ShellResult> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;

        shell.kill()?;
        shell.snapshot()
    }

    pub fn mark_cancel_reported(&mut self, result: &ShellResult) {
        if result.status == ShellStatus::Killed
            && let Some(shell) = result
                .task_id
                .as_deref()
                .and_then(|id| self.processes.get_mut(id))
        {
            shell.completion_reported = true;
        }
    }

    pub fn kill_for_session(
        &mut self,
        active_session_id: &str,
        task_id: &str,
    ) -> Result<ShellResult> {
        self.require_session_owner(task_id, active_session_id)?;
        self.kill(task_id)
    }

    /// Kill every currently running background shell process.
    #[cfg(any(test, feature = "test-support"))]
    pub fn kill_running(&mut self) -> Result<Vec<ShellResult>> {
        let ids = self
            .processes
            .iter()
            .filter(|(_, shell)| shell.status == ShellStatus::Running)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            results.push(self.kill(&id)?);
        }
        Ok(results)
    }

    pub fn kill_running_for_session(
        &mut self,
        active_session_id: &str,
    ) -> Result<Vec<ShellResult>> {
        let ids = self
            .processes
            .iter_mut()
            .filter(|(_, shell)| {
                !active_session_id.is_empty() && shell.owner_session_id == active_session_id
            })
            .filter_map(|(id, shell)| {
                shell.poll();
                (shell.status == ShellStatus::Running).then(|| id.clone())
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            results.push(self.kill(&id)?);
        }
        results.retain(|result| result.status == ShellStatus::Killed);
        Ok(results)
    }

    /// Transfer every still-running `persist:true` process out of Codewhale's
    /// ownership. This is called only by the real headless exec host after the
    /// enclosing turn has completed successfully.
    #[cfg(unix)]
    pub fn commit_persistent_services(&mut self) -> Result<Vec<PersistentServiceReceipt>> {
        let mut ids = self
            .processes
            .iter()
            .filter(|(_, shell)| shell.ownership == ShellOwnership::PersistPending)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();

        for id in &ids {
            let shell = self
                .processes
                .get_mut(id)
                .ok_or_else(|| anyhow!("Persistent service {id} disappeared before commit"))?;
            shell.poll();
            if shell.status != ShellStatus::Running {
                return Err(anyhow!(
                    "Persistent service {id} exited before ownership transfer (status {:?}, exit code {:?})",
                    shell.status,
                    shell.exit_code
                ));
            }
            if shell
                .child
                .as_ref()
                .and_then(ShellChild::process_id)
                .is_none()
            {
                return Err(anyhow!(
                    "Persistent service {id} has no releasable process id"
                ));
            }
        }

        let mut receipts = Vec::with_capacity(ids.len());
        for id in ids {
            let mut shell = self
                .processes
                .remove(&id)
                .ok_or_else(|| anyhow!("Persistent service {id} disappeared during commit"))?;
            let pid = shell
                .child
                .as_ref()
                .and_then(ShellChild::process_id)
                .ok_or_else(|| anyhow!("Persistent service {id} lost its process id"))?;
            unregister_pending_persistent_process_group(pid);
            shell.ownership = ShellOwnership::Released;
            shell.stdin = None;
            shell.heavy_permit.take();
            shell.work_lifecycle = None;
            receipts.push(PersistentServiceReceipt {
                task_id: id,
                pid,
                process_group_id: pid,
                ownership: "external".to_string(),
            });
        }
        Ok(receipts)
    }

    /// Kill only services waiting for a successful exec ownership transfer.
    /// Ordinary background jobs retain their existing manager lifetime.
    pub fn abort_persistent_services(&mut self) {
        let ids = self
            .processes
            .iter()
            .filter(|(_, shell)| shell.ownership == ShellOwnership::PersistPending)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            if let Err(error) = self.kill(&id) {
                tracing::warn!(shell_id = %id, %error, "failed to abort pending persistent service");
            }
        }
    }

    /// Poll a background process and return incremental output.
    #[cfg(any(test, feature = "test-support"))]
    pub fn poll_delta(
        &mut self,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> Result<ShellDeltaResult> {
        self.get_output_delta(task_id, wait, timeout_ms)
    }

    fn get_output_delta_for_session(
        &mut self,
        active_session_id: &str,
        task_id: &str,
        wait: bool,
        timeout_ms: u64,
    ) -> Result<ShellDeltaResult> {
        self.require_session_owner(task_id, active_session_id)?;
        self.get_output_delta(task_id, wait, timeout_ms)
    }

    /// Read a job's raw stream at an absolute byte offset without consuming
    /// anything. This is the `/v1/jobs` byte-stream contract: HTTP clients hold
    /// the cursor, so reads must not disturb the engine's own delta consumer.
    ///
    /// `cursor` is a byte offset into the stream's lifetime output (matching
    /// `total`). When the bounded buffer has already discarded `[0, dropped)`,
    /// the window starts at `dropped` instead and the caller sees the gap in
    /// the response rather than a replayed tail. With `wait_ms > 0` on a
    /// running job, polls up to that bound for new bytes past `cursor` before
    /// answering — long-poll instead of a hot loop.
    pub fn read_output_chunk(
        &mut self,
        task_id: &str,
        stream: ShellOutputStream,
        cursor: usize,
        max_bytes: usize,
        wait_ms: u64,
    ) -> Result<ShellOutputChunk> {
        let Some(shell) = self.processes.get_mut(task_id) else {
            // Evicted jobs retain only their snapshot tails. Serve that tail as
            // the final retained window so a late reader still gets the ending
            // of the stream instead of a bare not-found.
            let snapshot = self
                .stale_jobs
                .get(task_id)
                .ok_or_else(|| anyhow!("Job {task_id} not found"))?;
            let (tail, total) = match stream {
                ShellOutputStream::Stdout => (&snapshot.stdout_tail, snapshot.stdout_len),
                ShellOutputStream::Stderr => (&snapshot.stderr_tail, snapshot.stderr_len),
            };
            let tail_start = total.saturating_sub(tail.len());
            let offset = cursor.max(tail_start).min(total);
            let next_offset = offset.saturating_add(max_bytes).min(total);
            return Ok(ShellOutputChunk {
                offset,
                bytes: tail.as_bytes()[offset - tail_start..next_offset - tail_start].to_vec(),
                next_offset,
                total,
                dropped: tail_start,
                status: snapshot.status.clone(),
                exit_code: snapshot.exit_code,
            });
        };
        let buffer = match stream {
            ShellOutputStream::Stdout => shell.stdout_buffer.clone(),
            ShellOutputStream::Stderr => shell
                .stderr_buffer
                .clone()
                .ok_or_else(|| anyhow!("Job {task_id} merges stderr into stdout"))?,
        };

        let wait_deadline = (wait_ms > 0 && shell.status == ShellStatus::Running)
            .then(|| Instant::now() + Duration::from_millis(wait_ms.clamp(50, 30_000)));
        loop {
            shell.poll();
            let total = buffer.lock().map(|guard| guard.total_len()).unwrap_or(0);
            let done_waiting = total > cursor
                || shell.status != ShellStatus::Running
                || wait_deadline.is_none_or(|deadline| Instant::now() >= deadline);
            if done_waiting {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let (bytes, offset, next_offset, total, dropped) = {
            let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let total = guard.total_len();
            let dropped = guard.dropped();
            let offset = cursor.max(dropped).min(total);
            let next_offset = offset.saturating_add(max_bytes).min(total);
            let retained = guard.retained();
            let bytes = retained[offset - dropped..next_offset - dropped].to_vec();
            (bytes, offset, next_offset, total, dropped)
        };
        Ok(ShellOutputChunk {
            offset,
            bytes,
            next_offset,
            total,
            dropped,
            status: shell.status.clone(),
            exit_code: shell.exit_code,
        })
    }

    /// Attach durable task context to a live shell job.
    pub fn tag_linked_task(&mut self, task_id: &str, linked_task_id: Option<String>) -> Result<()> {
        let shell = self
            .processes
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("Task {task_id} not found"))?;
        shell.linked_task_id = linked_task_id;
        Ok(())
    }

    /// Inspect full output for a live or stale job.
    pub fn inspect_job(&mut self, task_id: &str) -> Result<ShellJobDetail> {
        if let Some(shell) = self.processes.get_mut(task_id) {
            shell.poll();
            return Ok(shell.job_detail());
        }
        if let Some(snapshot) = self.stale_jobs.get(task_id) {
            return Ok(ShellJobDetail {
                snapshot: snapshot.clone(),
                stdout: snapshot.stdout_tail.clone(),
                stderr: snapshot.stderr_tail.clone(),
            });
        }
        Err(anyhow!("Job {task_id} not found"))
    }

    pub fn inspect_job_for_session(
        &mut self,
        active_session_id: &str,
        task_id: &str,
    ) -> Result<ShellJobDetail> {
        self.require_session_owner(task_id, active_session_id)?;
        self.inspect_job(task_id)
    }

    /// List all live and known-stale background shell jobs for the TUI.
    pub fn list_jobs(&mut self) -> Vec<ShellJobSnapshot> {
        for shell in self.processes.values_mut() {
            shell.poll();
        }
        // Evict completed processes older than 1 hour to bound memory growth.
        self.cleanup(FINISHED_SHELL_MAX_AGE);

        let mut jobs = self
            .processes
            .values()
            .map(BackgroundShell::job_snapshot)
            .collect::<Vec<_>>();
        jobs.extend(self.stale_jobs.values().cloned());
        jobs.sort_by(|a, b| {
            job_status_rank(&a.status, a.stale)
                .cmp(&job_status_rank(&b.status, b.stale))
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs
    }

    pub fn list_jobs_for_session(&mut self, active_session_id: &str) -> Vec<ShellJobSnapshot> {
        if active_session_id.is_empty() {
            return Vec::new();
        }
        self.list_jobs()
            .into_iter()
            .filter(|job| job.owner_session_id == active_session_id)
            .collect()
    }

    /// Whether a finished parent-owned job's completion is waiting to be
    /// claimed. Unlike
    /// [`Self::may_have_undelivered_completion`] this polls, so it reports
    /// readiness the moment the process exits; the engine's idle shell wake
    /// uses it to fire exactly when evidence exists.
    #[cfg(any(test, feature = "test-support"))]
    pub fn has_finished_unreported_jobs(&mut self) -> bool {
        self.processes.values_mut().any(|shell| {
            shell.poll();
            shell.owner_agent.is_none()
                && shell.status != ShellStatus::Running
                && !shell.completion_reported
        })
    }

    pub fn has_finished_unreported_jobs_for_session(&mut self, active_session_id: &str) -> bool {
        !active_session_id.is_empty()
            && self.processes.values_mut().any(|shell| {
                shell.poll();
                shell.owner_session_id == active_session_id
                    && shell.owner_agent.is_none()
                    && shell.status != ShellStatus::Running
                    && !shell.completion_reported
            })
    }

    /// Drain once-only completion events together with lossless stream bytes.
    /// The engine publishes the bytes outside this manager's mutex and puts
    /// only the bounded event plus resulting handle into model context.
    #[cfg(any(test, feature = "test-support"))]
    pub fn drain_finished_jobs_with_evidence(&mut self) -> Vec<ShellCompletionEvidence> {
        self.drain_finished_jobs_with_evidence_inner(None)
    }

    pub fn drain_finished_jobs_with_evidence_for_session(
        &mut self,
        active_session_id: &str,
    ) -> Vec<ShellCompletionEvidence> {
        self.drain_finished_jobs_with_evidence_inner(Some(active_session_id))
    }

    fn drain_finished_jobs_with_evidence_inner(
        &mut self,
        active_session_id: Option<&str>,
    ) -> Vec<ShellCompletionEvidence> {
        let mut completions = Vec::new();
        for shell in self.processes.values_mut() {
            shell.poll();
            let owned = active_session_id.is_none_or(|session_id| {
                !session_id.is_empty() && shell.owner_session_id == session_id
            });
            if owned
                && shell.owner_agent.is_none()
                && shell.status != ShellStatus::Running
                && !shell.completion_reported
            {
                shell.completion_reported = true;
                completions.push(shell.completion_evidence());
                // The bytes are now in the caller's hands (they become a durable
                // session artifact). Holding a second copy here for the rest of
                // the retention hour is what #5472 measured.
                shell.release_delivered_output();
            }
        }
        completions.sort_by(|a, b| a.event.task_id.cmp(&b.event.task_id));
        completions
    }

    /// A terminal foreground result is already returned as the tool result;
    /// do not emit it again through the background-completion channel.
    fn acknowledge_foreground_completion(&mut self, task_id: &str) {
        if let Some(shell) = self.processes.get_mut(task_id) {
            shell.completion_reported = true;
            // The caller already holds this job's `ShellResult`; the record only
            // stays listed so `/jobs` can show it. A 1,200-char tail is all any
            // remaining consumer reads, so the rest is released now instead of
            // at the 1 h `cleanup` (#5472 finding 1 — the dominant term: every
            // uppercase `Bash` call, foreground included, went through here).
            shell.release_delivered_output();
        }
    }

    /// Whether the next production turn may inject a parent-owned shell
    /// completion event.
    ///
    /// This deliberately does not poll processes or flip
    /// `completion_reported`: preview is read-only. A running job counts as
    /// pending because it can finish before production drains completions; in
    /// that race an exact request body cannot be proved without mutation.
    #[cfg(any(test, feature = "test-support"))]
    pub fn may_have_undelivered_completion(&self) -> bool {
        self.processes
            .values()
            .any(|shell| shell.owner_agent.is_none() && !shell.completion_reported)
    }

    pub fn may_have_undelivered_completion_for_session(&self, active_session_id: &str) -> bool {
        !active_session_id.is_empty()
            && self.processes.values().any(|shell| {
                shell.owner_session_id == active_session_id
                    && shell.owner_agent.is_none()
                    && !shell.completion_reported
            })
    }

    /// Return agent owners whose tracked shell work is still running. The
    /// engine uses this to keep a worker's heartbeat alive while its only
    /// pending work is an explicitly tracked background shell task.
    #[cfg(any(test, feature = "test-support"))]
    pub fn running_owner_agent_ids(&mut self) -> Vec<String> {
        self.running_owner_agent_ids_inner(None)
    }

    pub fn running_owner_agent_ids_for_session(&mut self, active_session_id: &str) -> Vec<String> {
        self.running_owner_agent_ids_inner(Some(active_session_id))
    }

    fn running_owner_agent_ids_inner(&mut self, active_session_id: Option<&str>) -> Vec<String> {
        let mut owners = self
            .processes
            .values_mut()
            .filter_map(|shell| {
                shell.poll();
                (shell.status == ShellStatus::Running
                    && active_session_id.is_none_or(|session_id| {
                        !session_id.is_empty() && shell.owner_session_id == session_id
                    }))
                .then(|| {
                    shell
                        .owner_agent
                        .as_ref()
                        .map(|owner| owner.agent_id.clone())
                })
                .flatten()
            })
            .collect::<Vec<_>>();
        owners.sort();
        owners.dedup();
        owners
    }

    /// Remember a restart-stale job so the UI can show it instead of hiding it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn remember_stale_job(
        &mut self,
        id: impl Into<String>,
        command: impl Into<String>,
        cwd: PathBuf,
        linked_task_id: Option<String>,
    ) {
        let id = id.into();
        self.stale_jobs.insert(
            id.clone(),
            ShellJobSnapshot {
                id: id.clone(),
                job_id: id,
                command: command.into(),
                cwd,
                status: ShellStatus::Killed,
                background: true,
                finished_at: None,
                exit_code: None,
                elapsed_ms: 0,
                stdout_tail: String::new(),
                stderr_tail: "Process is no longer attached to this TUI session.".to_string(),
                stdout_len: 0,
                stderr_len: 0,
                stdin_available: false,
                stale: true,
                elapsed_since_output_ms: None,
                linked_task_id,
                owner_agent_id: None,
                owner_agent_name: None,
                origin_tool_call_id: None,
                origin_turn_id: None,
                owner_session_id: String::new(),
            },
        );
    }

    /// Clean up completed processes older than the given duration, then enforce
    /// the count and byte ceilings on what is left.
    ///
    /// Age alone is not a bound: it only fired from `list_jobs()`, so a session
    /// that never opened the jobs panel evicted nothing, and 500 finished
    /// records inside one hour were all retained regardless of size (#5472).
    ///
    /// Age counts from when a job finished, not when it started: a job that
    /// ran longer than `max_age` would otherwise be dropped the moment it
    /// exited, before its completion was delivered. Undelivered completions
    /// age out too: jobs launched over the Runtime API, or owned by a session
    /// that is no longer active, are never drained.
    pub fn cleanup(&mut self, max_age: Duration) {
        self.processes.retain(|_, shell| {
            if shell.status == ShellStatus::Running {
                return true;
            }
            shell.mark_finished();
            shell
                .finished_at
                .is_none_or(|finished| finished.elapsed() < max_age)
        });
        self.enforce_finished_job_bounds();
    }

    /// Bytes still held across every tracked job. The retention bound in #5472
    /// is stated in these terms, so the tests assert on them directly.
    #[cfg(all(test, unix))]
    pub fn retained_output_bytes_total(&self) -> usize {
        self.processes
            .values()
            .map(BackgroundShell::retained_output_bytes)
            .fold(0, usize::saturating_add)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn tracked_job_count(&self) -> usize {
        self.processes.len()
    }

    /// Drop the oldest finished records once they exceed either ceiling.
    /// Running jobs are never evicted — killing the handle would orphan a live
    /// process — and a job whose completion has not been delivered yet is
    /// evicted last, since dropping it loses the only copy of its result.
    fn enforce_finished_job_bounds(&mut self) {
        let mut finished = self
            .processes
            .iter()
            .filter(|(_, shell)| shell.status != ShellStatus::Running)
            .map(|(id, shell)| {
                (
                    id.clone(),
                    shell.completion_reported || shell.owner_agent.is_some(),
                    // Oldest by finish, like the age rule in `cleanup`.
                    shell.finished_at.unwrap_or(shell.started_at),
                    shell.retained_output_bytes(),
                )
            })
            .collect::<Vec<_>>();
        let total_bytes: usize = finished
            .iter()
            .map(|(_, _, _, bytes)| *bytes)
            .fold(0, usize::saturating_add);
        if finished.len() <= MAX_FINISHED_SHELL_RECORDS && total_bytes <= MAX_FINISHED_SHELL_BYTES {
            return;
        }
        // Undelivered parent completions last, then oldest first. Child-owned
        // results never reach the parent model, so they rank with delivered.
        finished.sort_by(|a, b| a.1.cmp(&b.1).reverse().then_with(|| a.2.cmp(&b.2)));
        let mut remaining_count = finished.len();
        let mut remaining_bytes = total_bytes;
        for (id, _, _, bytes) in finished {
            if remaining_count <= MAX_FINISHED_SHELL_RECORDS
                && remaining_bytes <= MAX_FINISHED_SHELL_BYTES
            {
                break;
            }
            self.processes.remove(&id);
            remaining_count -= 1;
            remaining_bytes = remaining_bytes.saturating_sub(bytes);
        }
    }
}

fn wait_while_running(shell: &mut BackgroundShell, wait: bool, timeout_ms: u64) {
    if wait && shell.status == ShellStatus::Running {
        let timeout = Duration::from_millis(timeout_ms.clamp(1000, 600_000));
        let deadline = Instant::now() + timeout;

        while shell.status == ShellStatus::Running && Instant::now() < deadline {
            if shell.poll() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    } else {
        shell.poll();
    }
}

fn job_status_rank(status: &ShellStatus, stale: bool) -> u8 {
    if stale {
        return 4;
    }
    match status {
        ShellStatus::Running => 0,
        ShellStatus::Failed | ShellStatus::TimedOut => 1,
        ShellStatus::Killed => 2,
        ShellStatus::Completed => 3,
    }
}

/// Thread-safe wrapper for `ShellManager`
pub type SharedShellManager = Arc<Mutex<ShellManager>>;

/// Create a new shared shell manager with default sandbox policy.
pub fn new_shared_shell_manager(workspace: PathBuf) -> SharedShellManager {
    Arc::new(Mutex::new(ShellManager::new(workspace)))
}

// === ToolSpec Implementations ===

use crate::tools::cargo_failure_summary::summarize_cargo_failure;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, optional_u64, required_str, type_mismatch,
};
use async_trait::async_trait;
use codewhale_execpolicy::command_safety::{
    NetworkRead, ReadonlyRejection, SafetyAnalysis, SafetyLevel, agent_readonly_verdict,
    analyze_command, extract_primary_command, is_parallel_readonly_command,
    normalize_windows_command_paths, readonly_network_reads, split_leading_cd,
};
use codewhale_execpolicy::toml_rules::{ExecPolicyConfig, RuleDecision};
use codewhale_runtime::features::Feature;
use serde_json::json;

/// The TOML execpolicy file lives in the user config home; the rule engine
/// itself is `codewhale_execpolicy::toml_rules`.
fn default_execpolicy_path() -> Option<std::path::PathBuf> {
    codewhale_paths::user_home().map(|home| home.join(".deepseek").join("execpolicy.toml"))
}

fn load_default_policy() -> anyhow::Result<Option<ExecPolicyConfig>> {
    /// A parsed rules file, tagged with the identity it was parsed from.
    type PolicyKey = (std::path::PathBuf, u64, Option<std::time::SystemTime>);

    let Some(path) = default_execpolicy_path() else {
        return Ok(None);
    };
    // An unreadable or missing file (including a permissions error, which
    // `exists()` also swallows) means "no file rules" — the same answer as
    // before, just reached with one `stat` instead of an existence check plus a
    // full read.
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    let key: PolicyKey = (path.clone(), metadata.len(), metadata.modified().ok());

    // #6208: this runs on every shell execution, so the read and TOML parse
    // happen only when the file's identity changes. Length joins the timestamp
    // because a coarse-mtime filesystem can hand back the same instant for two
    // different revisions.
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(PolicyKey, ExecPolicyConfig)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((cached_key, config)) = cache.as_ref()
        && *cached_key == key
    {
        return Ok(Some(config.clone()));
    }

    let config = ExecPolicyConfig::from_path(&path)?;
    *cache = Some((key, config.clone()));
    Ok(Some(config))
}

/// The last `n` lines of `text`, marking how many were left out.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        return text.to_string();
    }
    format!(
        "[{} earlier lines]\n{}",
        lines.len() - n,
        lines[lines.len() - n..].join("\n")
    )
}

const MACOS_PROVENANCE_HINT: &str = "Docker buildx failed to update its activity file due to a macOS \
com.apple.provenance restriction. Files created by Docker Desktop's signed process carry a \
kernel-enforced provenance tag that blocks writes from child processes (including the TUI \
shell sandbox). Workarounds: (1) run the Docker build from a regular terminal outside the \
TUI, or (2) disable BuildKit with DOCKER_BUILDKIT=0 (only works if your Dockerfiles do not \
use RUN --mount directives).";

/// Human-readable exit status for a shell result: the numeric code when the
/// process returned one, or "terminated by signal" when it did not (rather
/// than leaking `Some(127)` / `None` Debug output to the user).
fn exit_code_label(code: Option<i64>) -> String {
    match (code, exit_code_hex(code)) {
        (Some(code), Some(hex)) => format!("exit code {code} ({hex})"),
        (Some(code), None) => format!("exit code {code}"),
        (None, _) => "terminated by signal".to_string(),
    }
}

fn exit_code_hex(code: Option<i64>) -> Option<String> {
    code.filter(|code| *code > i64::from(i32::MAX) && *code <= i64::from(u32::MAX))
        .map(|code| format!("0x{code:08X}"))
}
const PYTHON_BUILD_DEPENDENCY_HINT: &str = "Python build dependency missing: setuptools is not \
available in the active environment. Install the declared build requirements first, for example \
`python -m pip install -U pip setuptools wheel build`, then rerun the build command.";

fn attach_cargo_failure_summary(
    metadata: &mut serde_json::Value,
    command: &str,
    result: &ShellResult,
) {
    if let Some(summary) = summarize_cargo_failure(
        command,
        &result.stdout,
        &result.stderr,
        result.exit_code.and_then(|code| i32::try_from(code).ok()),
    ) {
        metadata["cargo_failure_summary"] = summary.to_metadata_value();
    }
}

fn attach_python_build_dependency_hint(
    metadata: &mut serde_json::Value,
    hint: Option<&'static str>,
) {
    if let Some(hint) = hint {
        metadata["python_build_dependency_hint"] = json!({
            "kind": "missing_setuptools",
            "hint": hint,
            "recommended_first_step": "python -m pip install -U pip setuptools wheel build",
        });
    }
}

pub fn looks_like_macos_provenance_failure(result: &ShellResult) -> bool {
    if matches!(result.status, ShellStatus::Completed) && result.exit_code == Some(0) {
        return false;
    }
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_ascii_lowercase();
    combined.contains("com.apple.provenance")
        || combined.contains("update builder last activity")
        || (combined.contains("buildx/activity") && combined.contains("operation not permitted"))
}

fn macos_provenance_hint(result: &ShellResult) -> Option<&'static str> {
    if looks_like_macos_provenance_failure(result) {
        Some(MACOS_PROVENANCE_HINT)
    } else {
        None
    }
}

fn python_build_dependency_hint(command: &str, result: &ShellResult) -> Option<&'static str> {
    if matches!(result.status, ShellStatus::Completed) && result.exit_code == Some(0) {
        return None;
    }

    let command = command.to_ascii_lowercase();
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_ascii_lowercase();
    let mentions_missing_setuptools = [
        "no module named 'setuptools'",
        "no module named \"setuptools\"",
        "setuptools is not available",
        "cannot import 'setuptools",
        "cannot import \"setuptools",
        "missing dependencies",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
        && combined.contains("setuptools");
    if !mentions_missing_setuptools {
        return None;
    }

    let pythonish_command = [
        "python",
        "pip",
        "pytest",
        "tox",
        "nox",
        "cython",
        "setup.py",
        "build_ext",
    ]
    .iter()
    .any(|needle| command.contains(needle));
    let pythonish_output = [
        "setup.py",
        "pyproject.toml",
        "build_meta",
        "build_ext",
        "pep 517",
        "cython",
    ]
    .iter()
    .any(|needle| combined.contains(needle));

    if pythonish_command || pythonish_output {
        Some(PYTHON_BUILD_DEPENDENCY_HINT)
    } else {
        None
    }
}

fn command_likely_needs_network(command: &str) -> bool {
    let normalized = command.to_ascii_lowercase();
    let Some(primary) = extract_primary_command(&normalized) else {
        return false;
    };
    let primary = primary.rsplit(['/', '\\']).next().unwrap_or(primary);

    match primary {
        "curl" | "wget" | "fetch" | "nc" | "netcat" | "ncat" | "ssh" | "scp" | "sftp" | "rsync"
        | "ftp" | "ping" | "traceroute" | "nslookup" | "dig" | "host" | "nmap" | "gh" | "hub" => {
            true
        }
        "git" => [
            " fetch",
            " pull",
            " clone",
            " ls-remote",
            " submodule",
            " push",
        ]
        .iter()
        .any(|needle| normalized.contains(needle)),
        "cargo" => [" install", " fetch", " update", " publish", " search"]
            .iter()
            .any(|needle| normalized.contains(needle)),
        "npm" | "pnpm" | "yarn" => [" install", " i", " add", " update", " publish"]
            .iter()
            .any(|needle| normalized.contains(needle)),
        "pip" | "pip3" | "uv" | "poetry" => [" install", " add", " sync", " update"]
            .iter()
            .any(|needle| normalized.contains(needle)),
        "brew" | "apt" | "apt-get" | "yum" | "dnf" | "pacman" => true,
        "go" => [" get", " install", " mod download"]
            .iter()
            .any(|needle| normalized.contains(needle)),
        _ => false,
    }
}

fn looks_like_network_blocked_failure(result: &ShellResult) -> bool {
    if matches!(result.status, ShellStatus::Completed | ShellStatus::Running)
        || result.exit_code == Some(0)
    {
        return false;
    }

    if result.stdout.trim() == "000" {
        return true;
    }
    if result.sandboxed && result.stdout.is_empty() && result.stderr.is_empty() {
        return true;
    }

    let output = format!("{}\n{}", result.stdout, result.stderr).to_ascii_lowercase();
    [
        "operation not permitted",
        "network is unreachable",
        "could not resolve host",
        "couldn't resolve host",
        "failed to resolve",
        "temporary failure in name resolution",
        "name or service not known",
        "nodename nor servname provided",
        "no address associated",
        "failed to connect",
        "couldn't connect",
        "connection timed out",
        "connection reset",
    ]
    .iter()
    .any(|pattern| output.contains(pattern))
}

const READONLY_OFFLINE_NETWORK_HINT: &str = "This read_only run has no network and cannot write files. Read-only commands reach the network only in Full Access, with a session sandbox that allows network, a [network] policy that does not deny by default, and this agent's network grant. Use a web tool if this session has one; otherwise report the source as unreachable.";

fn readonly_offline_network_hint(command: &str, result: &ShellResult) -> Option<&'static str> {
    if result.sandbox_denied && codewhale_sandbox::bwrap_setup_failure(&result.stderr).is_some() {
        return None;
    }
    let rest = split_leading_cd(command).map(|(_, rest)| rest);
    let command = rest.as_deref().unwrap_or(command);
    (command_likely_needs_network(command)
        && (result.sandbox_denied || looks_like_network_blocked_failure(result)))
    .then_some(READONLY_OFFLINE_NETWORK_HINT)
}

fn shell_network_restricted_hint<'a>(
    context: &'a ToolContext,
    command: &str,
    result: &ShellResult,
) -> Option<&'a str> {
    if result.sandbox_denied && codewhale_sandbox::bwrap_setup_failure(&result.stderr).is_some() {
        return None;
    }
    let hint = context.shell_network_denied_hint.as_deref()?;
    let policy_blocks_network = context
        .elevated_sandbox_policy
        .as_ref()
        .is_some_and(|policy| !policy.has_network_access());
    if !policy_blocks_network || !command_likely_needs_network(command) {
        return None;
    }
    if result.sandbox_denied || looks_like_network_blocked_failure(result) {
        Some(hint)
    } else {
        None
    }
}

/// Coaching line when the execution sandbox denied a command and the
/// Plan-mode network hint did not already explain it. Most often a write under
/// a read-only posture: name the effective posture and the Ask-only retry shape
/// so other postures do not mistake it for autonomous authority.
fn shell_sandbox_denied_hint(context: &ToolContext, result: &ShellResult) -> Option<String> {
    if !result.sandbox_denied {
        return None;
    }
    if let Some(error) = codewhale_sandbox::bwrap_setup_failure(&result.stderr) {
        return Some(format!(
            "The Linux sandbox (bubblewrap) could not be set up, so this command did not run and nothing ran outside the sandbox: {error}. Every sandboxed command fails the same way until the cause it names is fixed or removed, or `prefer_bwrap = false` is set to run without the OS sandbox. [sandbox: Ask-only escalation — to run only this command without the sandbox, retry it once with sandbox_permissions \"danger-full-access\" + justification; the approval prompt asks the user]"
        ));
    }
    let policy = context.elevated_sandbox_policy.as_ref()?;
    Some(format!(
        "The execution sandbox blocked this command. Effective sandbox posture: {}. [sandbox: Ask-only escalation — retry this exact command once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]",
        policy.posture_label()
    ))
}

fn shell_job_owner_from_context(context: &ToolContext) -> Option<ShellJobOwner> {
    let agent_id = context
        .owner_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let agent_name = context
        .owner_agent_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(agent_id);
    Some(ShellJobOwner {
        agent_id: agent_id.to_string(),
        agent_name: agent_name.to_string(),
    })
}

fn shell_work_lifecycle_from_context(context: &ToolContext) -> Option<ShellWorkLifecycle> {
    context
        .runtime
        .work
        .as_ref()
        .map(|work| ShellWorkLifecycle {
            work: work.clone(),
            session_id: context.state_namespace.clone(),
        })
}

fn lifecycle_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn attach_shell_owner_metadata(metadata: &mut serde_json::Value, context: &ToolContext) {
    let Some(owner) = shell_job_owner_from_context(context) else {
        return;
    };
    metadata["owner_agent_id"] = json!(owner.agent_id);
    metadata["owner_agent_name"] = json!(owner.agent_name);
}

/// NUL bytes cannot cross the `exec` boundary: `Command` panics on them.
/// Refuse with the byte offset before anything spawns (#5529).
fn require_no_nul<'a>(value: &'a str, field: &str) -> Result<&'a str, ToolError> {
    if let Some(offset) = value.find('\0') {
        return Err(ToolError::invalid_input(format!(
            "Shell {field} contains a NUL byte at byte offset {offset}; it cannot cross the exec boundary. Remove it (usually a truncated heredoc or binary paste) and re-send."
        )));
    }
    Ok(value)
}

/// Apply the network policy to every network read inside a read-only
/// command, segment by segment, so a pipeline or chain cannot hide one.
/// A full shell keeps its historical scope here: only a lone `gh` read is
/// judged, because its other network use is governed elsewhere.
fn enforce_readonly_network_reads(command: &str, context: &ToolContext) -> Result<(), ToolError> {
    let Some(decider) = context.network_policy.as_ref() else {
        return Ok(());
    };
    let mut reads = readonly_network_reads(command);
    if context.shell_policy != ShellPolicy::ReadOnly {
        let lone = agent_readonly_verdict(command).is_ok_and(|segments| segments.len() == 1);
        reads.retain(|read| lone && *read == NetworkRead::GitHub);
    }
    use codewhale_net::network_policy::Decision;
    for read in reads {
        let host = read.host();
        match decider.evaluate(host, "Bash") {
            Decision::Allow => {}
            Decision::Deny => {
                return Err(ToolError::permission_denied(format!(
                    "Read-only network access to '{host}' is blocked by the active network policy."
                )));
            }
            Decision::Prompt => {
                return Err(ToolError::permission_denied(format!(
                    "Read-only network access to '{host}' requires network approval; allow that host in the parent session or network policy first."
                )));
            }
        }
    }
    Ok(())
}

/// This is a request for mandatory filesystem isolation, not a claim
/// about what the command does. The executor refuses it without a native
/// enforcing sandbox and checks the prepared environment again before spawn.
fn enforced_readonly_input(input: &serde_json::Value) -> bool {
    let Some(fields) = input.as_object() else {
        return false;
    };
    fields.get("read_only").and_then(serde_json::Value::as_bool) == Some(true)
        && fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "action" | "command" | "cwd" | "timeout_ms" | "read_only"
            )
        })
        && fields
            .get("action")
            .is_none_or(|action| action.as_str() == Some("run"))
        && fields
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|command| !command.trim().is_empty())
}

fn is_native_readonly_sandbox(sandbox_type: SandboxType) -> bool {
    match sandbox_type {
        #[cfg(target_os = "macos")]
        SandboxType::MacosSeatbelt => true,
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        SandboxType::LinuxBubblewrap => true,
        _ => false,
    }
}

/// Build the child for a tool that runs workspace code outside `exec_shell`
/// (gate commands, the cargo test runner). It gets exactly the confinement a
/// shell command gets in this session: the same policy, the same OS sandbox
/// wrapper, and the sanitized child environment. Callers keep their own
/// process-tree containment, timeout and cancellation.
///
/// Known limitations: where the platform has no enforcing sandbox configured
/// (Linux without bubblewrap, Windows) a workspace-write policy runs
/// unsandboxed, as it does for `exec_shell`; a read-only policy refuses there.
/// A session whose commands run in an external sandbox backend cannot run
/// these local children at all.
pub fn sandboxed_runner_command(
    context: &crate::tools::spec::ToolContext,
    program: &str,
    args: Vec<String>,
    cwd: &Path,
    timeout: Duration,
) -> std::result::Result<tokio::process::Command, crate::tools::spec::ToolError> {
    use crate::tools::spec::ToolError;
    if context.sandbox_backend.is_some() {
        return Err(ToolError::not_available(
            "this tool starts a local process, and this session runs commands in an external sandbox; run the command through the shell tool instead",
        ));
    }
    let exec_env = context
        .shell_manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .prepare_runner(
            program,
            args,
            cwd,
            timeout,
            context.elevated_sandbox_policy.clone(),
        );
    if matches!(exec_env.policy, ExecutionSandboxPolicy::ReadOnly { .. }) {
        require_native_readonly_execution(&exec_env)
            .map_err(|error| ToolError::permission_denied(error.to_string()))?;
    }
    let mut cmd = tokio::process::Command::new(exec_env.program());
    codewhale_util::utils::suppress_tokio_console_window(&mut cmd);
    cmd.args(exec_env.args())
        .current_dir(&exec_env.cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Workspace code never inherits parent credentials.
    codewhale_runtime::child_env::apply_to_tokio_command(
        &mut cmd,
        codewhale_runtime::child_env::string_map_env(&exec_env.env),
    );
    Ok(cmd)
}

fn require_native_readonly_execution(exec_env: &ExecEnv) -> Result<()> {
    if matches!(exec_env.policy, ExecutionSandboxPolicy::ReadOnly { .. })
        && is_native_readonly_sandbox(exec_env.sandbox_type)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "read_only execution requires an enforcing native read-only sandbox; nothing was run"
        ))
    }
}

/// `exec_shell_input_is_parallel_readonly` with the agent-posture classifier:
/// same input-shape restrictions (run action only, no background/tty/stdin),
/// but commands are judged by
/// [`codewhale_execpolicy::command_safety::agent_readonly_verdict`] so
/// `ShellPolicy::ReadOnly` agents keep a usable inspection surface
/// (pipelines and chains of reads, globs, `git -C`, `find`, `sed -n`,
/// `npm view`). Callers pass input already through [`normalize_readonly_cd`].
fn exec_shell_input_agent_readonly_verdict(
    input: &serde_json::Value,
) -> Result<(), ReadonlyRejection> {
    if enforced_readonly_input(input) {
        return Ok(());
    }
    if !exec_shell_input_is_parallel_readonly_shape(input) {
        return Err(ReadonlyRejection::new(
            "shape",
            "read-only shell accepts only a foreground `command` with optional `cwd` and timeout; background, stdin, interactive and TTY modes are not admitted",
        ));
    }
    let command = input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .expect("shape check established a command string");
    agent_readonly_verdict(command).map(|_| ())
}

/// Move a leading `cd <dir> &&` into the `cwd` field, resolved against any
/// `cwd` already present, so no gate has to interpret `cd`: the ordinary
/// working-directory workspace check then judges the directory, and every
/// gate and the executor see the same rewritten input. Only the shape
/// [`split_leading_cd`] accepts is rewritten; `read_only: true` input runs
/// its command unchanged under the enforced lane.
pub fn normalize_readonly_cd(input: &serde_json::Value) -> serde_json::Value {
    let mut input = input.clone();
    if enforced_readonly_input(&input) {
        return input;
    }
    // Bounded: each pass removes one leading `cd`.
    for _ in 0..4 {
        let Some((dir, rest)) = input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .and_then(split_leading_cd)
        else {
            break;
        };
        let cwd = match input.get("cwd") {
            None | Some(serde_json::Value::Null) => dir,
            Some(serde_json::Value::String(existing)) => std::path::Path::new(existing)
                .join(&dir)
                .to_string_lossy()
                .into_owned(),
            // A wrong type is reported by the executor's own type check.
            Some(_) => break,
        };
        input["command"] = json!(rest);
        input["cwd"] = json!(cwd);
    }
    input
}

/// The refusal a read-only agent sees, shared by every gate that judges the
/// agent grammar (subagent posture, durable authority, and the executor), so
/// the rule text is byte-identical wherever a command is refused. The next
/// steps name only tools such an agent has.
pub fn readonly_refusal(rejection: &ReadonlyRejection, enforced_lane: bool) -> String {
    let lane = if enforced_lane {
        ", rerun it with `read_only: true` to run it in an enforced read-only sandbox"
    } else {
        ""
    };
    format!(
        "{rejection}. Next: use admitted reads only (the shell tool description lists the grammar), read or search files with the File tool{lane}, or, if the probe is essential, return your findings and the blocked probe to the parent."
    )
}

/// Whether `read_only: true` can run here: a native enforcing sandbox and
/// no external backend.
pub fn readonly_enforced_lane_available(context: &ToolContext) -> bool {
    context.sandbox_backend.is_none()
        && context.shell_manager.lock().is_ok_and(|manager| {
            manager
                .configured_sandbox_type()
                .is_some_and(is_native_readonly_sandbox)
        })
}

pub fn readonly_lane_network_access(context: &ToolContext) -> bool {
    (context.auto_approve || context.approval_mode == codewhale_execpolicy::ApprovalMode::Bypass)
        && context
            .elevated_sandbox_policy
            .as_ref()
            .is_some_and(ExecutionSandboxPolicy::has_network_access)
        && context
            .network_policy
            .as_ref()
            .is_none_or(|decider| !decider.denies_by_default())
        && crate::tools::web::fetch::network_authorized(context)
}

/// `exec_shell_input_agent_readonly_verdict` is also the gate-side predicate for the
/// subagent posture check (#5426): the catalog carve-out that admits
/// canonical `bash` to Scout/Reviewer/Planner must judge the same call the
/// `BashTool::execute` `ShellPolicy::ReadOnly` branch will judge, so the
/// posture gate can admit a proven-readonly call without ever widening past
/// the execute-time refusal.
pub fn agent_readonly_bash_input(input: &serde_json::Value) -> bool {
    agent_readonly_bash_verdict(input).is_ok()
}

/// [`agent_readonly_bash_input`] with the rule that refused the call.
pub fn agent_readonly_bash_verdict(input: &serde_json::Value) -> Result<(), ReadonlyRejection> {
    // Canonical lowercase `bash` advertises `timeout` in seconds, while the
    // internal executor contract uses `timeout_ms`. Normalize through the same
    // translator the concrete tool uses so posture, session approval, envelope,
    // and execute judge one input (#5595). Legacy/internal shapes fall back to
    // their existing direct classification. A leading `cd` is then moved into
    // `cwd` exactly as `BashTool::execute` does.
    let translated = contract_bash_legacy_input(input).unwrap_or_else(|_| input.clone());
    exec_shell_input_agent_readonly_verdict(&normalize_readonly_cd(&translated))
}

fn exec_shell_input_is_parallel_readonly(input: &serde_json::Value) -> bool {
    if enforced_readonly_input(input) {
        return true;
    }
    if !exec_shell_input_is_parallel_readonly_shape(input) {
        return false;
    }
    let command = input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .expect("shape check established a command string");
    is_parallel_readonly_command(command)
}

fn exec_shell_input_is_parallel_readonly_shape(input: &serde_json::Value) -> bool {
    let Some(fields) = input.as_object() else {
        return false;
    };
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "action" | "command" | "cwd" | "timeout_ms"))
    {
        return false;
    }
    match input.get("action") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(action)) if action == "run" => {}
        Some(_) => return false,
    }
    if ["background", "interactive", "tty", "combined_output"]
        .iter()
        .any(|key| {
            !matches!(
                input.get(*key),
                None | Some(serde_json::Value::Null | serde_json::Value::Bool(false))
            )
        })
    {
        return false;
    }
    if ["stdin", "input", "data"]
        .iter()
        .any(|key| input.get(*key).is_some())
    {
        return false;
    }
    if ["task_id", "id", "wait", "block", "close_stdin", "all"]
        .iter()
        .any(|key| input.get(*key).is_some())
    {
        return false;
    }

    input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some()
}

/// Whether a classifier-approved read needs a shell to join its segments or
/// apply an admitted redirect; a lone plain command runs as direct argv.
fn readonly_command_needs_shell(command: &str) -> bool {
    agent_readonly_verdict(command).map_or(true, |segments| {
        segments.len() > 1 || segments.iter().any(|segment| !segment.redirects.is_empty())
    })
}

fn hardened_readonly_script(command: &str, workspace: &std::path::Path) -> Result<String> {
    use codewhale_sandbox::shell_dispatcher::ShellKind;
    // POSIX quoting must never be passed to a different command interpreter.
    let supported = match codewhale_sandbox::shell_dispatcher::global_dispatcher().kind() {
        ShellKind::Bash => true,
        ShellKind::Custom { binary, .. } => matches!(
            std::path::Path::new(binary)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("bash" | "zsh")
        ),
        _ => false,
    };
    if !supported {
        return Err(anyhow!(
            "read-only pipelines and chains require bash or zsh; run each read separately"
        ));
    }
    let segments = agent_readonly_verdict(command)
        .map_err(|rejection| anyhow!("command is outside the read-only policy: {rejection}"))?;
    let mut script = String::from("set -o pipefail;");
    for segment in &segments {
        let (program, args) = hardened_readonly_argv(&segment.command)?;
        let program = resolve_readonly_program(&program, workspace)?;
        let program = program
            .to_str()
            .ok_or_else(|| anyhow!("read-only executable path is not valid UTF-8"))?;
        for word in std::iter::once(program).chain(args.iter().map(String::as_str)) {
            script.push(' ');
            script.push_str(&shell_words::quote(word));
        }
        for redirect in &segment.redirects {
            script.push(' ');
            script.push_str(redirect);
        }
        if let Some(join) = segment.join {
            script.push(' ');
            script.push_str(join.as_str());
        }
    }
    // The only shell syntax is the admitted operators and redirect words.
    // Filenames cannot expand into options or unvalidated symlinks; every Git
    // stage retains helper guards.
    Ok(script)
}

fn hardened_readonly_argv(command: &str) -> Result<(String, Vec<String>)> {
    let mut argv = shell_words::split(&normalize_windows_command_paths(command))
        .map_err(|error| anyhow!("could not parse classifier-approved read command: {error}"))?;
    if argv.is_empty() {
        return Err(anyhow!("classifier-approved read command was empty"));
    }

    // Even when repository/user configuration names a diff or signature
    // helper, these flags make Git keep the read inside its own process.
    if argv.first().is_some_and(|program| program == "git") {
        // The agent read-only classifier admits `git -C <dir>` and
        // `git --no-pager` before the subcommand; keep the preamble but
        // locate the subcommand after it so the hardening flags splice in
        // the right place. `-C` targets were already workspace-checked by
        // `enforce_readonly_workspace_operands`.
        let mut subcommand_index = 1;
        while let Some(flag) = argv.get(subcommand_index) {
            match flag.as_str() {
                "--no-pager" => subcommand_index += 1,
                "-C" => subcommand_index += 2,
                _ => break,
            }
        }
        let subcommand = argv
            .get(subcommand_index)
            .map(String::as_str)
            .ok_or_else(|| {
                anyhow!("classifier-approved Git read was missing its literal subcommand")
            })?;
        match subcommand {
            "diff" => {
                let at = subcommand_index + 1;
                argv.splice(
                    at..at,
                    codewhale_dependencies::Git::REVIEW_DIFF_ARGS.map(String::from),
                );
            }
            "log" | "show" => {
                let at = subcommand_index + 1;
                argv.splice(
                    at..at,
                    codewhale_dependencies::Git::REVIEW_DIFF_ARGS
                        .into_iter()
                        .chain(["--no-show-signature"])
                        .map(String::from),
                );
            }
            "blame" => {
                let at = subcommand_index + 1;
                argv.insert(at, "--no-textconv".to_string());
            }
            "status" | "ls-files" | "grep" => {}
            _ => {
                return Err(anyhow!(
                    "classifier-approved Git read did not keep its subcommand in argv[1]"
                ));
            }
        }
    }

    let program = argv.remove(0);
    Ok((program, argv))
}

/// The directory each `git` segment of a read-only command runs in: `cwd`
/// followed through any leading `-C` hops, as git itself resolves them.
fn readonly_git_dirs(command: &str, cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    // Split with the same lexer the classifier and executor use (#6637), so a
    // `git` segment after `&&`, `||` or `;` gets its filter hardening too.
    let segments = agent_readonly_verdict(command).map_or_else(
        |_| command.split('|').map(str::to_string).collect::<Vec<_>>(),
        |segments| {
            segments
                .into_iter()
                .map(|segment| segment.command)
                .collect()
        },
    );
    segments
        .iter()
        .filter_map(|segment| shell_words::split(&normalize_windows_command_paths(segment)).ok())
        .filter(|argv| argv.first().is_some_and(|program| program == "git"))
        .map(|argv| {
            let mut dir = cwd.to_path_buf();
            let mut args = argv.iter().skip(1);
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--no-pager" => {}
                    "-C" => match args.next() {
                        Some(target) => dir = dir.join(target),
                        None => break,
                    },
                    _ => break,
                }
            }
            dir
        })
        .collect()
}

fn enforce_readonly_workspace_operands(
    command: &str,
    workspace: &std::path::Path,
    effective_cwd: &std::path::Path,
) -> Result<(), ToolError> {
    // Judge each segment of a pipeline or chain on its own, so the `gh`
    // exemption below covers only the `gh` segment itself.
    let segments = agent_readonly_verdict(command).map_or_else(
        |_| vec![command.to_string()],
        |segments| {
            segments
                .into_iter()
                .map(|segment| segment.command)
                .collect()
        },
    );
    for segment in &segments {
        enforce_readonly_segment_operands(segment, workspace, effective_cwd)?;
    }
    Ok(())
}

fn enforce_readonly_segment_operands(
    command: &str,
    workspace: &std::path::Path,
    effective_cwd: &std::path::Path,
) -> Result<(), ToolError> {
    let argv = shell_words::split(&normalize_windows_command_paths(command)).map_err(|error| {
        ToolError::invalid_input(format!(
            "Could not parse read-only command arguments: {error}"
        ))
    })?;
    if argv.first().is_some_and(|program| program == "gh") {
        // High-level gh reads do not consume local path operands. Their host
        // is pinned and evaluated separately by the network-policy guard.
        return Ok(());
    }
    let workspace = workspace.canonicalize().map_err(|error| {
        ToolError::execution_failed(format!(
            "Could not resolve the Scout workspace before shell dispatch: {error}"
        ))
    })?;
    let effective_cwd = effective_cwd.canonicalize().map_err(|error| {
        ToolError::permission_denied(format!(
            "Could not prove the read-only shell working directory stays in the workspace: {error}"
        ))
    })?;
    if !effective_cwd.starts_with(&workspace) {
        return Err(ToolError::permission_denied(
            "[shell.readonly.cwd.outside_workspace] Read-only Scout shell working directory resolves outside the workspace.",
        ));
    }

    for token in argv.iter().skip(1) {
        if token.starts_with('-') && (token.contains('/') || token.contains('\\')) {
            return Err(ToolError::permission_denied(format!(
                "[shell.readonly.option.attached_path] Read-only Scout shell options may not carry attached paths; refused {token:?}. Use the bounded File read/search actions for project evidence."
            )));
        }
        let value = token
            .split_once('=')
            .map_or(token.as_str(), |(_, value)| value)
            .trim();
        // Windows `Path::canonicalize` returns verbatim device paths
        // (`\\?\C:\...`), which `Path::is_absolute` does not recognize; without
        // stripping the prefix the operand falls into the shape refusal and
        // every canonical absolute read is denied on Windows. The stripped
        // form resolves to the same location, so location-based judgement is
        // unchanged. On unix hosts such spellings stay fail-closed below.
        let value = value
            .strip_prefix(r"\\?\")
            .or_else(|| value.strip_prefix(r"\\.\"))
            .unwrap_or(value);
        if value.is_empty() || value == "-" {
            continue;
        }
        let candidate = std::path::Path::new(value);
        let bytes = value.as_bytes();
        let windows_prefixed =
            (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
                || value.starts_with("\\\\")
                || candidate
                    .components()
                    .any(|component| matches!(component, std::path::Component::Prefix(_)));

        // #5595: an absolute operand is safe by resolved location, not by
        // spelling. This is the canonical `git -C /absolute/workspace log`
        // case. Requiring canonicalization also keeps nonexistent output-like
        // targets fail-closed, while symlinks are judged by their destination.
        if candidate.is_absolute() {
            let resolved = candidate.canonicalize().map_err(|error| {
                ToolError::permission_denied(format!(
                    "[shell.readonly.operand.unresolved] Could not prove absolute read-only operand {value:?} stays inside the workspace because it could not be resolved: {error}"
                ))
            })?;
            if !resolved.starts_with(&workspace) {
                return Err(ToolError::permission_denied(format!(
                    "[shell.readonly.operand.outside_workspace] Read-only Scout shell operand {value:?} resolves outside the workspace. Use the bounded File read/search actions for project evidence."
                )));
            }
            continue;
        }

        if value.starts_with('~')
            || value.contains('\\')
            || windows_prefixed
            || candidate.has_root()
            || candidate
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ToolError::permission_denied(format!(
                "[shell.readonly.operand.shape] Read-only Scout shell operands must stay inside the workspace; refused {value:?}. Use the bounded File read/search actions for project evidence."
            )));
        }

        let joined = effective_cwd.join(candidate);
        if joined.exists() {
            let resolved = joined.canonicalize().map_err(|error| {
                ToolError::permission_denied(format!(
                    "[shell.readonly.operand.unresolved] Could not prove read-only operand {value:?} stays in the workspace: {error}"
                ))
            })?;
            if !resolved.starts_with(&workspace) {
                return Err(ToolError::permission_denied(format!(
                    "[shell.readonly.operand.outside_workspace] Read-only Scout shell operand {value:?} resolves outside the workspace. Use the bounded File read/search actions for project evidence."
                )));
            }
        }
    }
    Ok(())
}

fn readonly_sanitized_path_from(
    workspace: &std::path::Path,
    path: &std::ffi::OsStr,
) -> Option<std::ffi::OsString> {
    let workspace = workspace.canonicalize().ok()?;
    let safe = std::env::split_paths(path).filter_map(|entry| {
        if !entry.is_absolute() {
            return None;
        }
        let resolved = entry.canonicalize().ok()?;
        (!resolved.starts_with(&workspace)).then_some(resolved)
    });
    std::env::join_paths(safe).ok()
}

fn readonly_sanitized_path(workspace: &std::path::Path) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    readonly_sanitized_path_from(workspace, &path).map(|value| value.to_string_lossy().into_owned())
}

fn resolve_readonly_program(program: &str, workspace: &std::path::Path) -> Result<PathBuf> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| anyhow!("no executable search path is configured"))?;
    resolve_readonly_program_from_path(program, workspace, &path)
}

fn resolve_readonly_program_from_path(
    program: &str,
    workspace: &std::path::Path,
    path: &std::ffi::OsStr,
) -> Result<PathBuf> {
    let workspace = workspace.canonicalize()?;
    if std::path::Path::new(program).components().count() != 1 {
        return Err(anyhow!(
            "read-only command must name a bare allowlisted executable"
        ));
    }
    let safe_path = readonly_sanitized_path_from(&workspace, path).ok_or_else(|| {
        anyhow!("no trusted executable search path remains outside the workspace")
    })?;
    let names = if cfg!(windows) {
        vec![format!("{program}.exe"), format!("{program}.com")]
    } else {
        vec![program.to_string()]
    };
    for directory in std::env::split_paths(&safe_path) {
        for name in &names {
            let candidate = directory.join(name);
            if !candidate.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if candidate.metadata()?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            let resolved = candidate.canonicalize()?;
            if resolved.is_absolute() && !resolved.starts_with(&workspace) {
                return Ok(resolved);
            }
        }
    }
    Err(anyhow!(
        "allowlisted read-only executable {program:?} was not found at a canonical path outside the workspace"
    ))
}

fn remove_readonly_redirect_env(cmd: &mut Command, env: &HashMap<String, String>) {
    if env.get(READONLY_ENV_MARKER).map(String::as_str) != Some("1") {
        return;
    }
    cmd.env_remove(READONLY_ENV_MARKER);
    let removals = cmd
        .get_envs()
        .filter_map(|(key, _)| {
            let upper = key.to_string_lossy().to_ascii_uppercase();
            let guarded = upper.starts_with("GIT_")
                || upper.starts_with("GH_")
                || upper.starts_with("GITHUB_");
            let safe = matches!(
                upper.as_str(),
                "GIT_OPTIONAL_LOCKS"
                    | "GIT_NO_LAZY_FETCH"
                    | "GIT_PAGER"
                    | "GIT_CONFIG_NOSYSTEM"
                    | "GIT_CONFIG_GLOBAL"
                    | "GIT_CONFIG_PARAMETERS"
                    | "GIT_EXTERNAL_DIFF"
                    | "GIT_ATTR_NOSYSTEM"
                    | "GIT_CONFIG_COUNT"
                    | "GH_PAGER"
                    | "GH_PROMPT_DISABLED"
                    | "GH_NO_UPDATE_NOTIFIER"
                    | "GH_HOST"
                    | "GH_REPO"
            ) || upper.starts_with("GIT_CONFIG_KEY_")
                || upper.starts_with("GIT_CONFIG_VALUE_");
            (guarded && !safe).then(|| key.to_os_string())
        })
        .collect::<Vec<_>>();
    for key in removals {
        cmd.env_remove(key);
    }
}

fn exec_shell_input_starts_detached(input: &serde_json::Value) -> bool {
    input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && input
            .get("interactive")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        && (input.get("background").and_then(serde_json::Value::as_bool) == Some(true)
            || input.get("tty").and_then(serde_json::Value::as_bool) == Some(true))
}

fn persistent_services_enabled_for(context: &ToolContext) -> bool {
    #[cfg(unix)]
    {
        context.persist_services_enabled
            && context.owner_agent_id.is_none()
            && context.tool_authority.is_none()
            && context.sandbox_backend.is_none()
            && matches!(context.shell_policy, ShellPolicy::Full)
            && matches!(
                context.elevated_sandbox_policy,
                Some(ExecutionSandboxPolicy::DangerFullAccess)
            )
    }
    #[cfg(not(unix))]
    {
        let _ = context;
        false
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_foreground_via_background(
    context: &ToolContext,
    command: &str,
    heavy_permit: Option<HeavyCommandPermit>,
    working_dir: Option<String>,
    timeout_ms: Option<u64>,
    stdin_data: Option<&str>,
    tty: bool,
    policy_override: Option<ExecutionSandboxPolicy>,
    extra_env: HashMap<String, String>,
    direct_argv: bool,
    timeout_bounds_ms: (u64, u64),
    wants_receipt: bool,
    receipt_identity: &mut Option<ShellExecutionIdentity>,
) -> Result<ShellResult> {
    let timeout_ms =
        timeout_ms.map(|timeout| timeout.clamp(timeout_bounds_ms.0, timeout_bounds_ms.1));
    let spawn_timeout_ms = timeout_ms.unwrap_or(timeout_bounds_ms.1);
    // Freeze the receipt's directory before execution and hand that same
    // resolved spelling to the OS. Looking up the original symlink after
    // the command runs can name a different directory than the one it used.
    // Resolution is asynchronous; failure leaves the ordinary execution
    // path intact but cannot produce an exact receipt.
    let receipt_cwd = if wants_receipt {
        match working_dir.as_deref() {
            Some(cwd) => tokio::fs::canonicalize(cwd)
                .await
                .ok()
                .and_then(|path| path.into_os_string().into_string().ok()),
            None => None,
        }
    } else {
        None
    };
    let working_dir = receipt_cwd.clone().or(working_dir);
    let (task_id, detach) = {
        let mut manager = context
            .shell_manager
            .lock()
            .map_err(|_| anyhow!("shell manager lock poisoned"))?;
        let owner = shell_job_owner_from_context(context);
        let lifecycle = shell_work_lifecycle_from_context(context);
        let spawned = manager.execute_with_options_env_for_owner_and_work(
            command,
            working_dir.as_deref(),
            spawn_timeout_ms,
            true,
            stdin_data,
            tty,
            policy_override,
            extra_env,
            owner,
            context.state_namespace.clone(),
            context.origin_tool_call_id.clone(),
            context.origin_turn_id.clone(),
            lifecycle,
            direct_argv.then_some(context.workspace.as_path()),
            false,
            timeout_bounds_ms,
        )?;
        let task_id = spawned
            .task_id
            .ok_or_else(|| anyhow!("foreground shell did not return a process id"))?;
        // Classify before releasing the manager lock: even an immediately
        // completed foreground command must never look like background work.
        let process = manager
            .processes
            .get_mut(&task_id)
            .ok_or_else(|| anyhow!("foreground shell {task_id} is not tracked"))?;
        process.background = false;
        // #6689: the receipt identity is what the manager recorded for this
        // spawn — the admitted command and the directory handed to the OS —
        // never the before-hook request. Only pipe-backed, unsandboxed local
        // runs through a POSIX-style `<shell> <flag> <source>` dispatcher
        // qualify: a PTY, the hardened read-only argv rewrite, an OS sandbox
        // wrapper, Windows shell prefixes, and a PowerShell dispatcher (even
        // `$SHELL=pwsh` on Unix, which wraps the source or runs it from a temp
        // `-File`) all change what the process executes relative to this string.
        if receipt_cwd.is_some()
            && !cfg!(windows)
            && !tty
            && !direct_argv
            && !spawned.sandboxed
            && !spawned.sandbox_denied
            && !codewhale_sandbox::shell_dispatcher::global_dispatcher()
                .kind()
                .is_powershell()
        {
            *receipt_identity = Some(ShellExecutionIdentity {
                command: process.command.clone(),
                cwd: process.working_dir.clone(),
            });
        }
        let detach = manager.register_shell_wait(&context.state_namespace);
        (task_id, detach)
    };
    let mut foreground = ForegroundShellGuard {
        manager: context.shell_manager.clone(),
        task_id: task_id.clone(),
        armed: true,
    };
    if let Some(permit) = heavy_permit {
        let mut manager = context
            .shell_manager
            .lock()
            .map_err(|_| anyhow!("shell manager lock poisoned"))?;
        manager.attach_heavy_permit(&task_id, permit)?;
    }

    // A foreground pipe command gets EOF on stdin: an unexpected read (`cat`,
    // `read x`, a confirmation prompt) then fails at once instead of blocking
    // until the timeout kills it. A TTY keeps its terminal input. A command
    // later moved to /jobs keeps the closed stdin; interactive input needs
    // `background: true` from the start.
    if stdin_data.is_some() || !tty {
        let mut manager = context
            .shell_manager
            .lock()
            .map_err(|_| anyhow!("shell manager lock poisoned"))?;
        manager.write_stdin(&task_id, "", true)?;
    }

    let deadline = timeout_ms.map(|timeout| Instant::now() + Duration::from_millis(timeout));
    // Adaptive poll cadence: fast commands (the common case — grep, wc, echo)
    // finish in single-digit milliseconds, and a fixed 100ms tick made every
    // foreground call pay that full quantum before completion was noticed.
    // Start fine-grained and back off to the 100ms cap for long-running work.
    let mut poll_tick_ms: u64 = FOREGROUND_POLL_INITIAL_MS;
    loop {
        if context
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| anyhow!("shell manager lock poisoned"))?;
            let result = manager.kill(&task_id);
            if result.is_ok() {
                manager.acknowledge_foreground_completion(&task_id);
                foreground.armed = false;
            }
            return result;
        }

        // Poll status only. The snapshot — and the buffer clones behind it — is
        // built once, when there is actually a result to return (#5472). Both
        // happen under one lock acquisition so the record cannot be evicted
        // between observing that it finished and reading its result.
        let finished = {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| anyhow!("shell manager lock poisoned"))?;
            if detach.load(Ordering::Acquire) {
                if let Some(shell) = manager.processes.get_mut(&task_id) {
                    shell.background = true;
                }
                let snapshot = manager.get_output(&task_id, false, 0)?;
                foreground.armed = false;
                return Ok(snapshot);
            }
            if manager.poll_status(&task_id)? == ShellStatus::Running {
                None
            } else {
                // #6689: a status from a failed wait is neither an observed
                // exit nor an interruption, so the receipt is left out rather
                // than reporting a guessed state.
                if manager
                    .processes
                    .get(&task_id)
                    .is_none_or(|shell| shell.wait_failed)
                {
                    *receipt_identity = None;
                }
                let snapshot = manager.get_output(&task_id, false, 0)?;
                // Ordering matters: the snapshot is taken before the
                // acknowledgement releases the retained bytes.
                manager.acknowledge_foreground_completion(&task_id);
                Some(snapshot)
            }
        };

        if let Some(snapshot) = finished {
            foreground.armed = false;
            return Ok(snapshot);
        }

        // The foreground budget is how long the turn waits, never how long
        // the command may live. Past it the process keeps running as a
        // background job — exactly as Ctrl+B would move it — and the model
        // gets the output so far plus the job id to wait on, read, or cancel.
        // Killing here threw away minutes of a build or test run and made the
        // model start it over.
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| anyhow!("shell manager lock poisoned"))?;
            if let Some(process) = manager.processes.get_mut(&task_id) {
                process.background = true;
            }
            let snapshot = manager.get_output(&task_id, false, 0)?;
            foreground.armed = false;
            return Ok(snapshot);
        }

        tokio::time::sleep(Duration::from_millis(poll_tick_ms)).await;
        poll_tick_ms = (poll_tick_ms * 2).min(FOREGROUND_POLL_MAX_MS);
    }
}

/// A foreground wait owns its process even if its caller drops the future
/// before cooperative cancellation can be polled. Only an explicit transfer
/// to /jobs releases that ownership while the process is still running.
struct ForegroundShellGuard {
    manager: SharedShellManager,
    task_id: String,
    armed: bool,
}

impl Drop for ForegroundShellGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut manager = self
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = manager.poll_status(&self.task_id).and_then(|status| {
            if status == ShellStatus::Running {
                manager.kill(&self.task_id).map(|_| ())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            tracing::warn!(shell_id = %self.task_id, %error, "foreground shell cleanup failed");
        }
        if let Some(shell) = manager.processes.get_mut(&self.task_id) {
            // No result received these bytes. Retain them for /jobs inspection,
            // but do not let an abandoned foreground wait wake a new model turn.
            shell.completion_reported = true;
        }
    }
}

/// The admitted command and working directory a foreground spawn recorded,
/// for the `tool_call_after` execution receipt (#6689).
struct ShellExecutionIdentity {
    command: String,
    cwd: PathBuf,
}

/// Largest command or working directory a receipt carries. Identities are
/// exact or absent, never truncated.
const EXECUTION_RECEIPT_IDENTITY_MAX_BYTES: usize = 8 * 1024;

/// Starting per-stream output preview in an execution receipt. Halved until
/// the serialized receipt fits `HOOK_EXECUTION_RECEIPT_MAX_BYTES`.
const EXECUTION_RECEIPT_PREVIEW_MAX_BYTES: usize = 8 * 1024;

/// Keep both ends of an output stream — the first lines and the final
/// diagnostic — cut on UTF-8 boundaries with a visible marker.
fn execution_receipt_preview(text: &str, budget: usize) -> (String, bool) {
    if text.len() <= budget {
        return (text.to_owned(), false);
    }
    let mut head = budget / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - budget / 2;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    (
        format!(
            "{}\n[receipt preview truncated]\n{}",
            &text[..head],
            &text[tail..]
        ),
        true,
    )
}

/// Build the schema-1 execution receipt for a settled foreground shell run
/// (#6689), exported to `tool_call_after` hooks as
/// `DEEPSEEK_TOOL_EXECUTION_RECEIPT`.
///
/// Returns `None` — absence, which implies neither success nor failure — for
/// a run still in flight, a sandboxed run, or an identity that is not exact:
/// empty, over the identity bound, containing NUL, or a relative or non-UTF-8
/// directory. `output_kind` is `"combined"` when stdout and stderr shared one
/// pipe, so `stdout` holds the combined preview.
fn shell_execution_receipt(
    identity: &ShellExecutionIdentity,
    result: &ShellResult,
    output_kind: &'static str,
) -> Option<serde_json::Value> {
    let command = identity.command.as_str();
    let cwd = identity.cwd.to_str()?;
    if command.is_empty()
        || command.len() > EXECUTION_RECEIPT_IDENTITY_MAX_BYTES
        || command.contains('\0')
        || cwd.len() > EXECUTION_RECEIPT_IDENTITY_MAX_BYTES
        || cwd.contains('\0')
        || !identity.cwd.is_absolute()
        || result.sandboxed
        || result.sandbox_denied
    {
        return None;
    }
    // A nonzero exit is still a completed run; `interrupted` is a signal,
    // kill, cancel, or timeout. The exit code is only ever the observed one.
    let state = match result.status {
        ShellStatus::Completed => "completed",
        ShellStatus::Failed if result.exit_code.is_some() => "completed",
        ShellStatus::Failed | ShellStatus::Killed | ShellStatus::TimedOut => "interrupted",
        ShellStatus::Running => return None,
    };
    let mut budget = EXECUTION_RECEIPT_PREVIEW_MAX_BYTES;
    loop {
        let (stdout, stdout_clipped) = execution_receipt_preview(&result.stdout, budget);
        let (stderr, stderr_clipped) = execution_receipt_preview(&result.stderr, budget);
        let receipt = json!({
            "schema_version": 1,
            "command": command,
            "cwd": cwd,
            "state": state,
            "scope": "local",
            "exit_code": result.exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": result.stdout_truncated || stdout_clipped,
            "stderr_truncated": result.stderr_truncated || stderr_clipped,
            "output_kind": output_kind,
        });
        // The bound is on the serialized form, so JSON escaping counts.
        if serde_json::to_vec(&receipt).ok()?.len()
            <= crate::hooks::HOOK_EXECUTION_RECEIPT_MAX_BYTES
        {
            return Some(receipt);
        }
        if budget == 0 {
            return None;
        }
        budget /= 2;
    }
}

const BASH_MAX_TIMEOUT_MS: u64 = i32::MAX as u64;

/// Initial cadence for foreground-via-background completion polling. Fast
/// commands dominate real agent traffic; detection latency on `true`-class
/// commands drops from ~100ms to ~10ms, while long commands reach the
/// 100ms cap within one doubling step.
const FOREGROUND_POLL_INITIAL_MS: u64 = 10;
/// Poll-cadence ceiling; matches the previous fixed tick so long-running
/// command overhead is unchanged.
const FOREGROUND_POLL_MAX_MS: u64 = 100;

/// Default foreground lifetime for a contract-`bash` `action=run` that names
/// no `timeout_ms`. Matches the value the tool's own input schema advertises;
/// before this existed the omitted case fell through to
/// `BASH_MAX_TIMEOUT_MS`.
const CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Resolve the lifetime for one `bash` run.
///
/// A foreground contract-`bash` run that names no timeout used to inherit
/// `BASH_MAX_TIMEOUT_MS` (~24.8 days), so a command that blocked on an
/// interactive prompt or a hung network call pinned the turn indefinitely —
/// the tool row just counted seconds while the model waited. The tool's own
/// schema already promises `action=run 30000`, so an omitted timeout takes
/// that default as the foreground wait. Past it the command moves to the
/// background (it is never killed for running long) and the model gets its
/// output so far and task id.
///
/// An explicit `timeout_ms` is still honored up to the full contract ceiling,
/// and background and interactive runs keep their own lifetimes: their
/// processes are meant to outlive the call, so bounding them here would kill
/// long-lived jobs the model deliberately detached.
fn contract_bash_timeout_ms(
    optional_timeout: bool,
    requested_ms: Option<u64>,
    background: bool,
    interactive: bool,
) -> Option<u64> {
    if optional_timeout && requested_ms.is_none() && !background && !interactive {
        return Some(CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS);
    }
    requested_ms
}

fn promotion_notice(
    context: &ToolContext,
    task_id: &str,
    elapsed_seconds: u64,
    output: &str,
) -> String {
    let last_output = context
        .shell_manager
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .last_output_age_ms(task_id)
        .map_or_else(
            || "last output age unknown".to_string(),
            |ms| format!("last output {}s ago", ms / 1_000),
        );
    format!(
        "Still running after {elapsed_seconds}s ({last_output}; {}); moved to the background as {task_id} (not killed).",
        output_phase_hint(output)
    )
}

fn contract_bash_error_status(result: &ShellResult, timeout_ms: Option<u64>) -> String {
    match result.status {
        ShellStatus::TimedOut => {
            let millis = timeout_ms.unwrap_or(BASH_MAX_TIMEOUT_MS);
            let seconds = if millis.is_multiple_of(1_000) {
                (millis / 1_000).to_string()
            } else {
                format!("{}", millis as f64 / 1_000.0)
            };
            format!("Command timed out after {seconds} seconds")
        }
        ShellStatus::Killed => format!(
            "Command aborted. {}",
            kill_outcome_note(&format!("{}{}", result.stdout, result.stderr))
        ),
        ShellStatus::Failed | ShellStatus::Completed | ShellStatus::Running => format!(
            "Command exited with code {}",
            result.exit_code.unwrap_or(-1)
        ),
    }
}

fn finish_contract_bash_result(
    result: ShellResult,
    timeout_ms: Option<u64>,
    context: &ToolContext,
    execution_receipt: Option<serde_json::Value>,
    readonly_network: bool,
    network_restricted_hint: Option<&str>,
) -> Result<ToolResult, ToolError> {
    let sandbox_denied_hint = if network_restricted_hint.is_none() {
        shell_sandbox_denied_hint(context, &result)
    } else {
        None
    };
    let mut output = result.stdout.clone();
    output.push_str(&result.stderr);
    if let Some(hint) = network_restricted_hint {
        output = if output.is_empty() {
            hint.to_string()
        } else {
            format!("{hint}\n\n{output}")
        };
    }
    if let Some(hint) = sandbox_denied_hint {
        output = if output.is_empty() {
            hint
        } else {
            format!("{hint}\n\n{output}")
        };
    }
    let mut metadata = json!({
        "evidence_routing": "inline", "exit_code": result.exit_code,
        "status": format!("{:?}", result.status), "duration_ms": result.duration_ms,
        "sandboxed": result.sandboxed, "sandbox_type": result.sandbox_type,
        "task_id": result.task_id, "backgrounded": result.status == ShellStatus::Running,
    });
    if let Some(receipt) = execution_receipt {
        metadata["execution_receipt"] = receipt;
    }
    if readonly_network {
        metadata["sandbox_network"] = json!(true);
    }
    if let Some(hint) = network_restricted_hint {
        metadata["sandbox_network_restricted"] = json!(true);
        metadata["sandbox_network_denied_hint"] = json!(hint);
    }
    if result.status == ShellStatus::Running {
        let task_id = result.task_id.as_deref().unwrap_or("unknown");
        let so_far = if output.trim().is_empty() {
            "(no output yet)".to_string()
        } else {
            tail_lines(output.trim(), 20)
        };
        let notice = promotion_notice(context, task_id, result.duration_ms / 1_000, &output);
        let completion = if context.owner_agent_id.is_some() {
            "Its completion stays in task/status and is not injected into the parent model."
        } else {
            "Its completion reaches you as a runtime event before your next step; if you end the turn first, a headless run stops it unseen."
        };
        return Ok(ToolResult::success(format!(
            "{notice}\n\nOutput so far:\n{so_far}\n\n{completion} To see more output or block until it finishes, call `task_shell_wait` directly with task_id=\"{task_id}\"; do not sleep-poll with bash. To stop it, send the same call with cancel=true. It also stops when the session ends."
        )).with_metadata(metadata));
    }
    if result.status != ShellStatus::Completed {
        // A nonzero exit, timeout, or kill stays a failed tool call, but it
        // keeps the same metadata a success carries: hooks read `exit_code`
        // and `status` from it, and an error with no metadata left them blind
        // to exactly the commands that failed.
        let status = contract_bash_error_status(&result, timeout_ms);
        return Err(ToolError::execution_failed_with_metadata(
            if output.is_empty() {
                status
            } else {
                format!("{output}\n\n{status}")
            },
            metadata,
        ));
    }

    Ok(ToolResult::success(if output.is_empty() {
        "(no output)".to_string()
    } else {
        output
    })
    .with_metadata(metadata))
}

/// Small foreground-only shell surface shown to new model turns.
pub struct LowercaseBashTool;

#[async_trait]
impl ToolSpec for LowercaseBashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        guidance::foreground_description()
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": guidance::runtime_command_guidance() },
                "description": { "type": "string", "description": "Optional few-word summary shown to the user beside the command." },
                "timeout": { "type": "number", "description": "Optional seconds to wait in the foreground (default 30 seconds). A command still running then is not killed: it moves to the background by itself and you get its output so far, how long ago it last printed, its build phase, and a task_id. Call task_shell_wait directly with that task_id to read more or wait for it; do not sleep-poll with bash." },
                "read_only": { "type": "boolean", "description": "Set true to run analysis code (including Python/SQLite) or a network fetch (curl) with mandatory native filesystem read-only isolation. Outbound network is on only in Full Access when the session sandbox, the network policy and your network grant allow it. Available during peer writes. Refused when native enforcement is unavailable; no background, stdin, external backend, or sandbox escalation." },
                "sandbox_permissions": {
                    "type": "string",
                    "enum": ["workspace-write", "danger-full-access"],
                    "description": "The wider sandbox mode this exact command needs. Use only as a one-shot retry after a sandbox denial; requires justification and user approval in Ask."
                },
                "justification": {
                    "type": "string",
                    "description": "Required with sandbox_permissions: one sentence explaining why this exact command needs wider access."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        BashTool::contract_delegate().capabilities()
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn approval_requirement_for(&self, input: &serde_json::Value) -> ApprovalRequirement {
        let translated = contract_bash_legacy_input(input).unwrap_or_else(|_| input.clone());
        BashTool::contract_delegate().approval_requirement_for(&translated)
    }

    fn is_read_only_for(&self, input: &serde_json::Value) -> bool {
        contract_bash_legacy_input(input)
            .is_ok_and(|translated| BashTool::contract_delegate().is_read_only_for(&translated))
    }

    fn supports_parallel_for(&self, input: &serde_json::Value) -> bool {
        self.is_read_only_for(input)
    }

    fn refusal_before_approval(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
    ) -> Option<ToolResult> {
        let translated = contract_bash_legacy_input(input).ok()?;
        BashTool::contract_delegate().refusal_before_approval(&translated, context)
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let translated = contract_bash_legacy_input(&input)?;
        BashTool::contract_delegate()
            .execute(translated, context)
            .await
    }
}

fn dangerous_command_block(safety: &SafetyAnalysis) -> Option<ToolResult> {
    if safety.level != SafetyLevel::Dangerous {
        return None;
    }
    let reasons = safety.reasons.join("; ");
    let suggestions = if safety.suggestions.is_empty() {
        String::new()
    } else {
        format!("\nSuggestions: {}", safety.suggestions.join("; "))
    };
    Some(ToolResult {
        content: format!(
            "BLOCKED: This command was blocked for safety reasons.\n\nReasons: {reasons}{suggestions}\n\nNote: allow_shell=true exposes shell tools, but it does not disable built-in shell safety validation."
        ),
        success: false,
        metadata: Some(json!({
            "safety_level": "dangerous",
            "blocked": true,
            "reasons": safety.reasons,
            "suggestions": safety.suggestions,
        })),
    })
}

fn contract_bash_legacy_input(input: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::invalid_input("bash input must be an object"))?;
    let unexpected = object
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "command"
                    | "description"
                    | "timeout"
                    | "read_only"
                    | "sandbox_permissions"
                    | "justification"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "unexpected bash parameter(s): {}",
            unexpected.join(", ")
        )));
    }
    let command = required_str(input, "command")?;
    optional_str(input, "description")?;
    let mut translated = json!({"command": command});
    if let Some(timeout) = input.get("timeout") {
        let seconds = timeout.as_f64().ok_or_else(|| {
            ToolError::invalid_input("Invalid timeout: expected a finite number of seconds")
        })?;
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(ToolError::invalid_input(
                "Invalid timeout: expected a positive finite number of seconds",
            ));
        }
        let millis = seconds * 1000.0;
        if millis > BASH_MAX_TIMEOUT_MS as f64 {
            return Err(ToolError::invalid_input(format!(
                "Invalid timeout: maximum is {} seconds",
                BASH_MAX_TIMEOUT_MS as f64 / 1000.0
            )));
        }
        translated["timeout_ms"] = json!((millis as u64).max(1));
    }
    if let Some(value) = input.get("read_only") {
        if !value.is_boolean() {
            return Err(type_mismatch("read_only", value, "a boolean"));
        }
        translated["read_only"] = value.clone();
    }
    for field in ["sandbox_permissions", "justification"] {
        if let Some(value) = input.get(field) {
            translated[field] = value.clone();
        }
    }
    Ok(translated)
}

/// Compatibility shell tool retained for saved v0.9.x transcripts and the
/// background/session control surface. It is hidden from new model catalogs.
pub struct BashTool {
    name: &'static str,
    forced_action: Option<&'static str>,
    read_only: bool,
    optional_timeout: bool,
}

pub fn readonly_bash_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["run"] },
            "command": { "type": "string", "description": "A classifier-approved read command, or analysis code with read_only=true" },
            "read_only": { "type": "boolean", "description": "Require native filesystem read-only enforcement for analysis code or a network fetch (curl); outbound network is on only in Full Access when the session sandbox, the network policy and your network grant allow it. Unavailable sandboxes fail closed." },
            "cwd": { "type": "string", "description": "Workspace-relative working directory" },
            "timeout_ms": { "type": "integer", "description": "Foreground wait in milliseconds (1000-600000). A command still running then moves to the background; it is not killed." }
        },
        "required": ["command"],
        "additionalProperties": false
    })
}

impl BashTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
            read_only: false,
            optional_timeout: false,
        }
    }

    pub const fn read_only(name: &'static str) -> Self {
        Self {
            name,
            forced_action: None,
            read_only: true,
            optional_timeout: false,
        }
    }

    pub const fn alias(name: &'static str, action: &'static str) -> Self {
        Self {
            name,
            forced_action: Some(action),
            read_only: false,
            optional_timeout: false,
        }
    }

    const fn contract_delegate() -> Self {
        Self {
            name: "bash",
            forced_action: Some("run"),
            read_only: false,
            optional_timeout: true,
        }
    }
}

#[async_trait]
impl ToolSpec for BashTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        if self.read_only {
            "Inspect with classifier-bounded commands run directly as argv, never through a shell. For analysis code or a network fetch (curl), read_only=true instead requires a native filesystem read-only sandbox, with outbound network only in Full Access when the session sandbox, the network policy and your network grant allow it. Only foreground action=run with command, cwd, timeout_ms, and read_only is accepted."
        } else {
            guidance::description()
        }
    }

    fn input_schema(&self) -> serde_json::Value {
        if self.read_only {
            return readonly_bash_input_schema();
        }
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "wait", "interact", "cancel"],
                    "description": "Action to perform (default: run)"
                },
                "command": {
                    "type": "string",
                    "description": guidance::runtime_command_guidance()
                },
                "read_only": { "type": "boolean", "description": "Set true to require native filesystem read-only execution, with outbound network only in Full Access when the session sandbox, the network policy and your network grant allow it. Only foreground run with command, cwd and timeout_ms; unavailable enforcement fails closed." },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait, in milliseconds. action=run: how long the turn waits in the foreground (default 30000, max 600000); a command still running then is NOT killed — it moves to the background by itself and you get its output so far, how long ago it last printed, its build phase, and task_id. action=wait 30000, action=interact 1000. For action=wait, `timeout_secs` (seconds) and `timeout` (milliseconds) are accepted aliases."
                },
                "background": {
                    "type": "boolean",
                    "description": "Temporary background; killed at session exit. Surviving headless services need background:true,persist:true. It is not killed at timeout_ms; plan to poll it with action=wait or stop it with action=cancel."
                },
                "interactive": {
                    "type": "boolean",
                    "description": "Run interactively with terminal IO (default: false)"
                },
                "stdin": {
                    "type": "string",
                    "description": "Stdin data to send (action=run: before waiting; action=interact: to the background task). Also accepted as `input` or `data` — send only one."
                },
                "input": {
                    "type": "string",
                    "description": "Alias for `stdin`."
                },
                "data": {
                    "type": "string",
                    "description": "Alias for `stdin`."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the command"
                },
                "tty": {
                    "type": "boolean",
                    "description": "Allocate a pseudo-terminal for interactive programs (implies background)"
                },
                "combined_output": {
                    "type": "boolean",
                    "description": "Capture stdout and stderr as one chronological PTY stream (default false)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID for action=wait/interact/cancel. Also accepted as `id`."
                },
                "task_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "For action=wait: wait on several background task ids at once (alternative to `task_id`)."
                },
                "until": {
                    "type": "string",
                    "enum": ["any", "all"],
                    "description": "For action=wait with `task_ids`: return as soon as any task is terminal (`any`) or when every task is terminal (`all`, default)."
                },
                "id": {
                    "type": "string",
                    "description": "Alias for `task_id`."
                },
                "wait": {
                    "type": "boolean",
                    "description": "For action=wait, block until the task completes or timeout elapses (default: true). Pass false for a nonblocking snapshot; `block` is an accepted alias."
                },
                "close_stdin": {
                    "type": "boolean",
                    "description": "Close stdin after sending (action=interact)"
                },
                "all": {
                    "type": "boolean",
                    "description": "Cancel all running background tasks (action=cancel)"
                },
                "persist": {
                    "type": "boolean",
                    "description": "Keep this background service running after a successful headless exec (default: false). Requires background:true and explicit danger-full-access. Run the service itself in the foreground; do not use nohup or a trailing `&`."
                },
                "sandbox_permissions": {
                    "type": "string",
                    "enum": ["workspace-write", "danger-full-access"],
                    "description": "The wider sandbox mode this exact command needs. Use only as a one-shot retry after a sandbox denial; requires justification and user approval in Ask."
                },
                "justification": {
                    "type": "string",
                    "description": "Required with sandbox_permissions: one sentence explaining why this exact command needs wider access."
                }
            },
            // The schema used to declare nothing required at all, so
            // `Bash{}` was schema-valid for the tool that runs shell
            // commands. What is required is per-action and cannot be spelled
            // as a flat `required` list: `run` needs `command`,
            // `wait`/`interact`/`cancel` need `task_id` (or its `id` alias),
            // and `cancel` needs `all` instead when cancelling everything.
            // A root `anyOf` of `required` groups is how this repo already
            // spells that (`finance`, `apply_patch`), and `schema_sanitize`
            // knows the shape: providers that reject root composition get the
            // groups merged and the constraint restated as a description note
            // (`root_composition_constraint_note`). The cost is that
            // `strict_schema_supported` rejects a root `anyOf`, so `Bash`
            // opts out of DeepSeek strict mode — as `finance` already does on
            // the same default agent surface, which turns strict mode off for
            // the whole tool set regardless.
            "anyOf": [
                { "required": ["command"] },
                { "required": ["task_id"] },
                { "required": ["id"] },
                { "required": ["all"] }
            ]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::Sandboxable,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn approval_requirement_for(&self, input: &serde_json::Value) -> ApprovalRequirement {
        if exec_shell_input_is_parallel_readonly(input) {
            ApprovalRequirement::Auto
        } else {
            self.approval_requirement()
        }
    }

    fn is_read_only_for(&self, input: &serde_json::Value) -> bool {
        exec_shell_input_is_parallel_readonly(input)
    }

    fn supports_parallel_for(&self, input: &serde_json::Value) -> bool {
        exec_shell_input_is_parallel_readonly(input)
    }

    fn starts_detached_for(&self, input: &serde_json::Value) -> bool {
        exec_shell_input_starts_detached(input)
    }

    fn refusal_before_approval(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
    ) -> Option<ToolResult> {
        if context.auto_approve {
            return None;
        }
        let input = if context.shell_policy == ShellPolicy::ReadOnly {
            normalize_readonly_cd(input)
        } else {
            input.clone()
        };
        let action = match self.forced_action {
            Some(forced) => forced,
            None => optional_str(&input, "action").ok()?.unwrap_or("run"),
        };
        if action != "run" {
            return None;
        }
        let command = input.get("command")?.as_str()?;
        dangerous_command_block(&analyze_command(command))
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // A leading `cd <dir> &&` becomes the working directory before any
        // check, exactly as the read-only gates judged it.
        let input = if context.shell_policy == ShellPolicy::ReadOnly {
            normalize_readonly_cd(&input)
        } else {
            input
        };
        // `and_then(as_str).unwrap_or("run")` treated *any* non-string
        // `action` as absent and fell through to the branch that runs
        // arbitrary code: `Bash{action: 3, command: "…"}` executed the
        // command. Every sibling family refuses a non-string action
        // (`codewhale_tools::required_action`), and `Bash` cannot be the
        // lenient one. `optional_str` is the type-strictness lane's extractor:
        // absent or `null` takes the documented `run` default, anything else
        // is a `type_mismatch` naming the field and the type it needed.
        let action = match self.forced_action {
            Some(forced) => forced,
            None => optional_str(&input, "action")?.unwrap_or("run"),
        };
        let enforced_readonly = match input.get("read_only") {
            None => false,
            Some(serde_json::Value::Bool(enabled)) => *enabled,
            Some(value) => return Err(type_mismatch("read_only", value, "a boolean")),
        };
        if enforced_readonly && (!enforced_readonly_input(&input) || action != "run") {
            return Err(ToolError::invalid_input(
                "read_only=true accepts only foreground run with command, cwd and timeout_ms; background, input, interactive modes and sandbox escalation are incompatible",
            ));
        }
        if enforced_readonly {
            if context.sandbox_backend.is_some() {
                return Err(ToolError::permission_denied(
                    "read_only execution requires a native enforcing sandbox; external backends cannot attest this policy",
                ));
            }
            let manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            if !manager
                .configured_sandbox_type()
                .is_some_and(is_native_readonly_sandbox)
            {
                return Err(ToolError::not_available(
                    "read_only execution requires native read-only enforcement (macOS Seatbelt or configured Linux bubblewrap); nothing was run",
                ));
            }
        }
        let mut policy_input = input.clone();
        if let Some(object) = policy_input.as_object_mut() {
            object.insert("action".into(), json!(action));
        }
        crate::tools::execution_envelope::enforce_tool_denial(context, self.name(), &policy_input)?;
        if action == "interact" && context.shell_policy != ShellPolicy::Full {
            return Err(ToolError::permission_denied(
                "Sending shell input requires full shell permission.",
            ));
        }
        match action {
            "wait" => return self.execute_wait(&input, context).await,
            "interact" => return self.execute_interact(&input, context).await,
            "cancel" => return self.execute_cancel(&input, context).await,
            "run" => {}
            // Bash was the only action wrapper whose catch-all fell through to
            // its most dangerous branch: `{"action":"kill", "command":…}` ran
            // the command instead of cancelling, and a mis-cased "Cancel" did
            // the same. Every sibling (`File`, `Git`, `Web`, `Run`) already
            // refuses an unknown action; the tool that executes arbitrary code
            // should not be the lenient one.
            other => {
                return Err(ToolError::invalid_input(format!(
                    "Unknown Bash action \"{other}\"; nothing was run. Pass one of: run, wait, interact, cancel."
                )));
            }
        }
        let command = require_no_nul(required_str(&input, "command")?, "command")?;
        match context.shell_policy {
            ShellPolicy::None => {
                return Ok(ToolResult::error(
                    "Shell tools are disabled by the active permission profile.",
                ));
            }
            ShellPolicy::ReadOnly => {
                if let Err(rejection) = exec_shell_input_agent_readonly_verdict(&input) {
                    // A typed denial, so a Fleet worker's no-progress guard
                    // counts it. #6298: an agent has no mode to switch to,
                    // so it gets the same next steps as the other read-only
                    // gates; only a parent session is told the user can
                    // change modes.
                    let message = if context.owner_agent_id.is_some()
                        || context.tool_authority.is_some()
                    {
                        readonly_refusal(&rejection, readonly_enforced_lane_available(context))
                    } else {
                        format!(
                            "{rejection}. This shell admits read-only inspection commands only. The user can change modes with /mode."
                        )
                    };
                    return Err(ToolError::permission_denied(message));
                }
            }
            ShellPolicy::Full => {}
        }
        enforce_readonly_network_reads(command, context)?;
        let requested_timeout_ms = if self.optional_timeout {
            input
                .get("timeout_ms")
                .map(|value| {
                    value
                        .as_u64()
                        .ok_or_else(|| type_mismatch("timeout_ms", value, "a positive integer"))
                })
                .transpose()?
        } else {
            let detached = optional_bool(&input, "background", false)?
                || optional_bool(&input, "interactive", false)?;
            let default_ms = if detached {
                120_000
            } else {
                CONTRACT_BASH_FOREGROUND_DEFAULT_TIMEOUT_MS
            };
            Some(optional_u64(&input, "timeout_ms", default_ms)?.min(600_000))
        };
        let background = optional_bool(&input, "background", false)?;
        let interactive = optional_bool(&input, "interactive", false)?;
        let combined_output = optional_bool(&input, "combined_output", false)?;
        let tty = optional_bool(&input, "tty", false)? || (combined_output && background);
        let timeout_ms = contract_bash_timeout_ms(
            self.optional_timeout,
            requested_timeout_ms,
            background,
            interactive,
        );
        let timeout_value_ms = timeout_ms.unwrap_or(BASH_MAX_TIMEOUT_MS);
        // Strict types (2026-08-04 review): a non-string here used to be
        // silently dropped — the command then ran with NO stdin and reported
        // success, the exact silent-drop failure the alias hardening closed
        // for misspelled names. A wrong type is an error, never a no-op.
        let stdin_data = match first_present_field(&input, &["stdin", "input", "data"]) {
            None => None,
            Some((name, value)) => Some(
                value
                    .as_str()
                    .ok_or_else(|| type_mismatch(name, value, "a string"))?
                    .to_string(),
            ),
        };

        if interactive && background {
            return Ok(ToolResult::error(
                "Interactive commands cannot run in background mode.",
            ));
        }
        if interactive && (tty || combined_output) {
            return Ok(ToolResult::error(
                "Interactive mode cannot be combined with TTY or combined_output sessions.",
            ));
        }
        if interactive && stdin_data.is_some() {
            return Ok(ToolResult::error(
                "Interactive mode cannot be combined with stdin data.",
            ));
        }

        let persist = optional_bool(&input, "persist", false)?;
        if persist {
            if !background {
                return Err(ToolError::invalid_input(
                    "persist:true requires background:true; a persisted service must be started as a background task.",
                ));
            }
            if interactive || tty {
                return Err(ToolError::invalid_input(
                    "persist:true cannot be combined with interactive or TTY modes.",
                ));
            }
            if stdin_data.is_some() {
                return Err(ToolError::invalid_input(
                    "persist:true spawns the service with null stdio; stdin data is not accepted.",
                ));
            }
            if !persistent_services_enabled_for(context) {
                return Err(ToolError::not_available(
                    "persistent background services (persist:true) are only available on Unix in the real headless `codewhale exec` host under an explicit danger-full-access / full shell authority. They are rejected in interactive sessions, desktop/app-server hosts, Fleet/sub-agents, restricted or external sandboxes, and TTY/interactive/stdin modes.",
                ));
            }
        }

        let background = background || tty;

        let mut execpolicy_decision: Option<RuleDecision> = None;
        if context.features.enabled(Feature::ExecPolicy)
            && let Some(policy) = tokio::task::spawn_blocking(load_default_policy)
                .await
                .map_err(|e| {
                    ToolError::execution_failed(format!("execpolicy load task failed: {e}"))
                })?
                .map_err(|e| ToolError::execution_failed(format!("execpolicy load failed: {e}")))?
        {
            let decision = policy.evaluate(command);
            execpolicy_decision = Some(decision.clone());
            if let RuleDecision::Deny(reason) = decision {
                return Ok(ToolResult {
                    content: format!("BLOCKED: {reason}"),
                    success: false,
                    metadata: Some(json!({
                        "execpolicy": {
                            "decision": "deny",
                            "reason": reason,
                        }
                    })),
                });
            }
        }

        // Safety analysis (always run for metadata, but only block when not in YOLO mode)
        let safety = analyze_command(command);
        if !context.auto_approve
            && let Some(blocked) = dangerous_command_block(&safety)
        {
            return Ok(blocked);
        }

        // This explicit mode only narrows the caller's posture. No approval or
        // inherited full-access override can disable the required sandbox.
        let readonly_network = enforced_readonly && readonly_lane_network_access(context);
        let policy_override = if enforced_readonly {
            Some(ExecutionSandboxPolicy::ReadOnly {
                network_access: readonly_network,
            })
        } else {
            context.elevated_sandbox_policy.clone()
        };
        // Strict types: a non-string cwd used to silently run the command in
        // the workspace default instead of erroring (2026-08-04 review).
        let working_dir = match first_present_field(&input, &["cwd", "working_dir"])
            .map(|(name, value)| {
                value
                    .as_str()
                    .ok_or_else(|| type_mismatch(name, value, "a string"))
                    .and_then(|dir| require_no_nul(dir, name))
            })
            .transpose()?
        {
            Some(dir) => {
                // Validate cwd against workspace boundary (same as file tools)
                let resolved = context.resolve_path(dir)?;
                Some(resolved.to_string_lossy().to_string())
            }
            // Default to the tool context's workspace (which reflects the
            // child agent's worktree when `worktree: true` was used), not the
            // shared ShellManager's parent-workspace default_workspace.
            None => Some(context.workspace.display().to_string()),
        };
        if matches!(context.shell_policy, ShellPolicy::ReadOnly) && !enforced_readonly {
            let effective_cwd = working_dir
                .as_deref()
                .map(std::path::Path::new)
                .unwrap_or(&context.workspace);
            enforce_readonly_workspace_operands(command, &context.workspace, effective_cwd)?;
        }

        // #456 — collect env from any configured `shell_env` hooks. Runs
        // synchronously, captures stdout, parses `KEY=VAL` lines, audit-logs
        // the keys (never the values). Empty / no-op when no hook is
        // configured.
        let read_only_shell =
            matches!(context.shell_policy, ShellPolicy::ReadOnly) || enforced_readonly;
        let mut extra_env = if read_only_shell {
            // shell_env hooks are arbitrary operator-configured processes.
            // They cannot run inside the evidence-only execution boundary.
            HashMap::new()
        } else if let Some(hook_executor) = &context.runtime.hook_executor {
            let hook_ctx = crate::hooks::HookContext::new()
                .with_tool_context(context)
                .with_tool_name("exec_shell")
                .with_tool_args(&input);
            let executor = Arc::clone(hook_executor);
            let policy = codewhale_plugins::activation::extension_host_policy_enabled();
            #[cfg(any(test, feature = "test-support"))]
            let env_scope = crate::test_support::env_scope_ticket();
            tokio::task::spawn_blocking(move || {
                let _policy = codewhale_plugins::activation::PolicyScope::propagate(policy);
                #[cfg(any(test, feature = "test-support"))]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                executor.collect_shell_env(&hook_ctx)
            })
            .await
            .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        if read_only_shell {
            let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
            let inert_git_helper = if cfg!(windows) {
                "cmd.exe /d /c exit 1"
            } else {
                "/usr/bin/false"
            };
            // Read-only Bash is intentionally a small inspection surface. Git
            // can otherwise invoke operator/repository configured helpers
            // while performing nominal reads (a pager or fsmonitor), and it
            // may opportunistically refresh the index. These environment
            // overrides make those reads non-interactive and suppress the
            // optional mutation/extension seams; the command classifier and
            // machine authority gate remain authoritative as well.
            extra_env.insert("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string());
            extra_env.insert("GIT_NO_LAZY_FETCH".to_string(), "1".to_string());
            extra_env.insert("GIT_PAGER".to_string(), String::new());
            extra_env.insert("GH_PAGER".to_string(), String::new());
            extra_env.insert("GH_PROMPT_DISABLED".to_string(), "1".to_string());
            extra_env.insert("GH_NO_UPDATE_NOTIFIER".to_string(), "1".to_string());
            // The classifier rejects explicit GHES repo/URL targets. Pin the
            // implicit environment side too, so inherited GH_HOST/GH_REPO
            // cannot redirect the call after api.github.com was approved.
            extra_env.insert("GH_HOST".to_string(), "github.com".to_string());
            extra_env.insert("GH_REPO".to_string(), String::new());
            extra_env.insert("PAGER".to_string(), String::new());
            extra_env.insert("ENV".to_string(), String::new());
            extra_env.insert("BASH_ENV".to_string(), String::new());
            extra_env.insert("CDPATH".to_string(), String::new());
            extra_env.insert("RIPGREP_CONFIG_PATH".to_string(), String::new());
            // Ignore user/system Git configuration and replace any repository
            // external diff helper with a fixed inert executable. Repository
            // config and attributes are attacker-controlled evidence inputs;
            // a nominal `git diff/log/show` must not turn them into programs.
            extra_env.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
            extra_env.insert("GIT_CONFIG_GLOBAL".to_string(), null_device.to_string());
            extra_env.insert("GIT_CONFIG_PARAMETERS".to_string(), String::new());
            extra_env.insert(
                "GIT_EXTERNAL_DIFF".to_string(),
                inert_git_helper.to_string(),
            );
            extra_env.insert("GIT_ATTR_NOSYSTEM".to_string(), "1".to_string());
            if let Some(path) = readonly_sanitized_path(&context.workspace) {
                extra_env.insert("PATH".to_string(), path);
            }
            extra_env.insert("GIT_CONFIG_KEY_0".to_string(), "core.fsmonitor".to_string());
            extra_env.insert("GIT_CONFIG_VALUE_0".to_string(), "false".to_string());
            extra_env.insert("GIT_CONFIG_KEY_1".to_string(), "core.hooksPath".to_string());
            extra_env.insert("GIT_CONFIG_VALUE_1".to_string(), null_device.to_string());
            extra_env.insert(
                "GIT_CONFIG_KEY_2".to_string(),
                "log.showSignature".to_string(),
            );
            extra_env.insert("GIT_CONFIG_VALUE_2".to_string(), "false".to_string());
            // A working-tree read (diff, status, blame) runs the repository's
            // clean filters, which no command-line flag disables.
            let git_dirs = readonly_git_dirs(
                command,
                working_dir
                    .as_deref()
                    .map_or(context.workspace.as_path(), std::path::Path::new),
            );
            let overrides = tokio::task::spawn_blocking(move || {
                let mut overrides = std::collections::BTreeSet::new();
                for dir in git_dirs {
                    overrides.extend(codewhale_dependencies::Git::review_filter_overrides(&dir)?);
                }
                anyhow::Ok(overrides)
            })
            .await
            .map_err(|e| ToolError::execution_failed(format!("git task panicked: {e}")))?
            .map_err(|e| {
                ToolError::permission_denied(format!(
                    "Read-only shell could not inspect the repository's Git filters: {e:#}"
                ))
            })?;
            let mut count = 3;
            for (key, value) in overrides {
                extra_env.insert(format!("GIT_CONFIG_KEY_{count}"), key);
                extra_env.insert(format!("GIT_CONFIG_VALUE_{count}"), value.to_string());
                count += 1;
            }
            extra_env.insert("GIT_CONFIG_COUNT".to_string(), count.to_string());
            extra_env.insert(READONLY_ENV_MARKER.to_string(), "1".to_string());
        }

        let command_expense = infer_command_expense(command);
        let heavy_permit = acquire_heavy_command_permit(command, context.cancel_token.as_ref())
            .await
            .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        let admission_wait_ms = heavy_permit
            .as_ref()
            .map(|permit| u64::try_from(permit.queued_for().as_millis()).unwrap_or(u64::MAX));
        let admission_limit = heavy_permit.as_ref().map(HeavyCommandPermit::limit);
        let admission_memory = heavy_permit
            .as_ref()
            .map(HeavyCommandPermit::memory_pressure);

        // Route through external sandbox backend when configured.
        if let Some(backend) = &context.sandbox_backend {
            if self.optional_timeout {
                return Err(ToolError::not_available(
                    "bash is unavailable with this external sandbox backend because it cannot preserve combined streaming output and timeout semantics. Use the native sandbox or search for the backend-specific shell tool.",
                ));
            }
            if matches!(context.shell_policy, ShellPolicy::ReadOnly) {
                return Err(ToolError::permission_denied(
                    "Read-only Scout shell cannot use an external sandbox backend because that interface accepts a raw command string rather than the classifier-approved argv. Use File read/search, or run this Scout without the external backend.",
                ));
            }
            if interactive {
                return Ok(ToolResult::error(
                    "Interactive mode is not supported with external sandbox backends.",
                ));
            }
            if background {
                return Ok(ToolResult::error(
                    "Background mode is not supported with external sandbox backends.",
                ));
            }
            if tty {
                return Ok(ToolResult::error(
                    "TTY mode is not supported with external sandbox backends.",
                ));
            }

            let started = std::time::Instant::now();
            let backend_result = backend.exec(command, &extra_env).await;

            let result = match backend_result {
                Ok(output) => {
                    let (stdout, stdout_meta) = truncate_with_meta(&output.stdout);
                    let (stderr, stderr_meta) = truncate_with_meta(&output.stderr);
                    ShellResult {
                        task_id: None,
                        status: if output.exit_code == 0 {
                            ShellStatus::Completed
                        } else {
                            ShellStatus::Failed
                        },
                        exit_code: Some(i64::from(output.exit_code)),
                        stdout,
                        stderr,
                        duration_ms: u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        stdout_len: stdout_meta.original_len,
                        stderr_len: stderr_meta.original_len,
                        stdout_omitted: stdout_meta.omitted,
                        stderr_omitted: stderr_meta.omitted,
                        stdout_truncated: stdout_meta.truncated,
                        stderr_truncated: stderr_meta.truncated,
                        sandboxed: true,
                        sandbox_type: Some("opensandbox".to_string()),
                        sandbox_denied: false,
                    }
                }
                Err(e) => {
                    return Ok(ToolResult::error(format!("Sandbox backend error: {e}")));
                }
            };

            // Build result (reuse the existing output rendering below).
            let stdout_summary = summarize_output(&result.stdout);
            let stderr_summary = summarize_output(&result.stderr);
            let summary = if !stderr_summary.is_empty() {
                stderr_summary.clone()
            } else {
                stdout_summary.clone()
            };
            let python_dependency_hint = python_build_dependency_hint(command, &result);
            let mut output = if result.stdout.is_empty() && result.stderr.is_empty() {
                "(no output)".to_string()
            } else if result.stderr.is_empty() {
                result.stdout.clone()
            } else {
                format!("{}\n\nSTDERR:\n{}", result.stdout, result.stderr)
            };
            if let Some(hint) = python_dependency_hint {
                output = format!("{hint}\n\n{output}");
            }

            let mut metadata = json!({
                "exit_code": result.exit_code,
                "exit_code_hex": exit_code_hex(result.exit_code),
                "status": format!("{:?}", result.status),
                "duration_ms": result.duration_ms,
                "sandboxed": true,
                "sandbox_type": "opensandbox",
                "sandbox_denied": false,
                "task_id": result.task_id,
                "stdout_len": result.stdout_len,
                "stderr_len": result.stderr_len,
                "stdout_truncated": result.stdout_truncated,
                "stderr_truncated": result.stderr_truncated,
                "stdout_omitted": result.stdout_omitted,
                "stderr_omitted": result.stderr_omitted,
                "summary": summary,
                "stdout_summary": stdout_summary,
                "stderr_summary": stderr_summary,
                "safety_level": format!("{:?}", safety.level),
                "interactive": false,
                "canceled": false,
                "sandbox_backend": backend.kind().as_str(),
                "expense_class": match command_expense {
                    CommandExpense::Heavy => "heavy",
                    CommandExpense::Normal => "normal",
                },
                "resource_admission_wait_ms": admission_wait_ms,
                "resource_admission_limit": admission_limit,
            });
            attach_shell_owner_metadata(&mut metadata, context);
            attach_cargo_failure_summary(&mut metadata, command, &result);
            attach_python_build_dependency_hint(&mut metadata, python_dependency_hint);

            return Ok(ToolResult {
                content: output,
                success: result.status == ShellStatus::Completed,
                metadata: Some(metadata),
            });
        }

        let mut lifecycle_warning = None;
        let mut receipt_identity = None;
        // #6689: the receipt exists for completion hooks to read. Without one
        // registered it would only ride along in tool metadata, which the
        // Runtime API persists and emits for every call.
        let wants_receipt = context.runtime.hook_executor.as_ref().is_some_and(|hooks| {
            hooks.has_hooks_for_event(codewhale_config::hooks::HookEvent::ToolCallAfter)
                || hooks.has_hooks_for_event(codewhale_config::hooks::HookEvent::OnError)
        });
        let result = if interactive {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            let work_lifecycle = shell_work_lifecycle_from_context(context);
            let task_id = format!("shell_{}", &Uuid::new_v4().to_string()[..8]);
            let mut spawn_guard = ShellSpawnIntentGuard::new(work_lifecycle, &task_id, command);
            // Only a registered operation is observed (#6435).
            let work_lifecycle = spawn_guard.lifecycle.clone();
            let result = manager.execute_interactive_with_policy_env(
                command,
                working_dir.as_deref(),
                timeout_value_ms,
                policy_override,
                extra_env,
            );
            match result {
                Ok(result) => {
                    // The process result is authoritative once execution has
                    // completed. Disarm before observing it so a graph-write
                    // failure cannot relabel a successful command as Failed.
                    spawn_guard.disarm();
                    if let Some(lifecycle) = work_lifecycle.as_ref() {
                        let raw_bytes = result.stdout_len.saturating_add(result.stderr_len);
                        if let Err(err) = lifecycle.observe(&task_id, &result.status, 1, raw_bytes)
                        {
                            tracing::warn!(shell_id = %task_id, error = %err, "interactive shell completed but Work lifecycle reconciliation failed");
                            lifecycle_warning = Some(err.to_string());
                        }
                    }
                    Ok(result)
                }
                Err(err) => Err(err),
            }
        } else if background {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            let result = manager.execute_with_options_env_for_owner_and_work(
                command,
                working_dir.as_deref(),
                timeout_value_ms,
                true,
                stdin_data.as_deref(),
                tty,
                policy_override,
                extra_env,
                shell_job_owner_from_context(context),
                context.state_namespace.clone(),
                context.origin_tool_call_id.clone(),
                context.origin_turn_id.clone(),
                shell_work_lifecycle_from_context(context),
                None,
                persist,
                (1_000, 600_000),
            );
            if let (Ok(result), Some(permit)) = (&result, heavy_permit)
                && let Some(task_id) = result.task_id.as_deref()
            {
                manager
                    .attach_heavy_permit(task_id, permit)
                    .map_err(|error| ToolError::execution_failed(error.to_string()))?;
            }
            result
        } else {
            execute_foreground_via_background(
                context,
                command,
                heavy_permit,
                working_dir,
                timeout_ms,
                stdin_data.as_deref(),
                combined_output,
                policy_override,
                extra_env,
                matches!(context.shell_policy, ShellPolicy::ReadOnly) && !enforced_readonly,
                if self.optional_timeout {
                    (1, BASH_MAX_TIMEOUT_MS)
                } else {
                    (1_000, 600_000)
                },
                wants_receipt,
                &mut receipt_identity,
            )
            .await
        };

        match result {
            Ok(result) => {
                let backgrounded_foreground =
                    !background && !interactive && result.status == ShellStatus::Running;
                if (background || backgrounded_foreground)
                    && let (Some(shell_id), Some(task_id)) = (
                        result.task_id.as_deref(),
                        context.runtime.active_task_id.clone(),
                    )
                    && let Ok(mut manager) = context.shell_manager.lock()
                {
                    let _ = manager.tag_linked_task(shell_id, Some(task_id));
                }

                let was_cancelled = context
                    .cancel_token
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled());
                // Lowercase `bash` joins stdout and stderr on one pipe, so
                // its preview is combined output, not stdout.
                let execution_receipt = receipt_identity.as_ref().and_then(|identity| {
                    shell_execution_receipt(
                        identity,
                        &result,
                        if self.optional_timeout {
                            "combined"
                        } else {
                            "separate"
                        },
                    )
                });
                let readonly_offline_hint = (enforced_readonly && !readonly_network)
                    .then(|| readonly_offline_network_hint(command, &result))
                    .flatten();
                if self.optional_timeout {
                    let network_restricted_hint = readonly_offline_hint
                        .or_else(|| shell_network_restricted_hint(context, command, &result));
                    return finish_contract_bash_result(
                        result,
                        timeout_ms,
                        context,
                        execution_receipt,
                        readonly_network,
                        network_restricted_hint,
                    );
                }
                let task_id_str = result.task_id.clone().unwrap_or_default();
                let stdout_summary = summarize_output(&result.stdout);
                let stderr_summary = summarize_output(&result.stderr);
                let summary = if !stderr_summary.is_empty() {
                    stderr_summary.clone()
                } else {
                    stdout_summary.clone()
                };
                let network_restricted_hint = readonly_offline_hint
                    .or_else(|| shell_network_restricted_hint(context, command, &result))
                    .map(str::to_string);
                let sandbox_denied_hint = if network_restricted_hint.is_none() {
                    shell_sandbox_denied_hint(context, &result)
                } else {
                    None
                };
                let provenance_hint = macos_provenance_hint(&result);
                let python_dependency_hint = python_build_dependency_hint(command, &result);
                let mut output = if interactive {
                    format!(
                        "Interactive command completed (exit code: {:?})",
                        result.exit_code
                    )
                } else if result.status == ShellStatus::Completed {
                    if result.stdout.is_empty() && result.stderr.is_empty() {
                        "(no output)".to_string()
                    } else if result.stderr.is_empty() {
                        result.stdout.clone()
                    } else {
                        format!("{}\n\nSTDERR:\n{}", result.stdout, result.stderr)
                    }
                } else if persist && result.status == ShellStatus::Running {
                    format!(
                        "Persistent service staged: {task_id_str}. Probe readiness with a separate command. Codewhale will transfer ownership only if this exec finishes successfully."
                    )
                } else if result.status == ShellStatus::Running {
                    let completion_contract = if context.owner_agent_id.is_some() {
                        "completion stays in task/status and is not injected into the parent model."
                    } else {
                        "completion is delivered to the model as an internal runtime event before its next step and shown in task/status state; if the turn ends first, a headless run stops the task unseen."
                    };
                    if backgrounded_foreground {
                        let seconds = result.duration_ms / 1_000;
                        let so_far = match (result.stdout.trim(), result.stderr.trim()) {
                            ("", "") => "(no output yet)".to_string(),
                            (out, "") => tail_lines(out, 20),
                            ("", err) => format!("STDERR:\n{}", tail_lines(err, 20)),
                            (out, err) => format!(
                                "{}\n\nSTDERR:\n{}",
                                tail_lines(out, 20),
                                tail_lines(err, 20)
                            ),
                        };
                        let notice = promotion_notice(
                            context,
                            &task_id_str,
                            seconds,
                            &format!("{}{}", result.stdout, result.stderr),
                        );
                        format!(
                            "{notice}\n\nOutput so far:\n{so_far}\n\n{completion_contract} Keep working if you can. To decide: call `task_shell_wait` directly with task_id=\"{task_id_str}\" for more output or completion, or with cancel=true to stop it. It also stops when the session ends; do not sleep-poll with bash."
                        )
                    } else {
                        format!(
                            "Background task started: {task_id_str}\n\nReturns immediately; {completion_contract} Codewhale terminates this task when the session exits. If a service must survive a successful headless exec, start it with background=true and persist=true. Keep working; call `task_shell_wait` directly with task_id=\"{task_id_str}\" at a true dependency to block until completion or timeout. Do not sleep-poll with bash."
                        )
                    }
                } else if result.status == ShellStatus::Killed && was_cancelled {
                    format!(
                        "Command canceled; process killed. {}\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                        kill_outcome_note(&format!("{}{}", result.stdout, result.stderr)),
                        result.stdout,
                        result.stderr
                    )
                } else if result.status == ShellStatus::TimedOut {
                    format!(
                        "Command timed out after {timeout_value_ms}ms; process killed.\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                        result.stdout, result.stderr
                    )
                } else {
                    format!(
                        "Command failed ({})\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                        exit_code_label(result.exit_code),
                        result.stdout,
                        result.stderr
                    )
                };
                if let Some(hint) = network_restricted_hint.as_deref() {
                    output = format!("{hint}\n\n{output}");
                }
                if let Some(hint) = sandbox_denied_hint.as_deref() {
                    output = format!("{hint}\n\n{output}");
                }
                if let Some(hint) = provenance_hint {
                    output = format!("{hint}\n\n{output}");
                }
                if let Some(hint) = python_dependency_hint {
                    output = format!("{hint}\n\n{output}");
                }

                let mut metadata = json!({
                    "exit_code": result.exit_code,
                    "exit_code_hex": exit_code_hex(result.exit_code),
                    "status": format!("{:?}", result.status),
                    "duration_ms": result.duration_ms,
                    "sandboxed": result.sandboxed,
                    "sandbox_type": result.sandbox_type,
                    "sandbox_denied": result.sandbox_denied,
                    "task_id": result.task_id,
                    "stdout_len": result.stdout_len,
                    "stderr_len": result.stderr_len,
                    "stdout_truncated": result.stdout_truncated,
                    "stderr_truncated": result.stderr_truncated,
                    "stdout_omitted": result.stdout_omitted,
                    "stderr_omitted": result.stderr_omitted,
                    "lifecycle_warning": lifecycle_warning,
                    "expense_class": match command_expense {
                        CommandExpense::Heavy => "heavy",
                        CommandExpense::Normal => "normal",
                    },
                    "resource_admission_wait_ms": admission_wait_ms,
                    "resource_admission_limit": admission_limit,
                    "resource_admission_memory": match admission_memory {
                        Some(MemoryPressure::Critical) => "critical",
                        Some(MemoryPressure::Constrained) => "constrained",
                        Some(MemoryPressure::Nominal) | None => "nominal",
                        Some(MemoryPressure::Unknown) => "unknown",
                    },
                    "summary": summary,
                    "stdout_summary": stdout_summary,
                    "stderr_summary": stderr_summary,
                    "safety_level": format!("{:?}", safety.level),
                    "interactive": interactive,
                    "combined_output": combined_output,
                    "canceled": was_cancelled,
                    "execpolicy": execpolicy_decision.as_ref().map(|decision| match decision {
                        RuleDecision::Allow => json!({
                            "decision": "allow",
                        }),
                        RuleDecision::Deny(reason) => json!({
                            "decision": "deny",
                            "reason": reason,
                        }),
                        RuleDecision::AskUser(reason) => json!({
                            "decision": "ask_user",
                            "reason": reason,
                        }),
                    }),
                });
                if let Some(receipt) = execution_receipt {
                    metadata["execution_receipt"] = receipt;
                }
                metadata["backgrounded"] = json!(background || backgrounded_foreground);
                if persist {
                    metadata["persist_requested"] = json!(true);
                    metadata["ownership"] = json!("managed_pending_exec_success");
                    metadata["background_policy"] = json!("pending_ownership_transfer");
                    metadata["auto_resume_on_completion"] = json!(false);
                    metadata["completion_surface"] = json!("headless_exec_release_receipt");
                } else if background || backgrounded_foreground {
                    let child_owned = context.owner_agent_id.is_some();
                    metadata["auto_resume_on_completion"] = json!(!child_owned);
                    metadata["completion_surface"] = if child_owned {
                        json!("task_status_and_explicit_wait")
                    } else {
                        json!("runtime_event_and_task_status")
                    };
                    metadata["background_policy"] = json!("nonblocking");
                }
                if let Some(hint) = network_restricted_hint {
                    metadata["sandbox_network_restricted"] = json!(true);
                    metadata["sandbox_network_denied_hint"] = json!(hint);
                }
                if readonly_network {
                    metadata["sandbox_network"] = json!(true);
                }
                if let Some(hint) = sandbox_denied_hint {
                    metadata["sandbox_denied_hint"] = json!(hint);
                }
                if provenance_hint.is_some() {
                    metadata["macos_provenance_restricted"] = json!(true);
                }
                attach_shell_owner_metadata(&mut metadata, context);
                attach_cargo_failure_summary(&mut metadata, command, &result);
                attach_python_build_dependency_hint(&mut metadata, python_dependency_hint);

                Ok(ToolResult {
                    content: output,
                    success: result.status == ShellStatus::Completed
                        || result.status == ShellStatus::Running,
                    metadata: Some(metadata),
                })
            }
            Err(e) => Ok(ToolResult::error(shell_execution_failed_message(&e))),
        }
    }
}

/// Render a spawn/stream failure for the model and the user: the full cause
/// chain (an anyhow context alone hides the `ENOSPC`/`EMFILE` underneath) plus,
/// when the innermost error looks like host resource exhaustion, what to do
/// about it. The shell tool keeps no state from a failed spawn, so retrying is
/// always safe.
fn shell_execution_failed_message(error: &anyhow::Error) -> String {
    let hint = error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
        .find_map(output::resource_exhaustion_hint);
    match hint {
        Some(hint) => format!(
            "Shell execution failed: {error:#}. Likely host resource exhaustion — {hint}. The shell tool itself is still usable; the next call starts fresh."
        ),
        None => format!("Shell execution failed: {error:#}"),
    }
}

/// Maximum deliberate dependency-barrier wait accepted by `exec_shell_wait`.
pub const EXEC_SHELL_WAIT_MAX_TIMEOUT_MS: u64 = 600_000;

impl BashTool {
    async fn execute_wait(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Multi-task wait (#5549): task_ids + until=any|all. The validated
        // single-task path stays unchanged below when task_ids is absent.
        if let Some(task_ids) = input.get("task_ids") {
            let ids = task_ids
                .as_array()
                .ok_or_else(|| type_mismatch("task_ids", task_ids, "an array of strings"))?;
            let ids = ids
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| type_mismatch("task_ids", value, "strings"))
                        .map(str::to_string)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return self.execute_wait_many(input, context, &ids).await;
        }
        let task_id = required_task_id(input)?;
        let wait = match first_present_field(input, &["wait", "block"]) {
            None => true,
            Some((name, value)) => value
                .as_bool()
                .ok_or_else(|| type_mismatch(name, value, "a boolean"))?,
        };
        let timeout_ms = wait_timeout_ms(input)?;

        let (delta, interruption) = if wait {
            wait_for_shell_delta_cancellable(context, task_id, timeout_ms).await?
        } else {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            let delta = manager
                .get_output_delta_for_session(&context.state_namespace, task_id, false, timeout_ms)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            (delta, None)
        };

        let status = delta.result.status.clone();
        let mut result = build_shell_delta_tool_result(delta, context);
        if let Some(metadata) = result.metadata.as_mut()
            && let Some(object) = metadata.as_object_mut()
        {
            object.insert("wait_timeout_ms".to_string(), json!(timeout_ms));
        }
        if matches!(status, ShellStatus::Running)
            && let Some(progress) = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?
                .running_progress(task_id)
        {
            result.content = format!("{progress}\n{}", result.content);
        }
        if let Some(interruption) = interruption {
            let (action, key) = match interruption {
                ShellWaitInterruption::Canceled => ("canceled", "wait_canceled"),
                ShellWaitInterruption::Detached => ("detached", "wait_detached"),
            };
            if matches!(status, ShellStatus::Running) {
                result.content = format!(
                    "Wait {action}; background shell task {task_id} is still running.\n\n{}",
                    result.content
                );
            }
            if let Some(metadata) = result.metadata.as_mut()
                && let Some(object) = metadata.as_object_mut()
            {
                object.insert(key.to_string(), json!(true));
            }
        }

        Ok(result)
    }

    /// Wait on several background tasks at once (#5549).
    ///
    /// `until` selects the completion condition: `any` returns as soon as one
    /// task is terminal, `all` waits for every one. Unknown task ids fail the
    /// call (same contract as the single-task path); a timeout reports the
    /// still-running ids so the caller can cancel or wait again.
    async fn execute_wait_many(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
        task_ids: &[String],
    ) -> Result<ToolResult, ToolError> {
        let until = match input.get("until").and_then(serde_json::Value::as_str) {
            None | Some("all") => "all",
            Some("any") => "any",
            Some(other) => {
                return Err(ToolError::invalid_input(format!(
                    "until must be \"any\" or \"all\", got {other}"
                )));
            }
        };
        let wait = match first_present_field(input, &["wait", "block"]) {
            None => true,
            Some((name, value)) => value
                .as_bool()
                .ok_or_else(|| type_mismatch(name, value, "a boolean"))?,
        };
        let timeout_ms = wait_timeout_ms(input)?;
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        let mut wait_canceled = false;
        let mut wait_detached = false;

        let snapshot =
            |manager: &mut ShellManager| -> Result<Vec<(String, ShellStatus)>, ToolError> {
                task_ids
                    .iter()
                    .map(|id| {
                        let detail = manager
                            .inspect_job_for_session(&context.state_namespace, id)
                            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
                        Ok((id.clone(), detail.snapshot.status))
                    })
                    .collect()
            };

        let detach = if wait {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            // Validate every owner before registering or consuming any output.
            snapshot(&mut manager)?;
            Some(manager.register_shell_wait(&context.state_namespace))
        } else {
            None
        };
        let mut poll_tick_ms: u64 = FOREGROUND_POLL_INITIAL_MS;
        let statuses = loop {
            let current = {
                let mut manager = context
                    .shell_manager
                    .lock()
                    .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
                snapshot(&mut manager)?
            };
            if context
                .cancel_token
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
            {
                wait_canceled = true;
                break current;
            }
            let running = current
                .iter()
                .filter(|(_, status)| *status == ShellStatus::Running)
                .count();
            let terminal = current.len().saturating_sub(running);
            let satisfied = if until == "any" {
                terminal >= 1
            } else {
                running == 0
            };
            if !wait || satisfied {
                break current;
            }
            if detach
                .as_ref()
                .is_some_and(|detach| detach.load(Ordering::Acquire))
            {
                wait_detached = true;
                break current;
            }
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                break current;
            }
            tokio::time::sleep(Duration::from_millis(poll_tick_ms)).await;
            poll_tick_ms = (poll_tick_ms * 2).min(FOREGROUND_POLL_MAX_MS);
        };

        let running_after = statuses
            .iter()
            .filter(|(_, status)| *status == ShellStatus::Running)
            .count();
        let settled = statuses.len().saturating_sub(running_after);
        let lines: Vec<String> = {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            statuses
                .iter()
                .map(|(id, status)| {
                    let progress = (*status == ShellStatus::Running)
                        .then(|| manager.running_progress(id))
                        .flatten();
                    match progress {
                        Some(progress) => format!("{id}: {status:?} ({progress})"),
                        None => format!("{id}: {status:?}"),
                    }
                })
                .collect()
        };
        let still_running: Vec<&str> = statuses
            .iter()
            .filter(|(_, status)| *status == ShellStatus::Running)
            .map(|(id, _)| id.as_str())
            .collect();
        let summary = if timed_out {
            format!(
                "wait timed out after {}ms; still running: {}",
                timeout_ms,
                if still_running.is_empty() {
                    "none".to_string()
                } else {
                    still_running.join(", ")
                }
            )
        } else if wait_canceled || wait_detached {
            let action = if wait_canceled {
                "canceled"
            } else {
                "detached"
            };
            format!(
                "wait {action}; still running: {}",
                if still_running.is_empty() {
                    "none".to_string()
                } else {
                    still_running.join(", ")
                }
            )
        } else {
            format!(
                "{settled} of {} background command{} settled; remaining running: {}",
                statuses.len(),
                if statuses.len() == 1 { "" } else { "s" },
                if still_running.is_empty() {
                    "none".to_string()
                } else {
                    still_running.join(", ")
                }
            )
        };
        let content = format!("{summary}\n{}\n", lines.join("\n"));
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "statuses".to_string(),
            serde_json::Value::Object(
                statuses
                    .iter()
                    .map(|(id, status)| (id.clone(), serde_json::json!(format!("{status:?}"))))
                    .collect::<serde_json::Map<_, _>>(),
            ),
        );
        metadata.insert("wait_timeout_ms".to_string(), json!(timeout_ms));
        metadata.insert("until".to_string(), json!(until));
        metadata.insert("timed_out".to_string(), json!(timed_out));
        if wait_canceled {
            metadata.insert("wait_canceled".to_string(), json!(true));
        }
        if wait_detached {
            metadata.insert("wait_detached".to_string(), json!(true));
        }
        Ok(ToolResult {
            content: content.trim().to_string(),
            success: true,
            metadata: Some(serde_json::Value::Object(metadata)),
        })
    }

    async fn execute_interact(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let task_id = required_task_id(input)?;
        let close_stdin = optional_bool(input, "close_stdin", false)?;
        let timeout_ms = optional_u64(input, "timeout_ms", 1_000)?;
        // Same strict-type contract as `run` (2026-08-04): a non-string here
        // was silently dropped, so an `interact` call reported success while
        // writing nothing to the child's stdin. Alias order also matches
        // `run` now — `stdin` first — so the same payload reaches the same
        // place whichever spelling the model uses.
        let interaction_input = match first_present_field(input, &["stdin", "input", "data"]) {
            None => "",
            Some((name, value)) => value
                .as_str()
                .ok_or_else(|| type_mismatch(name, value, "a string"))?,
        };

        {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            if !interaction_input.is_empty() || close_stdin {
                manager
                    .write_stdin_for_session(
                        &context.state_namespace,
                        task_id,
                        interaction_input,
                        close_stdin,
                    )
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            }
        }

        let mut elapsed = 0u64;
        loop {
            if context
                .cancel_token
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
            {
                let mut manager = context
                    .shell_manager
                    .lock()
                    .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
                let delta = manager
                    .get_output_delta_for_session(&context.state_namespace, task_id, false, 0)
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?;
                let mut result = build_shell_delta_tool_result(delta, context);
                if let Some(metadata) = result.metadata.as_mut()
                    && let Some(object) = metadata.as_object_mut()
                {
                    object.insert("wait_canceled".to_string(), json!(true));
                }
                return Ok(result);
            }

            let delta = {
                let mut manager = context
                    .shell_manager
                    .lock()
                    .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
                manager
                    .get_output_delta_for_session(&context.state_namespace, task_id, false, 0)
                    .map_err(|err| ToolError::execution_failed(err.to_string()))?
            };

            if !delta.result.stdout.is_empty()
                || !delta.result.stderr.is_empty()
                || delta.result.status != ShellStatus::Running
                || elapsed >= timeout_ms
            {
                return Ok(build_shell_delta_tool_result(delta, context));
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
            elapsed = elapsed.saturating_add(50);
        }
    }

    async fn execute_cancel(
        &self,
        input: &serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let cancel_all = optional_bool(input, "all", false)?;
        let mut manager = context
            .shell_manager
            .lock()
            .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;

        if cancel_all {
            let results = manager
                .kill_running_for_session(&context.state_namespace)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            for result in &results {
                manager.mark_cancel_reported(result);
            }
            if results.is_empty() {
                return Ok(ToolResult {
                    content: "No running background commands.".to_string(),
                    success: true,
                    metadata: Some(json!({
                        "status": "Noop",
                        "canceled": 0,
                        "task_ids": [],
                    })),
                });
            }

            let task_ids = results
                .iter()
                .filter_map(|result| result.task_id.clone())
                .collect::<Vec<_>>();
            return Ok(ToolResult {
                content: format!(
                    "Canceled {} background command{}: {}",
                    task_ids.len(),
                    if task_ids.len() == 1 { "" } else { "s" },
                    task_ids.join(", ")
                ),
                success: true,
                metadata: Some(json!({
                    "status": "Killed",
                    "canceled": task_ids.len(),
                    "task_ids": task_ids,
                })),
            });
        }

        let task_id = required_task_id(input)?;
        let result = manager
            .kill_for_session(&context.state_namespace, task_id)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        manager.mark_cancel_reported(&result);
        let task_id = result
            .task_id
            .clone()
            .unwrap_or_else(|| task_id.to_string());
        Ok(ToolResult {
            content: if result.status == ShellStatus::Killed {
                format!(
                    "Canceled background command: {task_id}. {}",
                    kill_outcome_note(&format!("{}{}", result.stdout, result.stderr))
                )
            } else {
                format!("{} Nothing was killed.", wait_timing_line(&result))
            },
            success: true,
            metadata: Some(json!({
                "status": format!("{:?}", result.status),
                "task_id": task_id,
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
            })),
        })
    }
}

fn required_task_id(input: &serde_json::Value) -> Result<&str, ToolError> {
    // A present-but-non-string task_id is a type error, not a missing field:
    // "missing required field" sends the model's retry in the wrong
    // direction when it already supplied `task_id: 42` (2026-08-04 review).
    match first_present_field(input, &["task_id", "id"]) {
        None => Err(ToolError::missing_field("task_id")),
        Some((name, value)) => value
            .as_str()
            .ok_or_else(|| type_mismatch(name, value, "a string")),
    }
}

/// First PRESENT value among aliased spellings of one field. `null` counts
/// as absent, matching the `is_absent` rule the shared typed helpers use.
fn first_present_field<'a>(
    input: &'a serde_json::Value,
    names: &[&'static str],
) -> Option<(&'static str, &'a serde_json::Value)> {
    names.iter().find_map(|name| match input.get(*name) {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some((*name, value)),
    })
}

/// Effective `action=wait` timeout in milliseconds. `timeout_ms` is
/// canonical; `timeout_secs` (seconds) and bare `timeout` (milliseconds) are
/// honored so a habit formed on other wait tools gets the duration it asked
/// for instead of silently falling back to the 30 s default.
fn wait_timeout_ms(input: &serde_json::Value) -> Result<u64, ToolError> {
    match first_present_field(input, &["timeout_ms", "timeout_secs", "timeout"]) {
        None => Ok(30_000),
        Some(("timeout_secs", value)) => {
            let secs = value
                .as_u64()
                .ok_or_else(|| type_mismatch("timeout_secs", value, "an integer"))?;
            Ok(secs.saturating_mul(1_000))
        }
        Some((name, value)) => value
            .as_u64()
            .ok_or_else(|| type_mismatch(name, value, "an integer")),
    }
}

fn build_shell_delta_tool_result(delta: ShellDeltaResult, context: &ToolContext) -> ToolResult {
    let result = delta.result;
    let network_restricted_hint =
        shell_network_restricted_hint(context, &delta.command, &result).map(str::to_string);
    let sandbox_denied_hint = if network_restricted_hint.is_none() {
        shell_sandbox_denied_hint(context, &result)
    } else {
        None
    };
    let provenance_hint = macos_provenance_hint(&result);
    let python_dependency_hint = python_build_dependency_hint(&delta.command, &result);
    let stdout_summary = summarize_output(&result.stdout);
    let stderr_summary = summarize_output(&result.stderr);
    let summary = if !stderr_summary.is_empty() {
        stderr_summary.clone()
    } else {
        stdout_summary.clone()
    };

    let mut output = if result.stdout.is_empty() && result.stderr.is_empty() {
        match result.status {
            ShellStatus::Running => "Background task running (no new output).".to_string(),
            ShellStatus::Completed => "(no new output)".to_string(),
            ShellStatus::Failed => {
                format!("Command failed ({})", exit_code_label(result.exit_code))
            }
            ShellStatus::TimedOut => "Command timed out (no new output).".to_string(),
            ShellStatus::Killed => "Command killed (no new output).".to_string(),
        }
    } else if result.stderr.is_empty() {
        result.stdout.clone()
    } else {
        format!("{}\n\nSTDERR:\n{}", result.stdout, result.stderr)
    };
    // The model cannot see metadata, so surface the real elapsed time in the
    // visible content. Without it every wait result looks identical whether
    // the task just started or has been running for minutes, which biases the
    // model into busy-polling short waits and misjudging long ones.
    output = format!("{}\n\n{output}", wait_timing_line(&result));

    if let Some(hint) = network_restricted_hint.as_deref() {
        output = format!("{hint}\n\n{output}");
    }
    if let Some(hint) = sandbox_denied_hint.as_deref() {
        output = format!("{hint}\n\n{output}");
    }
    if let Some(hint) = provenance_hint {
        output = format!("{hint}\n\n{output}");
    }
    if let Some(hint) = python_dependency_hint {
        output = format!("{hint}\n\n{output}");
    }

    let mut metadata = json!({
        "exit_code": result.exit_code,
        "exit_code_hex": exit_code_hex(result.exit_code),
        "status": format!("{:?}", result.status),
        "duration_ms": result.duration_ms,
        "sandboxed": result.sandboxed,
        "sandbox_type": result.sandbox_type,
        "sandbox_denied": result.sandbox_denied,
        "task_id": result.task_id,
        "stdout_len": result.stdout_len,
        "stderr_len": result.stderr_len,
        "stdout_truncated": result.stdout_truncated,
        "stderr_truncated": result.stderr_truncated,
        "stdout_omitted": result.stdout_omitted,
        "stderr_omitted": result.stderr_omitted,
        "stdout_total_len": delta.stdout_total_len,
        "stderr_total_len": delta.stderr_total_len,
        "summary": summary,
        "stdout_summary": stdout_summary,
        "stderr_summary": stderr_summary,
        "command": delta.command,
        "stream_delta": true,
    });
    attach_shell_owner_metadata(&mut metadata, context);
    attach_cargo_failure_summary(&mut metadata, &delta.command, &result);
    attach_python_build_dependency_hint(&mut metadata, python_dependency_hint);

    let mut tool_result = ToolResult {
        content: output,
        success: matches!(result.status, ShellStatus::Completed | ShellStatus::Running),
        metadata: Some(metadata),
    };
    if let Some(hint) = network_restricted_hint
        && let Some(metadata) = tool_result.metadata.as_mut()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("sandbox_network_restricted".to_string(), json!(true));
        object.insert("sandbox_network_denied_hint".to_string(), json!(hint));
    }
    if let Some(hint) = sandbox_denied_hint
        && let Some(metadata) = tool_result.metadata.as_mut()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("sandbox_denied_hint".to_string(), json!(hint));
    }
    if provenance_hint.is_some()
        && let Some(metadata) = tool_result.metadata.as_mut()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("macos_provenance_restricted".to_string(), json!(true));
    }
    tool_result
}

/// Human-readable elapsed time for a shell task ("450 ms", "12.3 s", "2m5s").
fn format_elapsed_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        let secs = ms as f64 / 1_000.0;
        format!("{secs} s")
    } else {
        let total_secs = ms / 1_000;
        format!("{}m{}s", total_secs / 60, total_secs % 60)
    }
}

/// One-line status + elapsed summary for wait/delta results, placed at the top
/// of the visible content so the model can judge how long it actually waited.
fn wait_timing_line(result: &ShellResult) -> String {
    let status_phrase = match result.status {
        ShellStatus::Running => "still running",
        ShellStatus::Completed => "completed",
        ShellStatus::Failed => "failed",
        ShellStatus::Killed => "killed",
        ShellStatus::TimedOut => "timed out",
    };
    let elapsed = format_elapsed_ms(result.duration_ms);
    match result.task_id.as_deref() {
        Some(task_id) => format!("Task {task_id} {status_phrase} after {elapsed}."),
        None => format!("Task {status_phrase} after {elapsed}."),
    }
}

enum ShellWaitInterruption {
    Canceled,
    Detached,
}

async fn wait_for_shell_delta_cancellable(
    context: &ToolContext,
    task_id: &str,
    timeout_ms: u64,
) -> Result<(ShellDeltaResult, Option<ShellWaitInterruption>), ToolError> {
    let detach = {
        let mut manager = context
            .shell_manager
            .lock()
            .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
        manager
            .require_session_owner(task_id, &context.state_namespace)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        manager.register_shell_wait(&context.state_namespace)
    };
    let timeout_ms = timeout_ms.clamp(1000, EXEC_SHELL_WAIT_MAX_TIMEOUT_MS);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut stdout_accum = String::new();
    let mut stderr_accum = String::new();

    let mut poll_tick_ms: u64 = FOREGROUND_POLL_INITIAL_MS;
    let (command, result, stdout_total_len, stderr_total_len) = loop {
        let canceled = context
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled());
        if canceled || detach.load(Ordering::Acquire) {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            let delta = manager
                .get_output_delta_for_session(&context.state_namespace, task_id, false, 0)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?;
            append_shell_delta_output(&mut stdout_accum, &mut stderr_accum, &delta.result);
            return Ok((
                shell_delta_with_accumulated_output(
                    delta.command,
                    delta.result,
                    &stdout_accum,
                    &stderr_accum,
                    delta.stdout_total_len,
                    delta.stderr_total_len,
                ),
                Some(if canceled {
                    ShellWaitInterruption::Canceled
                } else {
                    ShellWaitInterruption::Detached
                }),
            ));
        }

        let delta = {
            let mut manager = context
                .shell_manager
                .lock()
                .map_err(|_| ToolError::execution_failed("shell manager lock poisoned"))?;
            manager
                .get_output_delta_for_session(&context.state_namespace, task_id, false, 0)
                .map_err(|err| ToolError::execution_failed(err.to_string()))?
        };

        let stdout_total_len = delta.stdout_total_len;
        let stderr_total_len = delta.stderr_total_len;
        let command = delta.command.clone();
        append_shell_delta_output(&mut stdout_accum, &mut stderr_accum, &delta.result);

        let status = delta.result.status.clone();
        if status != ShellStatus::Running || Instant::now() >= deadline {
            break (command, delta.result, stdout_total_len, stderr_total_len);
        }

        tokio::time::sleep(Duration::from_millis(poll_tick_ms)).await;
        poll_tick_ms = (poll_tick_ms * 2).min(FOREGROUND_POLL_MAX_MS);
    };

    Ok((
        shell_delta_with_accumulated_output(
            command,
            result,
            &stdout_accum,
            &stderr_accum,
            stdout_total_len,
            stderr_total_len,
        ),
        None,
    ))
}

fn append_shell_delta_output(
    stdout_accum: &mut String,
    stderr_accum: &mut String,
    result: &ShellResult,
) {
    if !result.stdout.is_empty() {
        stdout_accum.push_str(&result.stdout);
    }
    if !result.stderr.is_empty() {
        stderr_accum.push_str(&result.stderr);
    }
}

fn shell_delta_with_accumulated_output(
    command: String,
    mut result: ShellResult,
    stdout_accum: &str,
    stderr_accum: &str,
    stdout_total_len: usize,
    stderr_total_len: usize,
) -> ShellDeltaResult {
    let (stdout, stdout_meta) = truncate_with_meta(stdout_accum);
    let (stderr, stderr_meta) = truncate_with_meta(stderr_accum);
    result.stdout = stdout;
    result.stderr = stderr;
    result.stdout_len = stdout_meta.original_len;
    result.stderr_len = stderr_meta.original_len;
    result.stdout_omitted = stdout_meta.omitted;
    result.stderr_omitted = stderr_meta.omitted;
    result.stdout_truncated = stdout_meta.truncated;
    result.stderr_truncated = stderr_meta.truncated;

    ShellDeltaResult {
        command,
        result,
        stdout_total_len,
        stderr_total_len,
    }
}

/// Refuse a notes target this auto-approved tool must not append to.
///
/// The file itself must not be a symlink (a committed `notes.md -> ~/.zshrc`
/// would redirect the append), and a target placed inside the workspace must
/// resolve inside it after symlinked parent directories are followed.
async fn ensure_notes_target_is_safe(
    notes_path: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<(), ToolError> {
    if let Ok(meta) = tokio::fs::symlink_metadata(notes_path).await
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(ToolError::permission_denied(format!(
            "Refusing to append a note to {}: the notes path is a symlink or not a regular file.",
            notes_path.display()
        )));
    }
    if notes_path.starts_with(workspace)
        && let (Some(parent), Ok(root)) = (
            notes_path.parent(),
            tokio::fs::canonicalize(workspace).await,
        )
    {
        // Walk up to the nearest existing ancestor: the rest is created
        // below as real directories, so only existing links can redirect.
        let mut existing = parent.to_path_buf();
        while !tokio::fs::try_exists(&existing).await.unwrap_or(false) {
            if !existing.pop() {
                break;
            }
        }
        if let Ok(resolved) = tokio::fs::canonicalize(&existing).await
            && !resolved.starts_with(&root)
        {
            return Err(ToolError::permission_denied(format!(
                "Refusing to append a note to {}: its directory resolves outside the workspace.",
                notes_path.display()
            )));
        }
    }
    Ok(())
}

/// Tool for appending notes to a notes file.
pub struct NoteTool;

#[async_trait]
impl ToolSpec for NoteTool {
    fn name(&self) -> &'static str {
        "note"
    }

    fn description(&self) -> &'static str {
        "Append a note to the agent notes file for persistent context across sessions."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The note content to append"
                }
            },
            "required": ["content"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto // Notes are low-risk
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let note_content = required_str(&input, "content")?;
        ensure_notes_target_is_safe(&context.notes_path, &context.workspace).await?;

        let mut file = if context.notes_path.starts_with(&context.workspace) {
            let workspace = context.workspace.clone();
            let notes_path = context.notes_path.clone();
            let file = tokio::task::spawn_blocking(move || {
                codewhale_util::fs_confined::open_append(&workspace, &notes_path)
            })
            .await
            .map_err(|e| ToolError::execution_failed(format!("Failed to open notes file: {e}")))?
            .map_err(|e| ToolError::execution_failed(format!("Failed to open notes file: {e}")))?;
            tokio::fs::File::from_std(file)
        } else {
            // Ensure parent directory exists. Tool handlers run on the Tokio
            // runtime, so filesystem calls use tokio::fs (blocking-call
            // convention, #6149).
            if let Some(parent) = context.notes_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    ToolError::execution_failed(format!("Failed to create notes directory: {e}"))
                })?;
            }

            // Append to notes file
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&context.notes_path)
                .await
                .map_err(|e| {
                    ToolError::execution_failed(format!("Failed to open notes file: {e}"))
                })?
        };

        use tokio::io::AsyncWriteExt;
        file.write_all(format!("\n---\n{note_content}\n").as_bytes())
            .await
            .map_err(|e| ToolError::execution_failed(format!("Failed to write note: {e}")))?;
        // tokio's File finishes a write on a background thread; report
        // success only once the bytes reached the file.
        file.flush()
            .await
            .map_err(|e| ToolError::execution_failed(format!("Failed to write note: {e}")))?;

        Ok(ToolResult::success(format!(
            "Note appended to {}",
            context.notes_path.display()
        )))
    }
}

#[cfg(test)]
#[path = "shell/tests/enforced_readonly.rs"]
mod enforced_readonly_tests;
#[cfg(test)]
mod tests;

pub fn foreground_command_requests_detach(command: &str) -> bool {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    let chars = command.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && !single_quoted {
            escaped = true;
            continue;
        }
        if ch == '\'' && !double_quoted {
            single_quoted = !single_quoted;
            continue;
        }
        if ch == '"' && !single_quoted {
            double_quoted = !double_quoted;
            continue;
        }
        if ch != '&' || single_quoted || double_quoted {
            continue;
        }
        let previous = index.checked_sub(1).and_then(|i| chars.get(i)).copied();
        let next = chars.get(index + 1).copied();
        // `&&`, `&>`/`&>>`, and `>&` are chaining/redirection rather than a
        // detached child. Any other unquoted ampersand is a background
        // control operator and is unavailable in ACP.
        if previous != Some('&') && next != Some('&') && next != Some('>') && previous != Some('>')
        {
            return true;
        }
    }

    shell_words::split(command).is_ok_and(|words| {
        words.iter().any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "nohup" | "disown" | "setsid" | "daemonize"
            )
        })
    })
}

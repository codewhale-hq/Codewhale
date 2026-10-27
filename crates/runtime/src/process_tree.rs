//! Process-tree containment for a spawned child and everything it starts.
//!
//! Moved out of `hooks/executor.rs` (where it was `HookProcessTree` /
//! `WindowsHookJob`) so the hook runner and the extension host share one
//! implementation. Killing only the immediate child can leave the real work
//! alive (a hook's shell runtime, a host plugin's `child_process`), so:
//!
//! * **Unix:** the child must be spawned as the leader of its own process
//!   group (`process_group(0)`); the tree is that group, and it is SIGKILLed
//!   as a whole.
//! * **Windows:** the child is assigned to a Job Object configured with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so closing the job kills every
//!   process in it. Hooks are created suspended and resumed only after the
//!   assignment. Tokio children are assigned after spawning and can start
//!   descendants before assignment. This bounds ordinary process lifetimes;
//!   it is not a security sandbox.
//!
//! Dropping the guard kills the tree. That is deliberate: the guard's lifetime
//! is the tree's lifetime, unless [`ProcessTree::release`] explicitly lets the
//! tree outlive it.

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows::core::PCWSTR;

#[cfg(windows)]
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

/// Owns the process tree rooted at one spawned child.
pub struct ProcessTree {
    #[cfg(unix)]
    pgid: libc::pid_t,
    #[cfg(windows)]
    job: WindowsJob,
}

// SAFETY (Windows): the job handle is an owned kernel handle; it is only
// used through `&self` calls that the OS serializes, and closed once in Drop.
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
#[cfg(windows)]
unsafe impl Sync for ProcessTree {}

impl ProcessTree {
    /// Contain a `std::process::Child` (spawned with `process_group(0)` on Unix).
    pub fn attach(child: &std::process::Child) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            Self::attach_parts(child.id(), child.as_raw_handle())
        }
        #[cfg(not(windows))]
        {
            Self::attach_parts(child.id())
        }
    }

    /// Contain a `tokio::process::Child` (spawned with `process_group(0)` on
    /// Unix). Fails if the child has already been reaped.
    pub fn attach_tokio(child: &tokio::process::Child) -> std::io::Result<Self> {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("child already exited"))?;
        #[cfg(windows)]
        {
            let handle = child
                .raw_handle()
                .ok_or_else(|| std::io::Error::other("child already exited"))?;
            Self::attach_parts(pid, handle)
        }
        #[cfg(not(windows))]
        {
            Self::attach_parts(pid)
        }
    }

    /// Typed owned handle from a suspended Core-selected process. The caller
    /// attaches/caps the Job before any process thread is resumed.
    #[cfg(windows)]
    pub fn attach_windows_handle(
        pid: u32,
        process: &std::os::windows::io::OwnedHandle,
    ) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        Self::attach_parts(pid, process.as_raw_handle())
    }

    #[cfg(windows)]
    fn attach_parts(_pid: u32, handle: std::os::windows::io::RawHandle) -> std::io::Result<Self> {
        Ok(Self {
            job: WindowsJob::attach(handle)?,
        })
    }

    #[cfg(not(windows))]
    #[allow(clippy::unnecessary_wraps)]
    fn attach_parts(pid: u32) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                pgid: pid as libc::pid_t,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Ok(Self {})
        }
    }

    /// Kill every process in the tree. A tree that is already gone is not an
    /// error. On failure the caller should fall back to killing the child.
    pub fn kill(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            // SAFETY: kill(2) dereferences no pointers; a negative pid names
            // the process group.
            let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(std::io::Error::other(
                "process-tree containment is not supported on this platform",
            ))
        }
    }
}

impl ProcessTree {
    /// Windows: cap the committed memory of every process in the job at
    /// `bytes` each (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`); an allocation past
    /// it fails. Kill-on-close stays set. The extension host's memory cap.
    #[cfg(windows)]
    pub fn limit_process_memory(&self, bytes: u64) -> std::io::Result<()> {
        self.job.limit_process_memory(bytes)
    }

    /// Give up containment without killing anything: the tree outlives the
    /// guard. For a command that exited on its own and may have deliberately
    /// left something running.
    pub fn release(self) {
        #[cfg(windows)]
        {
            // Closing the handle kills the job unless the limit is cleared
            // first. If clearing fails, the close still kills: the safe side.
            let _ = self.job.clear_kill_on_close();
        }
        #[cfg(not(windows))]
        std::mem::forget(self);
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: kill(2) dereferences no pointers.
        unsafe {
            // The leader may have exited while a descendant still holds an
            // inherited pipe. Reaping the group keeps lifetimes bounded.
            let _ = libc::kill(-self.pgid, libc::SIGKILL);
        }
        // On Windows, dropping `WindowsJob` closes a Job Object configured
        // with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
    }
}

/// Start a synchronous child inside the same process group / Job used by hooks.
/// Windows starts suspended and resumes only after Job assignment. The caller
/// still owns environment policy and bounded waits; this is lifetime containment.
pub fn spawn_contained_std(
    command: &mut std::process::Command,
) -> std::io::Result<(std::process::Child, ProcessTree)> {
    use wait_timeout::ChildExt;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0004 | 0x0800_0000); // suspended, no window
    }
    let mut child = command.spawn()?;
    let tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait_timeout(std::time::Duration::from_millis(250));
            return Err(error);
        }
    };
    #[cfg(windows)]
    if let Err(error) = resume_windows_process(&child) {
        drop(tree);
        let _ = child.kill();
        let _ = child.wait_timeout(std::time::Duration::from_millis(250));
        return Err(error);
    }
    Ok((child, tree))
}

#[cfg(windows)]
fn resume_windows_process(child: &std::process::Child) -> std::io::Result<()> {
    let snapshot =
        // SAFETY: returned handle is owned here; closed before return.
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(windows_io_error)? };
    let result = (|| {
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        // SAFETY: `entry` is live with dwSize initialized above.
        let mut next = unsafe { Thread32First(snapshot, &mut entry) };
        let mut resumed = 0usize;
        while next.is_ok() {
            if entry.th32OwnerProcessID == child.id() {
                // SAFETY: returned handle is owned here; closed below.
                let thread = unsafe {
                    OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
                        .map_err(windows_io_error)?
                };
                // SAFETY: `thread` is a live owned handle.
                let resume_result = unsafe { ResumeThread(thread) };
                // SAFETY: `thread` is owned here and not used after.
                let close_result = unsafe { CloseHandle(thread).map_err(windows_io_error) };
                if resume_result == u32::MAX {
                    return Err(std::io::Error::last_os_error());
                }
                close_result?;
                resumed += 1;
            }
            // SAFETY: `entry` is live with dwSize initialized above.
            next = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        if resumed == 0 {
            return Err(std::io::Error::other(
                "suspended process had no resumable thread",
            ));
        }
        Ok(())
    })();
    // SAFETY: `snapshot` is owned here and not used after.
    let close_result = unsafe { CloseHandle(snapshot).map_err(windows_io_error) };
    result?;
    close_result
}

/// `Command::output()` for a child whose whole process tree dies with the
/// returned future. Dropping it — a caller's `timeout` elapsing, or a
/// cancelled tool call — kills the child and everything it started, where a
/// bare `output()` left them running. Stdin is closed: callers run
/// non-interactive work that must never wait on input.
pub async fn contained_output(
    cmd: &mut tokio::process::Command,
) -> std::io::Result<std::process::Output> {
    contained_output_until(cmd, std::future::pending())
        .await
        .map(|run| run.output)
}

/// What [`contained_output_until`] collected.
pub struct ContainedOutput {
    pub output: std::process::Output,
    /// `stop` fired before the command exited: the tree was killed, and
    /// `output` holds everything it wrote until then.
    pub stopped: bool,
}

/// [`contained_output`], plus a `stop` future (a deadline, a cancel token).
/// When `stop` fires first the tree is killed and the output written so far is
/// still returned, so a hung command leaves evidence of where it hung.
///
/// A command that exits on its own is released, not killed: whatever it
/// deliberately left running with its output redirected (`nohup server >log
/// &`) keeps running, as it did with a bare `output()`. Only a dropped future
/// or `stop` kills the tree. Until then the group is also listed for
/// [`kill_contained_trees_for_exit`], because a terminating signal ends
/// Codewhale through `process::exit`, where no destructor runs, and a child in
/// its own process group no longer receives the terminal's Ctrl+C itself.
pub async fn contained_output_until(
    cmd: &mut tokio::process::Command,
    stop: impl std::future::Future<Output = ()>,
) -> std::io::Result<ContainedOutput> {
    contained_run(cmd, None, stop, ProcessTree::attach_tokio).await
}

/// [`contained_output`] for a command that reads a request on stdin (a plugin
/// tool's JSON input): `input` is written while the output pipes drain, then
/// stdin is closed.
pub async fn contained_output_with_input(
    cmd: &mut tokio::process::Command,
    input: Vec<u8>,
) -> std::io::Result<std::process::Output> {
    contained_run(
        cmd,
        Some(input),
        std::future::pending(),
        ProcessTree::attach_tokio,
    )
    .await
    .map(|run| run.output)
}

async fn contained_run(
    cmd: &mut tokio::process::Command,
    input: Option<Vec<u8>>,
    stop: impl std::future::Future<Output = ()>,
    attach: impl FnOnce(&tokio::process::Child) -> std::io::Result<ProcessTree>,
) -> std::io::Result<ContainedOutput> {
    contained_run_with_limits(cmd, input, stop, attach, None).await
}

/// Strict capture bounds for the admitted script runner. Overflow refuses the result;
/// it never presents a truncated ToolResult as success. Same containment driver.
pub async fn contained_output_with_input_bounded(
    cmd: &mut tokio::process::Command,
    input: Vec<u8>,
    stdout_limit: usize,
    stderr_limit: usize,
    stop: impl std::future::Future<Output = ()>,
) -> std::io::Result<ContainedOutput> {
    contained_run_with_limits(
        cmd,
        Some(input),
        stop,
        ProcessTree::attach_tokio,
        Some((stdout_limit, stderr_limit)),
    )
    .await
}
async fn contained_run_with_limits(
    cmd: &mut tokio::process::Command,
    input: Option<Vec<u8>>,
    stop: impl std::future::Future<Output = ()>,
    attach: impl FnOnce(&tokio::process::Child) -> std::io::Result<ProcessTree>,
    limits: Option<(usize, usize)>,
) -> std::io::Result<ContainedOutput> {
    use std::process::Stdio;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    // Never report a contained run when attachment failed. The child's
    // kill-on-drop guard still cleans up the direct child on this error path.
    let tree = attach(&child)?;
    // Declared after `tree`, so a dropped future unlists the group first and
    // the tree guard then kills it.
    #[cfg(unix)]
    let _listed = child.id().map(ContainedExitListing::new);
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdin_pipe = child.stdin.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exited = {
        // Written alongside the drains: a child that answers before it has
        // read all of its input would otherwise deadlock on a full pipe.
        let feed = async move {
            use tokio::io::AsyncWriteExt;
            if let (Some(mut pipe), Some(input)) = (stdin_pipe, input) {
                // A child that exits without reading its input closes the
                // pipe; that is its answer, not a run failure.
                if pipe.write_all(&input).await.is_ok() {
                    let _ = pipe.shutdown().await;
                }
            }
        };
        let run = async {
            let out = drain_pipe_with_limit(
                stdout_pipe.as_mut(),
                &mut stdout,
                limits.map(|limits| limits.0),
            );
            let err = drain_pipe_with_limit(
                stderr_pipe.as_mut(),
                &mut stderr,
                limits.map(|limits| limits.1),
            );
            let wait = async {
                let status = child.wait().await?;
                if limits.is_some() {
                    let _ = tree.kill();
                }
                Ok::<_, std::io::Error>(status)
            };
            if limits.is_some() {
                let (_, _, status, ()) = tokio::try_join!(out, err, wait, async {
                    feed.await;
                    Ok::<(), std::io::Error>(())
                })?;
                Ok(status)
            } else {
                let (out, err, status, ()) = tokio::join!(out, err, wait, feed);
                out?;
                err?;
                status
            }
        };
        tokio::select! {
            status = run => match status {
                Ok(status) => Some(status),
                Err(error) => {
                    let _ = tree.kill(); let _ = child.start_kill();
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
                    return Err(error);
                }
            },
            () = stop => None,
        }
    };
    let (status, stopped) = match exited {
        Some(status) => {
            if limits.is_some() {
                drop(tree);
            } else {
                tree.release();
            }
            (status, false)
        }
        None => {
            drop(tree);
            let _ = child.start_kill();
            let status = if limits.is_some() {
                tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                    .await
                    .map_err(|_| {
                        std::io::Error::other("execution child did not reap after cancellation")
                    })??
            } else {
                child.wait().await?
            };
            // Collect what is still buffered in the pipes. Bounded: a process
            // that escaped the tree may still hold one open.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                let _ = tokio::join!(
                    drain_pipe_with_limit(
                        stdout_pipe.as_mut(),
                        &mut stdout,
                        limits.map(|limits| limits.0)
                    ),
                    drain_pipe_with_limit(
                        stderr_pipe.as_mut(),
                        &mut stderr,
                        limits.map(|limits| limits.1)
                    ),
                );
            })
            .await;
            (status, true)
        }
    };
    Ok(ContainedOutput {
        output: std::process::Output {
            status,
            stdout,
            stderr,
        },
        stopped,
    })
}

/// Read each chunk under the selected capture bound; overflow is an error.
async fn drain_pipe_with_limit<R: tokio::io::AsyncRead + Unpin>(
    pipe: Option<&mut R>,
    into: &mut Vec<u8>,
    limit: Option<usize>,
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let Some(pipe) = pipe else {
        return Ok(());
    };
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        if limit.is_some_and(|limit| into.len().saturating_add(read) > limit) {
            return Err(std::io::Error::other(
                "execution output exceeded its capture limit",
            ));
        }
        into.extend_from_slice(&chunk[..read]);
    }
}

/// Process groups of [`contained_output_until`] runs still in flight.
#[cfg(unix)]
static CONTAINED_EXIT_GROUPS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<u32>>,
> = std::sync::OnceLock::new();

#[cfg(unix)]
fn contained_exit_groups() -> std::sync::MutexGuard<'static, std::collections::HashSet<u32>> {
    CONTAINED_EXIT_GROUPS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Lists one in-flight process group until dropped.
#[cfg(unix)]
struct ContainedExitListing(u32);

#[cfg(unix)]
impl ContainedExitListing {
    fn new(process_group_id: u32) -> Self {
        contained_exit_groups().insert(process_group_id);
        Self(process_group_id)
    }
}

#[cfg(unix)]
impl Drop for ContainedExitListing {
    fn drop(&mut self) {
        contained_exit_groups().remove(&self.0);
    }
}

/// Kill every in-flight contained tree. The process-wide signal path calls
/// this immediately before `process::exit`, where Rust destructors cannot run.
#[cfg(unix)]
pub fn kill_contained_trees_for_exit() {
    let groups = contained_exit_groups().drain().collect::<Vec<_>>();
    for process_group_id in groups {
        if let Ok(process_group_id) = libc::pid_t::try_from(process_group_id) {
            // SAFETY: the id is a child spawned with `process_group(0)` whose
            // run is still in flight. A negative pid names that group, never
            // Codewhale's own.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
struct WindowsJob {
    handle: HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn attach(child: std::os::windows::io::RawHandle) -> std::io::Result<Self> {
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
            AssignProcessToJobObject(job.handle, HANDLE(child)).map_err(windows_io_error)?;
        }
        Ok(job)
    }

    fn limit_process_memory(&self, bytes: u64) -> std::io::Result<()> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        // SAFETY: `limits` is live with matching size; the handle is live.
        unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(windows_io_error)
        }
    }

    fn clear_kill_on_close(&self) -> std::io::Result<()> {
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `limits` is live with matching size; the handle is live.
        unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(windows_io_error)
        }
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
pub(crate) fn windows_io_error(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error)
}

/// Test support: wait for `pid` to stop existing. `true` once it is gone.
/// A zombie counts as gone: it was killed and only awaits a reaper, which a
/// container whose PID 1 does not reap orphans may never provide.
#[cfg(all(any(test, feature = "test-support"), unix))]
pub fn wait_for_pid_exit(pid: libc::pid_t, within: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 || is_zombie(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            // Do not leak the fixture when the assertion is about to fail.
            // SAFETY: kill(2) dereferences no pointers.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Linux only (`/proc`); elsewhere launchd/init reaps orphans promptly.
#[cfg(all(any(test, feature = "test-support"), unix))]
fn is_zombie(pid: libc::pid_t) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Test support: read the pid a fixture wrote to `path`, waiting for it.
#[cfg(all(any(test, feature = "test-support"), unix))]
pub fn read_pid_file(path: &std::path::Path, within: std::time::Duration) -> libc::pid_t {
    let deadline = std::time::Instant::now() + within;
    loop {
        if let Some(pid) = parse_pid_file(path) {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture never wrote {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(all(any(test, feature = "test-support"), unix))]
fn parse_pid_file(path: &std::path::Path) -> Option<libc::pid_t> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Test support: drive `run` until the fixture has written its pid to `path`,
/// then drop `run`. No wall-clock guess about how long the fixture needs to
/// start; `run` finishing first is a fixture bug.
#[cfg(all(any(test, feature = "test-support"), unix))]
pub async fn drop_once_pid_written<F: std::future::Future>(
    run: F,
    path: &std::path::Path,
) -> libc::pid_t {
    let written = async {
        loop {
            if let Some(pid) = parse_pid_file(path) {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::select! {
            _ = run => panic!("fixture exited before writing {}", path.display()),
            pid = written => pid,
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture never wrote {}", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn failed_containment_attachment_is_reported_and_kills_the_child() {
        let pid = std::cell::Cell::new(None);
        let mut cmd = tokio::process::Command::new("/bin/sleep");
        cmd.arg("300");
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            contained_run(&mut cmd, None, std::future::pending(), |child| {
                pid.set(child.id());
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected containment attachment failure",
                ))
            }),
        )
        .await;
        let pid = libc::pid_t::try_from(pid.get().expect("child spawned")).expect("pid");
        // Let Tokio reap while checking cleanup; the helper kills the fixture
        // itself if the child ever outlives this failed run.
        assert!(
            tokio::task::spawn_blocking(move || wait_for_pid_exit(pid, Duration::from_secs(5)))
                .await
                .expect("cleanup probe"),
            "the direct child outlived failed containment attachment"
        );
        let Err(error) = result
            .expect("attachment failure must return without running the command to completion")
        else {
            panic!("a containment failure must not be accepted as a successful run");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "injected containment attachment failure");
    }

    #[tokio::test]
    async fn dropping_contained_output_kills_the_whole_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_file = tmp.path().join("grandchild.pid");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 300 & echo $! > grandchild.pid; wait")
            .current_dir(tmp.path());
        let grandchild = drop_once_pid_written(contained_output(&mut cmd), &pid_file).await;
        assert!(
            wait_for_pid_exit(grandchild, Duration::from_secs(5)),
            "a process started by the dropped command is still running"
        );
    }

    #[tokio::test]
    async fn contained_output_captures_output_and_closes_stdin() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("cat; echo done");
        let output = tokio::time::timeout(Duration::from_secs(10), contained_output(&mut cmd))
            .await
            .expect("stdin must be closed, not inherited")
            .expect("run");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "done\n");
    }

    /// The input reaches stdin, and stdin is closed after it.
    #[tokio::test]
    async fn contained_output_with_input_feeds_then_closes_stdin() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("cat; echo done");
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            contained_output_with_input(&mut cmd, b"request\n".to_vec()),
        )
        .await
        .expect("stdin must be closed after the input")
        .expect("run");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "request\ndone\n");
    }

    /// A command that exits on its own is not reaped along with what it
    /// deliberately left running, as with a bare `output()`.
    #[tokio::test]
    async fn contained_output_leaves_a_detached_daemon_of_a_clean_exit_running() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 300 >/dev/null 2>&1 & echo $! > daemon.pid")
            .current_dir(tmp.path());
        let output = contained_output(&mut cmd).await.expect("run");
        assert!(output.status.success());
        let daemon = read_pid_file(&tmp.path().join("daemon.pid"), Duration::from_secs(5));
        // `wait_for_pid_exit` kills the fixture when it times out.
        assert!(
            !wait_for_pid_exit(daemon, Duration::from_millis(500)),
            "the daemon a clean command left behind was killed"
        );
    }

    /// `stop` kills the tree but keeps what the command already wrote.
    #[tokio::test]
    async fn stopped_contained_output_keeps_partial_output_and_kills_the_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("echo before-stop; sleep 300 & echo $! > grandchild.pid; wait")
            .current_dir(tmp.path());
        let pid_file = tmp.path().join("grandchild.pid");
        let stop = async {
            while parse_pid_file(&pid_file).is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let run = tokio::time::timeout(
            Duration::from_secs(60),
            contained_output_until(&mut cmd, stop),
        )
        .await
        .expect("stop must end the run")
        .expect("run");
        assert!(run.stopped);
        assert_eq!(String::from_utf8_lossy(&run.output.stdout), "before-stop\n");
        let grandchild = read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(
            wait_for_pid_exit(grandchild, Duration::from_secs(5)),
            "a process started by the stopped command is still running"
        );
    }

    /// An in-flight run is listed for the signal-exit kill, and unlisted once
    /// the future is gone.
    #[tokio::test]
    async fn in_flight_contained_run_is_listed_for_signal_exit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("echo $$ > leader.pid; sleep 300")
            .current_dir(tmp.path());
        let pid_file = tmp.path().join("leader.pid");
        let mut run = Box::pin(contained_output(&mut cmd));
        let leader = drop_once_pid_written(
            async {
                run.as_mut().await.ok();
            },
            &pid_file,
        )
        .await;
        let leader_group = u32::try_from(leader).expect("pid");
        assert!(contained_exit_groups().contains(&leader_group));
        drop(run);
        assert!(!contained_exit_groups().contains(&leader_group));
        // Let this current-thread runtime poll Tokio's orphan reaper while the
        // cleanup probe waits; kill(pid, 0) still sees an unreaped macOS child.
        assert!(
            tokio::task::spawn_blocking(move || wait_for_pid_exit(leader, Duration::from_secs(5)))
                .await
                .expect("cleanup probe")
        );
    }
}

#[cfg(all(test, unix))]
mod admitted_execution_tests {
    use super::*;
    #[tokio::test]
    async fn bounded_script_driver_refuses_overflow_without_deadlock() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "while :; do printf 1234567890; done"]);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            contained_output_with_input_bounded(
                &mut command,
                Vec::new(),
                64,
                64,
                std::future::pending(),
            ),
        )
        .await
        .expect("overflow must not wait for execution deadline");
        assert!(result.is_err());
    }
    #[tokio::test]
    async fn bounded_script_driver_feeds_and_drains_concurrently() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf answer; cat >/dev/null"]);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            contained_output_with_input_bounded(
                &mut command,
                vec![b'x'; 256 * 1024],
                64,
                64,
                std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!result.stopped);
        assert!(result.output.status.success());
        assert_eq!(result.output.stdout, b"answer");
    }
}

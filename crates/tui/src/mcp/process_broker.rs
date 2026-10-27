//! Reviewed stdio process ownership, extracted from `mcp/stdio.rs`.
//!
//! Both the Rust MCP transport and the selected SDK broker use this lifetime
//! owner. This session owns spawn,
//! scrubbed environment, reviewed executable handles, stdin, bounded stderr,
//! authority cancellation, process-tree containment and graceful teardown.
//! `StdioTransport` owns only stdout framing and its cancellation-safe buffer.
//!
//! It accepts only the existing Rust MCP configuration. The SDK broker checks
//! Rust-issued launch and exact-operation authority before exposing its pipe
//! operations to the pinned builtin; plugins cannot use this process surface.
//! Protocol negotiation, catalog admission, tool approvals and call replay
//! policy remain in their owners.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex as TokioMutex;

use super::McpServerConfig;
use codewhale_runtime::child_env;

pub(crate) struct BrokerSession {
    child: Arc<TokioMutex<Child>>,
    stdin: ChildStdin,
    stderr_tail: Arc<StderrTail>,
    /// Idle reviewed children die when their connection authority is revoked.
    authority_cancel_watch: Option<tokio::task::JoinHandle<()>>,
    /// Keep the reviewed executable/script descriptors alive through cleanup.
    _reviewed_launch: Option<super::ReviewedStdioLaunch>,
    /// Owned until Drop transfers it to the bounded asynchronous cleanup.
    process_tree: Option<Arc<codewhale_runtime::process_tree::ProcessTree>>,
}

/// How long `BrokerSession::shutdown` waits for the child to exit on SIGTERM
/// before `kill_on_drop` fires SIGKILL. Tuned short so a hung MCP server
/// can't stall TUI exit; well-behaved servers almost always exit within
/// a few hundred ms.
pub(super) const STDIO_SHUTDOWN_GRACE: Duration = Duration::from_millis(2_000);

/// How many lines of MCP-server stderr to keep around for crash diagnostics.
/// Bounded so a chatty server can't grow this without limit; large enough to
/// catch typical Node/Python startup or panic output.
const STDERR_TAIL_CAPACITY: usize = 64;

/// Bounded ring buffer for the most recent stderr lines from a spawned MCP
/// server. Owned by `BrokerSession` to surface server-side context when the
/// transport read side fails (server crashed, exited early, etc).
#[derive(Default)]
struct StderrTail {
    lines: TokioMutex<VecDeque<String>>,
}

impl StderrTail {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lines: TokioMutex::new(VecDeque::with_capacity(STDERR_TAIL_CAPACITY)),
        })
    }

    async fn push(&self, line: String) {
        let mut buf = self.lines.lock().await;
        if buf.len() >= STDERR_TAIL_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.lines.lock().await.iter().cloned().collect()
    }

    async fn last_line(&self) -> Option<String> {
        self.lines
            .lock()
            .await
            .iter()
            .rev()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }
}

impl BrokerSession {
    pub(crate) fn spawn(
        server_name: &str,
        command: &str,
        config: &McpServerConfig,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(Self, ChildStdout)> {
        let reviewed_launch = if let Some(reviewed_plugin) = config.reviewed_plugin.as_ref() {
            // This is deliberately the last trust check before constructing
            // and spawning the lazy stdio child. It re-reads only the
            // Codewhale-owned plugin bundle, never user MCP/provider config or
            // credential files, and fails closed on any content/capability
            // drift after pool construction.
            Some(reviewed_plugin.prepare_stdio_launch(
                server_name,
                command,
                &config.args,
                config.cwd.as_deref(),
            )?)
        } else {
            None
        };
        // Windows: Path::canonicalize yields a verbatim prefix. Node's ESM loader
        // (and several other Windows tools) refuse that spelling for ordinary
        // paths under MAX_PATH, so the reviewed peer exits 1 before handshake.
        // Strip only at the child boundary; containment kept the prefixed form.
        let mut cmd = reviewed_launch.as_ref().map_or_else(
            || {
                let mut command_process =
                    tokio::process::Command::new(super::strip_windows_verbatim_for_child(command));
                for arg in &config.args {
                    command_process.arg(super::strip_windows_verbatim_for_child(arg.as_str()));
                }
                command_process
            },
            |launch| {
                let mut command_process = tokio::process::Command::new(
                    super::strip_windows_verbatim_for_child(&launch.command),
                );
                for arg in &launch.args {
                    command_process.arg(super::strip_windows_verbatim_for_child(arg));
                }
                command_process
            },
        );
        codewhale_util::utils::suppress_tokio_console_window(&mut cmd);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let launch_cwd = reviewed_launch
            .as_ref()
            .and_then(|launch| launch.cwd.as_ref())
            .or(config.cwd.as_ref().filter(|_| reviewed_launch.is_none()));
        if let Some(cwd) = launch_cwd {
            cmd.current_dir(super::strip_windows_verbatim_for_child(cwd.as_os_str()));
        }
        #[cfg(unix)]
        if let Some(cwd_fd) = reviewed_launch
            .as_ref()
            .and_then(|launch| launch.cwd_fd.as_ref())
        {
            use std::os::fd::AsRawFd as _;
            let fd = cwd_fd.as_raw_fd();
            // SAFETY: the closure calls only async-signal-safe `fchdir` on an
            // inherited directory descriptor before exec.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::fchdir(fd) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
        }

        // Expand `${NAME}` placeholders so secret env values can be sourced
        // from the process environment instead of being stored in cleartext
        // in the MCP config. The child env is allowlist-sanitized below, so
        // these vars would not otherwise be inherited by the child.
        let expanded_env = super::expanded_mcp_stdio_env(config)
            .with_context(|| format!("MCP server '{server_name}' env expansion failed"))?;

        // User-configured MCP keeps the compatibility-oriented Node/Python
        // bootstrap allowlist (#1244). Reviewed plugins receive only the base
        // secret-scrubbed child environment plus their explicitly reviewed
        // mappings, so namespaces such as NPM_CONFIG_* are never inherited
        // ambiently across the consent boundary.
        if let Some(reviewed_plugin) = config.reviewed_plugin.as_ref() {
            cmd.env_clear();
            for (key, value) in child_env::sanitized_plugin_mcp_env_from(
                reviewed_plugin.host_environment.entries().iter().cloned(),
                child_env::string_map_env(&expanded_env),
            ) {
                cmd.env(key, value);
            }
        } else {
            child_env::apply_to_tokio_command_mcp(
                &mut cmd,
                child_env::string_map_env(&expanded_env),
            );
        }
        // Lead a process group of its own so teardown can reach descendants.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|error| {
            let message = if error.kind() == std::io::ErrorKind::NotFound
                && super::is_node_command(command)
                && launch_cwd.is_none_or(|directory| directory.is_dir())
            {
                format!("MCP server {server_name} could not start because Node.js was not found. Install Node.js from https://nodejs.org/ and restart Codewhale with node on PATH. Computer Use requires Node.js 20 or newer.")
            } else if config.reviewed_plugin.is_some() {
                format!(
                    "MCP stdio spawn failed (transport=stdio server={server_name} reviewed-plugin argv_count={} env_count={})",
                    config.args.len(),
                    expanded_env.len(),
                )
            } else {
                let env_keys: Vec<&str> = expanded_env.keys().map(String::as_str).collect();
                format!(
                    "MCP stdio spawn failed (transport=stdio server={server_name} cmd={command:?} args={:?} env_keys={env_keys:?})",
                    config.args,
                )
            };
            anyhow::Error::new(error).context(message)
        })?;

        let process_tree = match codewhale_runtime::process_tree::ProcessTree::attach_tokio(&child)
        {
            Ok(tree) => Arc::new(tree),
            Err(error) => {
                let _ = child.start_kill();
                return Err(anyhow::Error::new(error).context(format!(
                    "MCP server {server_name} could not be contained with its child processes"
                )));
            }
        };

        let stdin = child.stdin.take().context("Failed to get MCP stdin")?;
        let stdout = child.stdout.take().context("Failed to get MCP stdout")?;
        let stderr = child.stderr.take().context("Failed to get MCP stderr")?;

        // Drain stderr into a bounded ring buffer so a crash mid-run leaves
        // diagnostic breadcrumbs instead of disappearing into `Stdio::null`.
        // The task exits naturally when the child closes its stderr
        // (kill_on_drop / exit / explicit shutdown).
        let stderr_tail = StderrTail::new();
        {
            let tail = Arc::clone(&stderr_tail);
            // A reviewed plugin child receives environment-backed values that
            // are intentionally absent from its manifest. Still drain its
            // stderr to avoid blocking, but do not retain or surface arbitrary
            // child output that could echo those credentials into a chat or
            // persisted transcript.
            let capture = config.reviewed_plugin.is_none().then_some(tail);
            tokio::spawn(drain_stderr(stderr, capture));
        }

        let child = Arc::new(TokioMutex::new(child));
        let authority_cancel_watch = config.reviewed_plugin.as_ref().map(|_| {
            let watched_child = Arc::clone(&child);
            let watched_tree = Arc::clone(&process_tree);
            tokio::spawn(async move {
                cancel_token.cancelled().await;
                terminate_child_for_authority_change(&watched_child, &watched_tree).await;
            })
        });

        Ok((
            Self {
                child,
                stdin,
                stderr_tail,
                authority_cancel_watch,
                _reviewed_launch: reviewed_launch,
                process_tree: Some(process_tree),
            },
            stdout,
        ))
    }
}

impl BrokerSession {
    /// Write already-framed bytes; the transport owns the framing policy.
    pub(crate) async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stdin.write_all(bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub(crate) async fn last_stderr_line(&self) -> Option<String> {
        // The child can write its reason immediately before the error reply.
        tokio::task::yield_now().await;
        if let Some(line) = self.stderr_tail.last_line().await {
            return Some(line);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.stderr_tail.last_line().await
    }

    pub(crate) async fn stderr_context(&self) -> Option<String> {
        format_stderr_context(&self.stderr_tail).await
    }

    pub(crate) async fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.child.lock().await.try_wait().ok().flatten()
    }

    /// Never await or spawn for readiness; a contended child still reads live.
    pub(crate) fn probe_dead(&self) -> bool {
        match self.child.try_lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(Some(_))),
            Err(_) => false,
        }
    }

    /// Reap the direct child and contain descendants through the same grace.
    pub(crate) async fn shutdown(&mut self) {
        let mut child = self.child.lock().await;
        terminate_child(&mut child, self.process_tree.as_deref()).await;
    }

    #[cfg(all(test, unix))]
    pub(super) fn child_for_tests(&self) -> Arc<TokioMutex<Child>> {
        Arc::clone(&self.child)
    }
}

/// Longest stderr line retained in the tail, in bytes after decoding. An
/// overlong line keeps its end, where the error usually is, and the start is
/// drained and discarded so a newline-free progress stream cannot grow memory.
const STDERR_LINE_CAP: usize = 4 * 1024;

/// Drain a child's stderr until EOF, retaining lossily decoded, length-capped
/// lines in `tail` when given. Stderr is not a protocol channel: a non-UTF-8
/// byte or an endless line must never stop the drain, because dropping the
/// pipe makes the child's next stderr write fail (EPIPE/SIGPIPE) and hides
/// the context this tail exists to keep.
async fn drain_stderr<R>(stderr: R, tail: Option<Arc<StderrTail>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        let (consumed, line_ended) = {
            let available = match reader.fill_buf().await {
                Ok(available) => available,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if available.is_empty() {
                break;
            }
            let (consumed, line_ended) = match available.iter().position(|&b| b == b'\n') {
                Some(pos) => (pos + 1, true),
                None => (available.len(), false),
            };
            if tail.is_some() {
                let content = &available[..consumed - usize::from(line_ended)];
                let keep = content.len().min(STDERR_LINE_CAP);
                let overflow = (line.len() + keep).saturating_sub(STDERR_LINE_CAP);
                line.drain(..overflow);
                line.extend_from_slice(&content[content.len() - keep..]);
            }
            (consumed, line_ended)
        };
        reader.consume(consumed);
        if line_ended {
            push_stderr_line(tail.as_deref(), &mut line).await;
        }
    }
    if !line.is_empty() {
        push_stderr_line(tail.as_deref(), &mut line).await;
    }
}

async fn push_stderr_line(tail: Option<&StderrTail>, line: &mut Vec<u8>) {
    if let Some(tail) = tail {
        let bytes: &[u8] = line;
        let text = String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes));
        // Lossy decoding can triple invalid bytes; hold the retained size to
        // the cap too, again keeping the end.
        let mut start = text.len().saturating_sub(STDERR_LINE_CAP);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        tail.push(text[start..].to_owned()).await;
    }
    line.clear();
}

/// Format the captured stderr tail for inclusion in an error message. Empty
/// tails return `None` so the caller can fall back to its original message.
async fn format_stderr_context(tail: &StderrTail) -> Option<String> {
    let lines = tail.snapshot().await;
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "MCP server stderr (last {} line{}):\n{}",
        lines.len(),
        if lines.len() == 1 { "" } else { "s" },
        lines.join("\n"),
    ))
}

/// Best-effort SIGTERM. On Unix uses `libc::kill`, addressed to the child's
/// whole process group when it leads one (`contained`); on Windows there's no
/// equivalent so we let `kill_on_drop` (TerminateProcess) and the Job Object
/// handle it. Returns whether a signal was actually sent.
fn send_sigterm(child: &Child, contained: bool) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let pid = pid as i32;
            let target = if contained { -pid } else { pid };
            // SAFETY: pid was just obtained from `child.id()` of an unreaped
            // child, so neither it nor the group it leads can have been
            // recycled. `libc::kill` with `SIGTERM` is async-signal-safe and
            // never observes invalid memory. ESRCH is deliberately ignored.
            unsafe {
                let _ = libc::kill(target, libc::SIGTERM);
            }
            return true;
        }
        false
    }
    #[cfg(not(unix))]
    {
        let _ = (child, contained);
        false
    }
}

async fn terminate_child_for_authority_change(
    child: &Arc<TokioMutex<Child>>,
    tree: &codewhale_runtime::process_tree::ProcessTree,
) {
    let mut child = child.lock().await;
    terminate_child(&mut child, Some(tree)).await;
}

async fn terminate_child(
    child: &mut Child,
    tree: Option<&codewhale_runtime::process_tree::ProcessTree>,
) {
    // Reap an already-exited child before resolving its PID. Until it is
    // reaped, the OS cannot recycle that identity; after it is reaped there is
    // nothing left to signal. This avoids a PID-only watcher ever targeting an
    // unrelated process after rapid PID reuse.
    if child.try_wait().is_ok_and(|status| status.is_some()) {
        // The server is gone, but what it started may not be.
        if let Some(tree) = tree {
            let _ = tree.kill();
        }
        return;
    }

    #[cfg(unix)]
    send_sigterm(child, tree.is_some());

    #[cfg(not(unix))]
    let _ = child.start_kill();

    match tokio::time::timeout(STDIO_SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) | Err(_) => {
            // SIGTERM is advisory. Revocation and explicit shutdown must not
            // leave the reviewed child alive indefinitely.
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
    // Descendants that outlived the grace (or ignored SIGTERM) go now.
    if let Some(tree) = tree {
        let _ = tree.kill();
    }
}

/// Session changes can drop a pool without explicitly awaiting shutdown.
/// Keep the owned child alive for the same bounded cleanup as explicit
/// shutdown, so servers can release input and recording resources. Runtime
/// teardown still drops the cleanup future and invokes `kill_on_drop`.
impl Drop for BrokerSession {
    fn drop(&mut self) {
        if let Some(watch) = self.authority_cancel_watch.take() {
            watch.abort();
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let child = Arc::clone(&self.child);
            let reviewed_launch = self._reviewed_launch.take();
            // The task owns the tree so it is not killed before the grace.
            let tree = self.process_tree.take();
            runtime.spawn(async move {
                let _reviewed_launch = reviewed_launch;
                let mut child = child.lock().await;
                terminate_child(&mut child, tree.as_deref()).await;
            });
            return;
        }
        if let Ok(mut child) = self.child.try_lock()
            && !child.try_wait().is_ok_and(|status| status.is_some())
        {
            send_sigterm(&child, self.process_tree.is_some());
        }
        // No runtime: dropping the tree below SIGKILLs the group / closes the
        // job, the same backstop `kill_on_drop` gives the direct child.
    }
}

#[cfg(test)]
mod stderr_drain_tests {
    use super::{STDERR_LINE_CAP, StderrTail, drain_stderr};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn invalid_utf8_and_long_lines_do_not_stop_the_drain() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let tail = StderrTail::new();
        let drain = tokio::spawn(drain_stderr(reader, Some(tail.clone())));
        writer.write_all(b"starting\r\n").await.unwrap();
        writer.write_all(b"caf\xe9 \xff\xfe\n").await.unwrap();
        writer
            .write_all(&vec![b'#'; STDERR_LINE_CAP * 3])
            .await
            .unwrap();
        writer.write_all(b"\npanic: boom").await.unwrap();
        drop(writer);
        drain.await.unwrap();
        let lines = tail.snapshot().await;
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0], "starting");
        assert_eq!(lines[1], "caf\u{fffd} \u{fffd}\u{fffd}");
        assert_eq!(lines[2].len(), STDERR_LINE_CAP);
        assert_eq!(lines[3], "panic: boom");
    }

    #[tokio::test]
    async fn overlong_lines_keep_their_end_within_the_cap() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let tail = StderrTail::new();
        let drain = tokio::spawn(drain_stderr(reader, Some(tail.clone())));
        // A long prefix, then the actual failure at the end of the line.
        writer
            .write_all(&vec![b'.'; STDERR_LINE_CAP * 3])
            .await
            .unwrap();
        writer.write_all(b"error: config missing\n").await.unwrap();
        // Invalid bytes decode to three-byte U+FFFD each.
        writer
            .write_all(&vec![0xff; STDERR_LINE_CAP])
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        drop(writer);
        drain.await.unwrap();
        let lines = tail.snapshot().await;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].ends_with("error: config missing"),
            "{:?}",
            &lines[0][lines[0].len() - 40..]
        );
        assert_eq!(lines[0].len(), STDERR_LINE_CAP);
        assert!(lines[1].len() <= STDERR_LINE_CAP, "{}", lines[1].len());
        assert!(lines[1].chars().all(|c| c == '\u{fffd}'));
    }

    #[tokio::test]
    async fn uncaptured_stderr_is_still_drained_to_eof() {
        let (mut writer, reader) = tokio::io::duplex(16);
        let drain = tokio::spawn(drain_stderr(reader, None));
        // A non-UTF-8 line, then far more than the pipe buffer: a drain that
        // stopped at the bad line would fail this write with a broken pipe.
        writer.write_all(b"\xff\n").await.unwrap();
        writer.write_all(&[b'x'; 4096]).await.unwrap();
        drop(writer);
        drain.await.unwrap();
    }
}

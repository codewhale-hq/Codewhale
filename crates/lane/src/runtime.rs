//! Runtime backends: tmux durability and inline (#4176). The never-implemented
//! `vm` / `ci` kinds are retired and only load from historical records (#6516).
//!
//! Runtime owns process/session lifecycle and stream-json log capture.
//! Fleet modules must not import this module.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::registry::{LaneRecord, LaneRegistry, LaneStatus, TerminalTransition};
use crate::worktree::{WorktreeProvision, provision_worktree, remove_worktree_if_expired};

/// Execution backend for a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBackendKind {
    Tmux,
    Inline,
    /// Retired (#6516): `vm` and `ci` were advertised but never implemented.
    /// [`Self::parse`] no longer accepts them; the variants stay only so lane
    /// records written by earlier releases (always `failed`) still load
    /// instead of being skipped as corrupt.
    Vm,
    Ci,
}

impl RuntimeBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Inline => "inline",
            Self::Vm => "vm",
            Self::Ci => "ci",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "tmux" => Ok(Self::Tmux),
            "inline" => Ok(Self::Inline),
            "vm" | "ci" => bail!(
                "runtime backend `{}` is not available; use tmux or inline",
                raw.trim().to_ascii_lowercase()
            ),
            other => bail!("unknown runtime backend `{other}` (use tmux|inline)"),
        }
    }
}

/// Inputs for starting a lane under a runtime backend.
#[derive(Clone)]
pub struct LaneStartSpec {
    /// Command argv to run inside the backend (e.g. `codewhale exec …`).
    pub command: Vec<String>,
    /// Working directory for the command (defaults to worktree or cwd).
    pub cwd: Option<PathBuf>,
    /// Process-local runtime overrides. Values are never written into the
    /// Lane record or command argv; tmux bridges them through a private 0600
    /// environment file that the detached shell removes before execution.
    pub environment: Vec<(String, String)>,
    /// Executable that exposes Codewhale's hidden `lane-log-proxy` command.
    /// Required by tmux so arbitrary/binary child output is framed as valid
    /// NDJSON without trusting a shell pipeline.
    pub log_proxy: Option<PathBuf>,
    /// When set, provision an isolated git worktree + branch under this repo.
    pub worktree: Option<WorktreeProvision>,
}

impl std::fmt::Debug for LaneStartSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneStartSpec")
            .field("command", &self.command)
            .field("cwd", &self.cwd)
            .field(
                "environment_keys",
                &self
                    .environment
                    .iter()
                    .map(|(key, _)| key)
                    .collect::<Vec<_>>(),
            )
            .field("log_proxy", &self.log_proxy)
            .field("worktree", &self.worktree)
            .finish()
    }
}

/// Runtime adapter contract.
pub trait RuntimeBackend {
    fn kind(&self) -> RuntimeBackendKind;

    /// Start the lane process/session; mutates record with attach/log metadata.
    fn start(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        spec: &LaneStartSpec,
    ) -> Result<()>;

    /// Human attach command, if any (tmux).
    fn attach_command(&self, record: &LaneRecord) -> Option<String>;

    /// Stop the running session/process.
    ///
    /// `fence` pins the durable lifecycle generation the caller observed. It is
    /// evaluated under the registry's per-Lane lock, so a record that moved on
    /// between the caller's read and this call is refused rather than stopped.
    /// The return value distinguishes "this call stopped it" from "it was
    /// already terminal" and "the fence did not match", so a caller can report
    /// a truthful outcome instead of claiming a transition someone else made.
    fn stop(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        fence: Option<u64>,
    ) -> Result<TerminalTransition>;

    /// Reconcile durable backend state into the Lane record before display.
    /// Most backends update synchronously and need no refresh; tmux records
    /// the detached process exit in a private sidecar and folds it in on the
    /// next read.
    ///
    /// Returns `true` when reconciliation *changed durable state*. Reads are
    /// allowed to fold a finished Runtime exit into the record, but a surface
    /// that calls this must be able to say so rather than reporting a pure
    /// observation (#4022).
    fn reconcile(&self, _registry: &LaneRegistry, _record: &mut LaneRecord) -> Result<bool> {
        Ok(false)
    }

    /// Optional worktree TTL cleanup after stop.
    fn cleanup_worktree(&self, record: &LaneRecord) -> Result<()> {
        if let Some(path) = record.worktree_path.as_ref() {
            remove_worktree_if_expired(
                path,
                record.worktree_ttl_secs,
                record.stopped_at.as_deref(),
            )?;
        }
        Ok(())
    }
}

pub fn resolve_backend(kind: RuntimeBackendKind) -> Box<dyn RuntimeBackend> {
    match kind {
        RuntimeBackendKind::Tmux => Box::new(TmuxRuntime),
        RuntimeBackendKind::Inline => Box::new(InlineRuntime),
        RuntimeBackendKind::Vm => Box::new(RetiredRuntime {
            kind: RuntimeBackendKind::Vm,
        }),
        RuntimeBackendKind::Ci => Box::new(RetiredRuntime {
            kind: RuntimeBackendKind::Ci,
        }),
    }
}

pub fn backend_for(record: &LaneRecord) -> Box<dyn RuntimeBackend> {
    resolve_backend(record.runtime)
}

fn append_log_event(log_path: &Path, event: serde_json::Value) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(log_path)
        .with_context(|| format!("open lane log {}", log_path.display()))?;
    let mut encoded = serde_json::to_vec(&event).context("serialize lane log event")?;
    encoded.push(b'\n');
    file.write_all(&encoded)
        .with_context(|| format!("write lane log {}", log_path.display()))?;
    Ok(())
}

const MAX_EXIT_RECEIPT_BYTES: u64 = 4 * 1024;
const MAX_ENVIRONMENT_BYTES: u64 = 1024 * 1024;
const MAX_CHILD_LOG_FRAME_BYTES: usize = 64 * 1024;
const LANE_PROXY_FAILURE_EXIT_CODE: i32 = 125;

#[derive(Debug, Deserialize, Serialize)]
struct LaneExitReceipt {
    lane_id: String,
    exit_code: i32,
}

/// Inputs to the hidden Rust log proxy used by detached runtimes.
///
/// The proxy is deliberately exposed by the Lane crate so the thin CLI
/// facade can invoke it before loading user configuration. Secrets travel in
/// the private environment file, never in argv or the Lane record.
#[derive(Debug, Clone)]
pub struct LaneLogProxySpec {
    pub command: Vec<String>,
    pub log_path: PathBuf,
    pub receipt_path: PathBuf,
    pub receipt_tmp_path: PathBuf,
    pub environment_path: Option<PathBuf>,
    pub lane_id: String,
}

fn lane_exit_receipt_path(log_path: &Path) -> PathBuf {
    log_path.with_extension("exit.json")
}

fn lane_exit_receipt_tmp_path(log_path: &Path) -> PathBuf {
    log_path.with_extension("exit.json.tmp")
}

fn lane_environment_path(log_path: &Path) -> PathBuf {
    log_path.with_extension("env.json")
}

fn lane_environment_tmp_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn valid_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn write_lane_environment(path: &Path, environment: &[(String, String)]) -> Result<()> {
    for (key, _) in environment {
        if !valid_environment_key(key) {
            bail!("invalid lane environment key {key:?}");
        }
    }
    let encoded = serde_json::to_vec(environment).context("serialize private lane environment")?;
    if encoded.len() as u64 > MAX_ENVIRONMENT_BYTES {
        bail!(
            "private lane environment exceeds {} bytes",
            MAX_ENVIRONMENT_BYTES
        );
    }

    let tmp_path = lane_environment_tmp_path(path);
    remove_file_if_present(path)?;
    remove_file_if_present(&tmp_path)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&tmp_path)
            .with_context(|| format!("create private lane environment {}", tmp_path.display()))?;
        file.write_all(&encoded)
            .with_context(|| format!("write private lane environment {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync private lane environment {}", tmp_path.display()))?;
        fs::rename(&tmp_path, path)
            .with_context(|| format!("publish private lane environment {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_file_if_present(&tmp_path);
        let _ = remove_file_if_present(path);
    }
    result
}

fn append_child_output(log_path: &Path, stream: &str, bytes: &[u8]) -> Result<()> {
    let mut line = bytes;
    if let Some(stripped) = line.strip_suffix(b"\n") {
        line = stripped;
    }
    if let Some(stripped) = line.strip_suffix(b"\r") {
        line = stripped;
    }
    if line.is_empty() {
        return Ok(());
    }
    if let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) {
        append_log_event(log_path, event)
    } else {
        append_log_event(
            log_path,
            serde_json::json!({
                "type": "lane_log",
                "stream": stream,
                "message": String::from_utf8_lossy(line),
            }),
        )
    }
}

fn stream_child_output(
    reader: impl std::io::Read,
    log_path: PathBuf,
    stream: &'static str,
) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut reached_eof = false;
        while line.len() < MAX_CHILD_LOG_FRAME_BYTES {
            let buffer = reader
                .fill_buf()
                .with_context(|| format!("read lane child {stream}"))?;
            if buffer.is_empty() {
                reached_eof = true;
                break;
            }
            let available = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |position| position + 1);
            let take = available.min(MAX_CHILD_LOG_FRAME_BYTES - line.len());
            let ended_line = buffer.get(take.saturating_sub(1)) == Some(&b'\n');
            line.extend_from_slice(&buffer[..take]);
            reader.consume(take);
            if ended_line {
                break;
            }
        }
        if line.is_empty() && reached_eof {
            return Ok(());
        }
        append_child_output(&log_path, stream, &line)?;
        if reached_eof {
            return Ok(());
        }
    }
}

fn read_lane_environment(path: &Path) -> Result<Vec<(String, String)>> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.len() > MAX_ENVIRONMENT_BYTES {
        bail!(
            "private lane environment {} exceeds {} bytes",
            path.display(),
            MAX_ENVIRONMENT_BYTES
        );
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() as u64 > MAX_ENVIRONMENT_BYTES {
        bail!(
            "private lane environment {} exceeds {} bytes",
            path.display(),
            MAX_ENVIRONMENT_BYTES
        );
    }
    let environment: Vec<(String, String)> =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    for (key, _) in &environment {
        if !valid_environment_key(key) {
            bail!("invalid lane environment key {key:?}");
        }
    }
    Ok(environment)
}

fn write_lane_exit_receipt(
    receipt_path: &Path,
    receipt_tmp_path: &Path,
    lane_id: &str,
    exit_code: i32,
) -> Result<()> {
    let encoded = serde_json::to_vec(&LaneExitReceipt {
        lane_id: lane_id.to_string(),
        exit_code,
    })
    .context("serialize lane exit receipt")?;
    if encoded.len() as u64 > MAX_EXIT_RECEIPT_BYTES {
        bail!("serialized lane exit receipt exceeds size bound");
    }
    remove_file_if_present(receipt_tmp_path)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(receipt_tmp_path)
            .with_context(|| format!("create {}", receipt_tmp_path.display()))?;
        file.write_all(&encoded)
            .with_context(|| format!("write {}", receipt_tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", receipt_tmp_path.display()))?;
        fs::rename(receipt_tmp_path, receipt_path)
            .with_context(|| format!("publish {}", receipt_path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_file_if_present(receipt_tmp_path);
    }
    result
}

fn exit_status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    LANE_PROXY_FAILURE_EXIT_CODE
}

fn append_proxy_failure(log_path: &Path, lane_id: &str, error: &anyhow::Error) -> Result<()> {
    append_log_event(
        log_path,
        serde_json::json!({
            "type": "lane_proxy_error",
            "lane_id": lane_id,
            "error": format!("{error:#}"),
        }),
    )
}

/// Run a child command while framing all output as NDJSON and atomically
/// publishing a private exit receipt. Returns the process-style exit code the
/// hidden CLI should propagate to tmux.
pub fn run_lane_log_proxy(spec: LaneLogProxySpec) -> Result<i32> {
    let LaneLogProxySpec {
        command,
        log_path,
        receipt_path,
        receipt_tmp_path,
        environment_path,
        lane_id,
    } = spec;

    let fail = |error: anyhow::Error, exit_code: i32| -> Result<i32> {
        append_proxy_failure(&log_path, &lane_id, &error)?;
        write_lane_exit_receipt(&receipt_path, &receipt_tmp_path, &lane_id, exit_code)?;
        Ok(exit_code)
    };

    if command.is_empty() {
        return fail(anyhow::anyhow!("lane log proxy requires a command"), 127);
    }

    let environment = if let Some(path) = environment_path.as_deref() {
        let loaded = read_lane_environment(path);
        let removed = remove_file_if_present(path);
        match (loaded, removed) {
            (Ok(environment), Ok(())) => environment,
            (Err(error), Ok(())) => return fail(error, LANE_PROXY_FAILURE_EXIT_CODE),
            (Ok(_), Err(error)) | (Err(_), Err(error)) => return Err(error),
        }
    } else {
        Vec::new()
    };

    let mut child_command = Command::new(&command[0]);
    child_command.args(&command[1..]);
    child_command.envs(environment);
    child_command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match child_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return fail(
                anyhow::Error::from(error).context(format!("spawn lane child command {command:?}")),
                127,
            );
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return fail(anyhow::anyhow!("lane child stdout was not piped"), 125),
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => return fail(anyhow::anyhow!("lane child stderr was not piped"), 125),
    };
    let stdout_log = log_path.clone();
    let stderr_log = log_path.clone();
    let stdout_thread = thread::spawn(move || stream_child_output(stdout, stdout_log, "stdout"));
    let stderr_thread = thread::spawn(move || stream_child_output(stderr, stderr_log, "stderr"));

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            return fail(
                anyhow::Error::from(error).context(format!("wait for lane child {command:?}")),
                LANE_PROXY_FAILURE_EXIT_CODE,
            );
        }
    };
    let stdout_result = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("lane proxy stdout logger panicked"))?;
    let stderr_result = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("lane proxy stderr logger panicked"))?;
    let mut exit_code = exit_status_code(status);
    if let Some(error) = stdout_result.err().or_else(|| stderr_result.err()) {
        append_proxy_failure(&log_path, &lane_id, &error)?;
        exit_code = LANE_PROXY_FAILURE_EXIT_CODE;
    }
    write_lane_exit_receipt(&receipt_path, &receipt_tmp_path, &lane_id, exit_code)?;
    Ok(exit_code)
}

fn read_lane_exit_receipt(log_path: &Path, lane_id: &str) -> Result<Option<LaneExitReceipt>> {
    let path = lane_exit_receipt_path(log_path);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.len() > MAX_EXIT_RECEIPT_BYTES {
        bail!(
            "lane exit receipt {} exceeds {} bytes",
            path.display(),
            MAX_EXIT_RECEIPT_BYTES
        );
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let receipt: LaneExitReceipt =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if receipt.lane_id != lane_id {
        bail!(
            "lane exit receipt {} belongs to {}, expected {}",
            path.display(),
            receipt.lane_id,
            lane_id
        );
    }
    Ok(Some(receipt))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TmuxSessionState {
    Present,
    Absent,
}

fn tmux_command(socket: &Path) -> Command {
    let mut command = Command::new("tmux");
    command.arg("-S").arg(socket);
    command
}

fn ensure_tmux_available() -> Result<()> {
    let output = Command::new("tmux")
        .arg("-V")
        .output()
        .context("tmux runtime requires the `tmux` executable")?;
    if !output.status.success() {
        bail!(
            "tmux runtime is unavailable: `tmux -V` failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn tmux_session_state(socket: &Path, session: &str) -> Result<TmuxSessionState> {
    let output = tmux_command(socket)
        .args(["has-session", "-t", session])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("query tmux session {session}"))?;
    if output.status.success() {
        return Ok(TmuxSessionState::Present);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("can't find session:")
        || stderr.contains("no server running on")
        || (stderr.contains("error connecting to") && stderr.contains("no such file or directory"))
    {
        return Ok(TmuxSessionState::Absent);
    }
    bail!(
        "tmux has-session for {session} failed with {}: {}",
        output.status,
        stderr.trim()
    )
}

fn stop_tmux_session(socket: &Path, session: &str) -> Result<()> {
    let status = tmux_command(socket)
        .args(["kill-session", "-t", session])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("kill tmux session {session}"))?;
    match tmux_session_state(socket, session).with_context(|| {
        format!(
            "confirm tmux session {session} on {} stopped after kill-session ({status})",
            socket.display()
        )
    })? {
        TmuxSessionState::Absent => {}
        TmuxSessionState::Present => {
            bail!("tmux session {session} remains active after kill-session ({status})")
        }
    }
    // A nonzero kill can be benign when the process and session exited in the
    // same instant. The explicit absence check above is the source of truth.
    Ok(())
}

fn tmux_log_proxy_command(
    proxy: &Path,
    command: &[String],
    log_path: &Path,
    receipt_path: &Path,
    receipt_tmp_path: &Path,
    environment_path: Option<&Path>,
    lane_id: &str,
) -> String {
    let mut argv = vec![
        proxy.display().to_string(),
        "lane-log-proxy".to_string(),
        "--log-path".to_string(),
        log_path.display().to_string(),
        "--receipt-path".to_string(),
        receipt_path.display().to_string(),
        "--receipt-tmp-path".to_string(),
        receipt_tmp_path.display().to_string(),
        "--lane-id".to_string(),
        lane_id.to_string(),
    ];
    if let Some(path) = environment_path {
        argv.push("--environment-path".to_string());
        argv.push(path.display().to_string());
    }
    argv.push("--".to_string());
    argv.extend(command.iter().cloned());
    format!("exec {}", shell_join(&argv))
}

/// The tmux dry-run hook, honoured only in this crate's own unit tests.
///
/// It records a Running lane with no process, skips the tmux kill on stop,
/// and blinds reconcile, so an inherited `CODEWHALE_LANE_TMUX_DRY_RUN` in a
/// real build would fabricate lane state. Release builds ignore it.
fn tmux_dry_run() -> bool {
    cfg!(test) && std::env::var_os("CODEWHALE_LANE_TMUX_DRY_RUN").is_some()
}

fn apply_worktree(record: &mut LaneRecord, spec: &LaneStartSpec) -> Result<Option<PathBuf>> {
    let Some(wt) = spec.worktree.as_ref() else {
        return Ok(spec.cwd.clone());
    };
    let provisioned = provision_worktree(wt)?;
    record.worktree_path = Some(provisioned.path.clone());
    record.branch = Some(provisioned.branch.clone());
    Ok(Some(provisioned.path))
}

fn fail_provisioned_start(
    backend: &dyn RuntimeBackend,
    registry: &LaneRegistry,
    record: &mut LaneRecord,
    error: anyhow::Error,
) -> anyhow::Error {
    let mut failed_record = record.clone();
    let _ = registry.mark_terminal_if_active(record, LaneStatus::Failed);
    failed_record.stopped_at = record.stopped_at.clone();
    let _ = backend.cleanup_worktree(&failed_record);
    error
}

fn prepare_tmux_launch(
    registry: &LaneRegistry,
    record: &mut LaneRecord,
    spec: &LaneStartSpec,
    cwd: Option<&Path>,
    session: &str,
    socket: &Path,
    dry_run: bool,
) -> Result<Option<(Command, Option<PathBuf>)>> {
    if spec.worktree.is_some() {
        registry.persist_worktree(record)?;
    }
    append_log_event(
        &record.log_path,
        serde_json::json!({
            "type": "lane_started",
            "lane_id": record.id,
            "runtime": "tmux",
            "session": session,
            "workflow": record.workflow,
            "fleet": record.fleet,
            "issue": record.issue,
            "dry_run": dry_run,
        }),
    )?;

    if dry_run {
        append_log_event(
            &record.log_path,
            serde_json::json!({
                "type": "lane_log",
                "message": "tmux dry-run: session recorded without spawning process",
                "command": spec.command,
                "cwd": cwd.map(|p| p.display().to_string()),
            }),
        )?;
        if !registry.mark_running_if_pending(record)? {
            bail!(
                "lane `{}` was stopped before tmux dry-run start completed",
                record.id
            );
        }
        return Ok(None);
    }

    let log_proxy = spec
        .log_proxy
        .as_deref()
        .context("tmux runtime requires a lane log proxy executable")?;

    // Detached session: child output remains an operator journal only.
    // Terminal control state goes through a separate bounded, atomically
    // renamed receipt so child stdout cannot forge Lane completion.
    let receipt_path = lane_exit_receipt_path(&record.log_path);
    let receipt_tmp_path = lane_exit_receipt_tmp_path(&record.log_path);
    let environment_path = lane_environment_path(&record.log_path);
    remove_file_if_present(&receipt_path)?;
    remove_file_if_present(&receipt_tmp_path)?;
    remove_file_if_present(&environment_path)?;
    let environment_path = if spec.environment.is_empty() {
        None
    } else {
        write_lane_environment(&environment_path, &spec.environment)?;
        Some(environment_path)
    };
    let shell_cmd = tmux_log_proxy_command(
        log_proxy,
        &spec.command,
        &record.log_path,
        &receipt_path,
        &receipt_tmp_path,
        environment_path.as_deref(),
        &record.id,
    );

    let mut cmd = tmux_command(socket);
    cmd.args(["new-session", "-d", "-s", session]);
    if let Some(cwd) = cwd {
        cmd.arg("-c").arg(cwd);
    }
    cmd.arg(shell_cmd);
    Ok(Some((cmd, environment_path)))
}

/// Durable local tmux sessions + attach + stream-json log file.
#[derive(Debug, Default)]
pub struct TmuxRuntime;

impl RuntimeBackend for TmuxRuntime {
    fn kind(&self) -> RuntimeBackendKind {
        RuntimeBackendKind::Tmux
    }

    fn start(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        spec: &LaneStartSpec,
    ) -> Result<()> {
        if spec.command.is_empty() {
            bail!("tmux runtime requires a non-empty command");
        }
        // Dry-run is an explicit test hook only. A missing/broken tmux binary
        // must fail closed rather than persisting a fictional Running Lane.
        let dry_run = tmux_dry_run();
        if !dry_run && let Err(error) = ensure_tmux_available() {
            append_log_event(
                &record.log_path,
                serde_json::json!({
                    "type": "lane_failed",
                    "lane_id": record.id,
                    "runtime": "tmux",
                    "error": error.to_string(),
                }),
            )?;
            let _ = registry.mark_terminal_if_active(record, LaneStatus::Failed)?;
            return Err(error);
        }

        let cwd = apply_worktree(record, spec)?;
        let session = format!("cw-{}", record.id);
        let socket = registry.root().join("tmux.sock");
        record.tmux_session = Some(session.clone());
        record.tmux_socket = Some(socket.clone());
        record.attach_target = Some(format!(
            "tmux -S {} attach -t {}",
            shell_escape(&socket.display().to_string()),
            shell_escape(&session)
        ));

        let prepared = match prepare_tmux_launch(
            registry,
            record,
            spec,
            cwd.as_deref(),
            &session,
            &socket,
            dry_run,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return Err(fail_provisioned_start(self, registry, record, error)),
        };
        let Some((mut cmd, environment_path)) = prepared else {
            return Ok(());
        };
        let proposed_record = record.clone();
        let spawned = std::cell::Cell::new(false);
        let rolled_back = std::cell::Cell::new(false);
        match registry.mark_running_if_pending_with(
            record,
            || {
                let status = cmd
                    .status()
                    .with_context(|| format!("spawn tmux session {session}"))?;
                if !status.success() {
                    bail!("tmux new-session failed with {status}");
                }
                spawned.set(true);
                Ok(())
            },
            || {
                stop_tmux_session(&socket, &session)?;
                rolled_back.set(true);
                Ok(())
            },
        ) {
            Ok(true) => {}
            Ok(false) => {
                if let Some(path) = environment_path.as_deref() {
                    remove_file_if_present(path)?;
                }
                let mut stopped_record = proposed_record;
                stopped_record.stopped_at = record.stopped_at.clone();
                self.cleanup_worktree(&stopped_record)?;
                bail!("lane `{}` was stopped before tmux launch", record.id);
            }
            Err(error) => {
                if let Some(path) = environment_path.as_deref() {
                    let _ = remove_file_if_present(path);
                }
                if !spawned.get() || rolled_back.get() {
                    let _ = registry.mark_terminal_if_active(record, LaneStatus::Failed)?;
                    let mut failed_record = proposed_record;
                    failed_record.stopped_at = record.stopped_at.clone();
                    self.cleanup_worktree(&failed_record)?;
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn attach_command(&self, record: &LaneRecord) -> Option<String> {
        if record.status != LaneStatus::Running {
            return None;
        }
        let socket = record.tmux_socket.as_ref()?;
        let session = record.tmux_session.as_deref()?;
        Some(format!(
            "tmux -S {} attach -t {}",
            shell_escape(&socket.display().to_string()),
            shell_escape(session)
        ))
    }

    fn stop(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        fence: Option<u64>,
    ) -> Result<TerminalTransition> {
        let dry_run = tmux_dry_run();
        let transition = registry.mark_terminal_if_active_fenced(
            record,
            LaneStatus::Stopped,
            fence,
            |current| {
                if !dry_run {
                    match (
                        current.tmux_socket.as_deref(),
                        current.tmux_session.as_deref(),
                    ) {
                        (Some(socket), Some(session)) => stop_tmux_session(socket, session)?,
                        _ if current.status == LaneStatus::Running => {
                            bail!(
                                "running tmux lane `{}` has incomplete pinned session metadata; refusing unsafe stop",
                                current.id
                            );
                        }
                        _ => {}
                    }
                }
                Ok(())
            },
        )?;
        if transition.transitioned() {
            append_log_event(
                &record.log_path,
                serde_json::json!({
                    "type": "lane_stopped",
                    "lane_id": record.id,
                    "session": record.tmux_session,
                }),
            )?;
            remove_file_if_present(&lane_environment_path(&record.log_path))?;
            self.cleanup_worktree(record)?;
        }
        Ok(transition)
    }

    fn reconcile(&self, registry: &LaneRegistry, record: &mut LaneRecord) -> Result<bool> {
        // Pending is the pre-launch state. Never reconcile it: a concurrent
        // status read must not race a fast `tmux new-session` and overwrite
        // the start path's later Running transition.
        if record.status != LaneStatus::Running {
            return Ok(false);
        }

        let receipt = read_lane_exit_receipt(&record.log_path, &record.id)?;
        let (lane_status, exit_code, reason) = if let Some(receipt) = receipt {
            (
                if receipt.exit_code == 0 {
                    LaneStatus::Completed
                } else {
                    LaneStatus::Failed
                },
                Some(receipt.exit_code),
                "process_exit_receipt",
            )
        } else {
            if tmux_dry_run() {
                return Ok(false);
            }
            let (Some(socket), Some(session)) = (
                record.tmux_socket.as_deref(),
                record.tmux_session.as_deref(),
            ) else {
                bail!(
                    "running tmux lane `{}` lacks pinned socket/session metadata; refusing unsafe reconciliation",
                    record.id
                );
            };
            match tmux_session_state(socket, session)? {
                TmuxSessionState::Present => return Ok(false),
                TmuxSessionState::Absent => {}
            }
            (
                LaneStatus::Failed,
                None,
                "tmux_session_missing_without_exit_receipt",
            )
        };

        if registry.mark_terminal_if_active(record, lane_status)? {
            append_log_event(
                &record.log_path,
                serde_json::json!({
                    "type": "lane_reconciled",
                    "lane_id": record.id,
                    "exit_code": exit_code,
                    "status": lane_status.as_str(),
                    "reason": reason,
                }),
            )?;
            remove_file_if_present(&lane_environment_path(&record.log_path))?;
            self.cleanup_worktree(record)?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// In-process / local command runtime (no tmux). Used for tests and headless.
#[derive(Debug, Default)]
pub struct InlineRuntime;

impl RuntimeBackend for InlineRuntime {
    fn kind(&self) -> RuntimeBackendKind {
        RuntimeBackendKind::Inline
    }

    fn start(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        spec: &LaneStartSpec,
    ) -> Result<()> {
        if spec.command.is_empty() {
            bail!("inline runtime requires a non-empty command");
        }
        let cwd = apply_worktree(record, spec)?;
        let proposed_record = record.clone();
        let announced = (|| -> Result<()> {
            if spec.worktree.is_some() {
                registry.persist_worktree(record)?;
            }
            append_log_event(
                &record.log_path,
                serde_json::json!({
                    "type": "lane_started",
                    "lane_id": record.id,
                    "runtime": "inline",
                    "command": spec.command,
                }),
            )
        })();
        if let Err(error) = announced {
            return Err(fail_provisioned_start(self, registry, record, error));
        }
        if !registry.mark_running_if_pending(record)? {
            let mut stopped_record = proposed_record;
            stopped_record.stopped_at = record.stopped_at.clone();
            self.cleanup_worktree(&stopped_record)?;
            bail!(
                "lane `{}` was stopped before inline start completed",
                record.id
            );
        }

        let mut cmd = Command::new(&spec.command[0]);
        if spec.command.len() > 1 {
            cmd.args(&spec.command[1..]);
        }
        if let Some(cwd) = cwd.as_ref() {
            cmd.current_dir(cwd);
        }
        cmd.envs(spec.environment.iter().map(|(key, value)| (key, value)));
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                append_log_event(
                    &record.log_path,
                    serde_json::json!({
                        "type": "lane_failed",
                        "lane_id": record.id,
                        "error": err.to_string(),
                    }),
                )?;
                let _ = registry.mark_terminal_if_active(record, LaneStatus::Failed)?;
                // Terminal now, so no later stop will clean the worktree up.
                self.cleanup_worktree(record)?;
                return Err(err).with_context(|| format!("run inline command {:?}", spec.command));
            }
        };
        let stdout = child
            .stdout
            .take()
            .context("inline lane child stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .context("inline lane child stderr was not piped")?;
        let stdout_log = record.log_path.clone();
        let stderr_log = record.log_path.clone();
        let stdout_thread =
            thread::spawn(move || stream_child_output(stdout, stdout_log, "stdout"));
        let stderr_thread =
            thread::spawn(move || stream_child_output(stderr, stderr_log, "stderr"));
        let status = child
            .wait()
            .with_context(|| format!("wait for inline command {:?}", spec.command))?;
        let stdout_result = stdout_thread
            .join()
            .map_err(|_| anyhow::anyhow!("inline stdout logger panicked"))?;
        let stderr_result = stderr_thread
            .join()
            .map_err(|_| anyhow::anyhow!("inline stderr logger panicked"))?;
        let logging_error = stdout_result.err().or_else(|| stderr_result.err());

        if status.success() && logging_error.is_none() {
            append_log_event(
                &record.log_path,
                serde_json::json!({"type": "lane_completed", "lane_id": record.id}),
            )?;
            let _ = registry.mark_terminal_if_active(record, LaneStatus::Completed)?;
        } else {
            append_log_event(
                &record.log_path,
                serde_json::json!({
                    "type": "lane_failed",
                    "lane_id": record.id,
                    "status": format!("{status}"),
                    "logging_error": logging_error.as_ref().map(ToString::to_string),
                }),
            )?;
            let _ = registry.mark_terminal_if_active(record, LaneStatus::Failed)?;
        }
        if let Some(err) = logging_error {
            return Err(err).context("stream inline lane output");
        }
        Ok(())
    }

    fn attach_command(&self, _record: &LaneRecord) -> Option<String> {
        None
    }

    fn stop(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        fence: Option<u64>,
    ) -> Result<TerminalTransition> {
        let transition = registry.mark_terminal_if_active_fenced(
            record,
            LaneStatus::Stopped,
            fence,
            |current| {
                if current.status == LaneStatus::Running {
                    bail!(
                        "inline lane `{}` cannot be stopped safely from another process",
                        current.id
                    );
                }
                Ok(())
            },
        )?;
        if transition.transitioned() {
            self.cleanup_worktree(record)?;
        }
        Ok(transition)
    }
}

/// Backend for a lane record written by an earlier release under the retired
/// `vm` / `ci` runtimes. It never starts anything: [`RuntimeBackendKind::parse`]
/// cannot produce these kinds, so only historical records reach it, and they
/// can still be listed, reconciled, and stopped.
#[derive(Debug)]
struct RetiredRuntime {
    kind: RuntimeBackendKind,
}

impl RuntimeBackend for RetiredRuntime {
    fn kind(&self) -> RuntimeBackendKind {
        self.kind
    }

    fn start(
        &self,
        _registry: &LaneRegistry,
        _record: &mut LaneRecord,
        _spec: &LaneStartSpec,
    ) -> Result<()> {
        bail!(
            "{} runtime is not available; use tmux or inline",
            self.kind.as_str()
        )
    }

    fn attach_command(&self, _record: &LaneRecord) -> Option<String> {
        None
    }

    fn stop(
        &self,
        registry: &LaneRegistry,
        record: &mut LaneRecord,
        fence: Option<u64>,
    ) -> Result<TerminalTransition> {
        registry.mark_terminal_if_active_fenced(record, LaneStatus::Stopped, fence, |_| Ok(()))
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    fn tmux_env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct ScopedEnvVar {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl ScopedEnvVar {
        fn set(name: &'static str, value: &std::ffi::OsStr) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: tmux environment tests hold `tmux_env_lock` and restore
            // the process environment in Drop.
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }

        #[cfg(unix)]
        fn remove(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: tmux environment tests hold `tmux_env_lock` and restore
            // the process environment in Drop.
            unsafe { std::env::remove_var(name) };
            Self { name, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            // SAFETY: paired with the serialized mutation above.
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var(self.name, previous);
                } else {
                    std::env::remove_var(self.name);
                }
            }
        }
    }

    #[test]
    fn tmux_dry_run_start_attach_stop_roundtrip() {
        let _env_guard = tmux_env_lock();
        let _dry_run = ScopedEnvVar::set("CODEWHALE_LANE_TMUX_DRY_RUN", std::ffi::OsStr::new("1"));
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let mut record = reg
            .create_pending(
                Some("stopship".into()),
                Some("stopship".into()),
                Some("4375".into()),
                None,
                RuntimeBackendKind::Tmux,
                None,
            )
            .unwrap();
        let backend = TmuxRuntime;
        backend
            .start(
                &reg,
                &mut record,
                &LaneStartSpec {
                    command: vec!["echo".into(), "hello-lane".into()],
                    cwd: None,
                    environment: Vec::new(),
                    log_proxy: None,
                    worktree: None,
                },
            )
            .unwrap();
        assert_eq!(record.status, LaneStatus::Running);
        assert!(record.tmux_session.is_some());
        let expected_socket = reg.root().join("tmux.sock");
        assert_eq!(
            record.tmux_socket.as_deref(),
            Some(expected_socket.as_path())
        );
        let attach = backend.attach_command(&record).expect("attach");
        assert!(attach.contains("tmux -S"));
        assert!(attach.contains(&expected_socket.display().to_string()));
        assert!(attach.contains("attach -t"));
        let log = std::fs::read_to_string(&record.log_path).unwrap();
        assert!(log.contains("lane_started"));

        backend.stop(&reg, &mut record, None).unwrap();
        assert_eq!(record.status, LaneStatus::Stopped);
        let reloaded = reg.load(&record.id).unwrap();
        assert_eq!(reloaded.status, LaneStatus::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_tmux_fails_lane_instead_of_faking_running() {
        use std::os::unix::fs::symlink;

        let _env_guard = tmux_env_lock();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        symlink("/usr/bin/false", bin_dir.join("tmux")).unwrap();
        let prior_path = std::env::var_os("PATH").unwrap_or_default();
        let combined_path = std::env::join_paths(
            std::iter::once(bin_dir).chain(std::env::split_paths(&prior_path)),
        )
        .unwrap();
        let _path = ScopedEnvVar::set("PATH", &combined_path);
        let _dry_run = ScopedEnvVar::remove("CODEWHALE_LANE_TMUX_DRY_RUN");

        let reg = LaneRegistry::open(dir.path().join("registry")).unwrap();
        let mut record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Tmux, None)
            .unwrap();
        let error = TmuxRuntime
            .start(
                &reg,
                &mut record,
                &LaneStartSpec {
                    command: vec!["/bin/true".to_string()],
                    cwd: None,
                    environment: Vec::new(),
                    log_proxy: Some(PathBuf::from("/bin/true")),
                    worktree: None,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("tmux runtime is unavailable"));
        assert_eq!(record.status, LaneStatus::Failed);
        assert_eq!(reg.load(&record.id).unwrap().status, LaneStatus::Failed);
        assert!(
            String::from_utf8_lossy(&fs::read(&record.log_path).unwrap()).contains("lane_failed")
        );
    }

    #[test]
    fn retired_remote_runtimes_are_rejected_before_a_lane_is_created() {
        for raw in ["vm", "ci", " VM "] {
            let error = RuntimeBackendKind::parse(raw).unwrap_err().to_string();
            assert!(error.contains("not available"), "{error}");
            assert!(error.contains("tmux or inline"), "{error}");
        }
        assert_eq!(
            RuntimeBackendKind::parse("inline").unwrap(),
            RuntimeBackendKind::Inline
        );
    }

    #[test]
    fn historical_retired_runtime_records_still_load_and_stop() {
        for kind in [RuntimeBackendKind::Vm, RuntimeBackendKind::Ci] {
            let dir = tempdir().unwrap();
            let reg = LaneRegistry::open(dir.path()).unwrap();
            let mut record = reg
                .create_pending(None, None, None, None, kind, None)
                .unwrap();
            assert_eq!(reg.list().unwrap().len(), 1, "record must not be skipped");
            assert_eq!(reg.load(&record.id).unwrap().runtime, kind);
            resolve_backend(kind)
                .stop(&reg, &mut record, None)
                .expect("a retired-runtime record can still be stopped");
            assert!(resolve_backend(kind).attach_command(&record).is_none());
        }
    }

    #[test]
    fn inline_runtime_writes_stream_json_log() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let mut record = reg
            .create_pending(
                Some("demo".into()),
                None,
                None,
                Some("echo".into()),
                RuntimeBackendKind::Inline,
                None,
            )
            .unwrap();
        InlineRuntime
            .start(
                &reg,
                &mut record,
                &LaneStartSpec {
                    command: vec!["echo".into(), "inline-ok".into()],
                    cwd: None,
                    environment: Vec::new(),
                    log_proxy: None,
                    worktree: None,
                },
            )
            .unwrap();
        assert_eq!(record.status, LaneStatus::Completed);
        let log = std::fs::read_to_string(&record.log_path).unwrap();
        assert!(log.contains("inline-ok"), "log={log}");
        assert!(log.contains("lane_completed"));
    }

    /// Audit R06-05: an inline command that cannot spawn marks the lane
    /// failed and cleans up the worktree it provisioned. The lane is terminal
    /// at that point, so no later stop would.
    #[test]
    fn inline_spawn_failure_cleans_up_its_worktree() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::worktree::tests::init_repo(&repo);
        let wt_path = dir.path().join("wt-inline");
        let reg = LaneRegistry::open(dir.path().join("lanes")).unwrap();
        let mut record = reg
            .create_pending(None, None, None, None, RuntimeBackendKind::Inline, Some(0))
            .unwrap();
        let result = InlineRuntime.start(
            &reg,
            &mut record,
            &LaneStartSpec {
                command: vec!["/nonexistent/codewhale-lane-test-binary".into()],
                cwd: None,
                environment: Vec::new(),
                log_proxy: None,
                worktree: Some(WorktreeProvision {
                    repo_root: repo,
                    branch: "codex/lane-spawn-fail".into(),
                    path: wt_path.clone(),
                    base_ref: Some("main".into()),
                }),
            },
        );
        assert!(result.is_err());
        assert_eq!(record.status, LaneStatus::Failed);
        assert!(
            !wt_path.exists(),
            "the failed lane's worktree was left behind"
        );
    }

    #[test]
    fn start_failure_after_provisioning_keeps_the_worktree_accounted_for() {
        for ttl in [Some(0), None] {
            let dir = tempdir().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            crate::worktree::tests::init_repo(&repo);
            let wt_path = dir.path().join("wt-unwritable-log");
            let reg = LaneRegistry::open(dir.path().join("lanes")).unwrap();
            let mut record = reg
                .create_pending(None, None, None, None, RuntimeBackendKind::Inline, ttl)
                .unwrap();
            let id = record.id.clone();
            record.log_path = dir.path().join("missing-dir").join("lane.ndjson");
            let result = InlineRuntime.start(
                &reg,
                &mut record,
                &LaneStartSpec {
                    command: vec!["echo".into(), "never-runs".into()],
                    cwd: None,
                    environment: Vec::new(),
                    log_proxy: None,
                    worktree: Some(WorktreeProvision {
                        repo_root: repo,
                        branch: "codex/lane-log-fail".into(),
                        path: wt_path.clone(),
                        base_ref: Some("main".into()),
                    }),
                },
            );
            let error = result.unwrap_err().to_string();
            assert!(error.contains("open lane log"), "ttl={ttl:?}: {error}");
            assert_eq!(record.status, LaneStatus::Failed, "ttl={ttl:?}");
            let stored = reg.load(&id).unwrap();
            assert_eq!(stored.status, LaneStatus::Failed, "ttl={ttl:?}");
            assert_eq!(stored.worktree_path.as_deref(), Some(wt_path.as_path()));
            assert_eq!(stored.branch.as_deref(), Some("codex/lane-log-fail"));
            assert_eq!(wt_path.exists(), ttl.is_none(), "ttl={ttl:?}");
        }
    }

    #[test]
    fn lane_start_spec_debug_redacts_environment_values() {
        let spec = LaneStartSpec {
            command: vec!["codewhale-tui".into()],
            cwd: None,
            environment: vec![("DEEPSEEK_API_KEY".into(), "secret-value".into())],
            log_proxy: None,
            worktree: None,
        };
        let rendered = format!("{spec:?}");
        assert!(rendered.contains("DEEPSEEK_API_KEY"));
        assert!(!rendered.contains("secret-value"));
    }

    #[cfg(unix)]
    #[test]
    fn inline_runtime_persists_typed_receipts_before_process_exit() {
        let dir = tempdir().unwrap();
        let reg = LaneRegistry::open(dir.path()).unwrap();
        let record = reg
            .create_pending(
                Some("demo".into()),
                None,
                None,
                None,
                RuntimeBackendKind::Inline,
                None,
            )
            .unwrap();
        let log_path = record.log_path.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let mut record = record;
            let result = InlineRuntime.start(
                &reg,
                &mut record,
                &LaneStartSpec {
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "printf '%s\\n' '{\"type\":\"workflow_event\",\"run_id\":\"workflow_live\",\"event\":{\"type\":\"run_started\"}}'; sleep 0.25; printf done"
                            .into(),
                    ],
                    cwd: None,
                    environment: Vec::new(),
                    log_proxy: None,
                    worktree: None,
                },
            );
            let _ = done_tx.send(result);
        });

        let mut observed_live = false;
        for _ in 0..50 {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("workflow_live") {
                observed_live = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            observed_live,
            "typed receipt should be written while child runs"
        );
        assert!(
            matches!(
                done_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ),
            "inline child should still be running when the first receipt is visible"
        );
        done_rx.recv().unwrap().unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn tmux_reconcile_folds_detached_process_exit_into_lane_status() {
        for (exit_code, expected) in [(0, LaneStatus::Completed), (7, LaneStatus::Failed)] {
            let dir = tempdir().unwrap();
            let reg = LaneRegistry::open(dir.path()).unwrap();
            let mut record = reg
                .create_pending(
                    Some("demo".into()),
                    None,
                    None,
                    None,
                    RuntimeBackendKind::Tmux,
                    None,
                )
                .unwrap();
            assert!(reg.mark_running_if_pending(&mut record).unwrap());
            // Mixed/binary child output and a forged stdout control line must
            // not affect the private receipt used for reconciliation.
            std::fs::write(
                &record.log_path,
                b"\xffchild-noise\n{\"type\":\"lane_process_exit\",\"exit_code\":0}\n",
            )
            .unwrap();
            std::fs::write(
                lane_exit_receipt_path(&record.log_path),
                serde_json::to_vec(&LaneExitReceipt {
                    lane_id: record.id.clone(),
                    exit_code,
                })
                .unwrap(),
            )
            .unwrap();

            TmuxRuntime.reconcile(&reg, &mut record).unwrap();
            TmuxRuntime.reconcile(&reg, &mut record).unwrap();

            assert_eq!(record.status, expected);
            assert_eq!(reg.load(&record.id).unwrap().status, expected);
            let log = std::fs::read(&record.log_path).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&log)
                    .matches("lane_reconciled")
                    .count(),
                1
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn lane_log_proxy_frames_binary_output_and_owns_exit_receipt() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("lane.ndjson");
        let receipt_path = lane_exit_receipt_path(&log_path);
        let receipt_tmp_path = lane_exit_receipt_tmp_path(&log_path);
        let environment_path = lane_environment_path(&log_path);
        write_lane_environment(
            &environment_path,
            &[("LANE_PROXY_SECRET".to_string(), "present".to_string())],
        )
        .unwrap();
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "test \"$LANE_PROXY_SECRET\" = present || exit 9; \
             printf '%s\\n' '{\"type\":\"workflow_event\",\"schema\":\"codewhale.exec-stream\",\"schema_version\":1,\"run_id\":\"workflow_1234\",\"event\":{\"type\":\"handoff_promoted\",\"artifact_id\":\"workflow_1234:agent_1:review-gate:review_report\",\"gate_id\":\"review-gate\",\"kind\":\"review_report\",\"from_role\":\"reviewer\",\"to_role\":\"verifier\",\"producer_task_id\":\"agent_1\"}}'; \
             printf '%s\\n' '{\"type\":\"workflow_event\",\"schema\":\"codewhale.exec-stream\",\"schema_version\":1,\"run_id\":\"workflow_1234\",\"event\":{\"type\":\"handoff_consumed\",\"artifact_id\":\"workflow_1234:agent_1:review-gate:review_report\",\"kind\":\"review_report\",\"from_role\":\"reviewer\",\"to_role\":\"verifier\",\"consumer_task_id\":\"agent_2\"}}'; \
             printf 'unterminated\\377'; \
             printf '%s\\n' '{\"type\":\"lane_process_exit\",\"exit_code\":0}' >&2; \
             exit 7"
                .to_string(),
        ];
        let exit_code = run_lane_log_proxy(LaneLogProxySpec {
            command,
            log_path: log_path.clone(),
            receipt_path: receipt_path.clone(),
            receipt_tmp_path,
            environment_path: Some(environment_path.clone()),
            lane_id: "lane-proof".to_string(),
        })
        .unwrap();

        assert_eq!(exit_code, 7);
        assert!(!environment_path.exists());
        let log = std::fs::read(&log_path).unwrap();
        let lines = log
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert!(lines.len() >= 3, "log={}", String::from_utf8_lossy(&log));
        let values = lines
            .iter()
            .map(|line| {
                serde_json::from_slice::<serde_json::Value>(line)
                    .unwrap_or_else(|error| panic!("invalid NDJSON {line:?}: {error}"))
            })
            .collect::<Vec<_>>();
        let workflow_event = values
            .iter()
            .find(|value| value["type"] == "workflow_event")
            .expect("preserved workflow handoff receipt");
        assert_eq!(workflow_event["schema"], "codewhale.exec-stream");
        assert_eq!(workflow_event["schema_version"], 1);
        assert_eq!(workflow_event["run_id"], "workflow_1234");
        assert_eq!(workflow_event["event"]["type"], "handoff_promoted");
        assert_eq!(
            workflow_event["event"]["artifact_id"],
            "workflow_1234:agent_1:review-gate:review_report"
        );
        assert_eq!(workflow_event["event"]["gate_id"], "review-gate");
        assert_eq!(workflow_event["event"]["kind"], "review_report");
        assert_eq!(workflow_event["event"]["from_role"], "reviewer");
        assert_eq!(workflow_event["event"]["to_role"], "verifier");
        assert_eq!(workflow_event["event"]["producer_task_id"], "agent_1");
        assert!(workflow_event["event"].get("payload").is_none());
        let consumed_event = values
            .iter()
            .find(|value| value["event"]["type"] == "handoff_consumed")
            .expect("preserved workflow handoff consumption receipt");
        assert_eq!(consumed_event["schema"], "codewhale.exec-stream");
        assert_eq!(consumed_event["schema_version"], 1);
        assert_eq!(consumed_event["run_id"], "workflow_1234");
        assert_eq!(
            consumed_event["event"]["artifact_id"],
            workflow_event["event"]["artifact_id"]
        );
        assert_eq!(consumed_event["event"]["kind"], "review_report");
        assert_eq!(consumed_event["event"]["from_role"], "reviewer");
        assert_eq!(consumed_event["event"]["to_role"], "verifier");
        assert_eq!(consumed_event["event"]["consumer_task_id"], "agent_2");
        assert!(consumed_event["event"].get("payload").is_none());
        let rendered = String::from_utf8_lossy(&log);
        assert!(rendered.contains("workflow_event"));
        assert!(rendered.contains("lane_process_exit"));
        assert!(rendered.contains("lane_log"));
        let receipt = read_lane_exit_receipt(&log_path, "lane-proof")
            .unwrap()
            .expect("private receipt");
        assert_eq!(receipt.exit_code, 7);
    }

    #[test]
    fn invalid_environment_key_never_leaves_a_partial_secret_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lane.env.json");
        let result = write_lane_environment(
            &path,
            &[
                ("VALID_KEY".to_string(), "first-secret".to_string()),
                ("INVALID-KEY".to_string(), "second-secret".to_string()),
            ],
        );
        assert!(result.is_err());
        assert!(!path.exists());
        assert!(!lane_environment_tmp_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_environment_is_mode_0600_and_malformed_input_is_removed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("lane.env.json");
        write_lane_environment(&path, &[("SECRET".to_string(), "sensitive".to_string())]).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::write(&path, b"{malformed").unwrap();
        let log_path = dir.path().join("lane.ndjson");
        let exit_code = run_lane_log_proxy(LaneLogProxySpec {
            command: vec!["/bin/true".to_string()],
            receipt_path: lane_exit_receipt_path(&log_path),
            receipt_tmp_path: lane_exit_receipt_tmp_path(&log_path),
            environment_path: Some(path.clone()),
            lane_id: "lane-malformed-env".to_string(),
            log_path: log_path.clone(),
        })
        .unwrap();
        assert_eq!(exit_code, LANE_PROXY_FAILURE_EXIT_CODE);
        assert!(!path.exists());
        assert!(String::from_utf8_lossy(&fs::read(log_path).unwrap()).contains("lane_proxy_error"));
    }

    #[cfg(unix)]
    #[test]
    fn tmux_stop_failure_keeps_lane_running_and_preserves_cleanup_targets() {
        use std::os::unix::fs::symlink;

        let _env_guard = tmux_env_lock();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        symlink("/usr/bin/false", bin_dir.join("tmux")).unwrap();
        let prior_path = std::env::var_os("PATH").unwrap_or_default();
        let combined_path = std::env::join_paths(
            std::iter::once(bin_dir.clone()).chain(std::env::split_paths(&prior_path)),
        )
        .unwrap();
        let _path = ScopedEnvVar::set("PATH", &combined_path);
        let _dry_run = ScopedEnvVar::remove("CODEWHALE_LANE_TMUX_DRY_RUN");

        let reg = LaneRegistry::open(dir.path().join("registry")).unwrap();
        let mut record = reg
            .create_pending(
                Some("demo".into()),
                None,
                None,
                None,
                RuntimeBackendKind::Tmux,
                Some(0),
            )
            .unwrap();
        record.tmux_session = Some(format!("cw-{}", record.id));
        record.tmux_socket = Some(reg.root().join("tmux.sock"));
        let worktree = dir.path().join("worktree");
        fs::create_dir(&worktree).unwrap();
        record.worktree_path = Some(worktree.clone());
        let environment_path = lane_environment_path(&record.log_path);
        write_lane_environment(
            &environment_path,
            &[("SECRET".to_string(), "still-private".to_string())],
        )
        .unwrap();
        assert!(reg.mark_running_if_pending(&mut record).unwrap());

        let error = TmuxRuntime.stop(&reg, &mut record, None).unwrap_err();
        assert!(format!("{error:#}").contains("tmux has-session"));
        assert_eq!(record.status, LaneStatus::Running);
        assert_eq!(reg.load(&record.id).unwrap().status, LaneStatus::Running);
        assert!(environment_path.exists());
        assert!(worktree.exists());
    }

    #[cfg(unix)]
    #[test]
    fn tmux_reconcile_marks_vanished_session_failed_without_receipt() {
        use std::os::unix::fs::PermissionsExt;

        let _env_guard = tmux_env_lock();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let tmux = bin_dir.join("tmux");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf '%s\\n' 'no server running on /tmp/codewhale-test.sock' >&2\nexit 1\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&tmux).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tmux, permissions).unwrap();
        let prior_path = std::env::var_os("PATH").unwrap_or_default();
        let combined_path = std::env::join_paths(
            std::iter::once(bin_dir).chain(std::env::split_paths(&prior_path)),
        )
        .unwrap();
        let _path = ScopedEnvVar::set("PATH", &combined_path);
        let _dry_run = ScopedEnvVar::remove("CODEWHALE_LANE_TMUX_DRY_RUN");

        let reg = LaneRegistry::open(dir.path()).unwrap();
        let mut record = reg
            .create_pending(
                Some("demo".into()),
                None,
                None,
                None,
                RuntimeBackendKind::Tmux,
                None,
            )
            .unwrap();
        record.tmux_session = Some(format!("missing-{}", record.id));
        record.tmux_socket = Some(reg.root().join("tmux.sock"));
        assert!(reg.mark_running_if_pending(&mut record).unwrap());

        TmuxRuntime.reconcile(&reg, &mut record).unwrap();

        assert_eq!(record.status, LaneStatus::Failed);
        let log = std::fs::read_to_string(&record.log_path).unwrap();
        assert!(log.contains("tmux_session_missing_without_exit_receipt"));
    }
}

//! Stateful, PTY-backed terminal sessions.
//!
//! Live PTY processes remain deliberately process-local, while a non-secret
//! durable summary records identity, last-known cwd, lifecycle state, and
//! replacement history. A later process reports the shell as stale/lost and
//! starts a new identity; it never claims to reattach from a reused PID.

#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::io::{self, Write};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(unix)]
use std::time::{Duration, Instant};

use async_trait::async_trait;
#[cfg(unix)]
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use uuid::Uuid;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
#[cfg(unix)]
use super::spec::{optional_u64, required_str};

#[cfg(unix)]
const BUFFER_LIMIT: usize = 512 * 1024;
#[cfg(unix)]
const OUTPUT_LIMIT: usize = 12 * 1024;
/// Ceiling for one [`OutputBuffer::read_since`] response. The ring is already
/// bounded at [`BUFFER_LIMIT`]; this bounds a single frame so a client cannot
/// ask for the whole window at once.
#[cfg(unix)]
pub(crate) const READ_LIMIT: usize = 64 * 1024;
#[cfg(unix)]
const DEFAULT_TIMEOUT_SECS: u64 = 120;
#[cfg(unix)]
const MAX_TIMEOUT_SECS: u64 = 600;
#[cfg(unix)]
const CANCEL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const CANCEL_SENTINEL_RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[cfg(unix)]
pub(crate) struct TerminalSession {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// The pty master is retained after the reader and writer clones are taken
    /// so [`TerminalSession::resize`] can reach the kernel's window size. The
    /// clones keep the pty alive; nothing else clones the master.
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    output: Arc<Mutex<OutputBuffer>>,
    read_cursor: u64,
    command: Option<CommandState>,
    durable: DurableTerminalRecord,
    durable_path: PathBuf,
    /// True when the live PTY was started through a real sandbox backend.
    /// A later narrowed posture must not reuse an unsandboxed shell.
    sandbox_confined: bool,
    /// A running process cannot adopt changed roots or network restrictions.
    sandbox_policy: codewhale_sandbox::SandboxPolicy,
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableTerminalRecord {
    schema_version: u32,
    session_id: String,
    runtime_nonce: String,
    process_id: u32,
    workspace: PathBuf,
    name: String,
    shell: String,
    last_known_cwd: PathBuf,
    environment_summary: EnvironmentSummary,
    state: DurableTerminalState,
    updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<Box<DurableTerminalRecord>>,
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EnvironmentSummary {
    term: Option<String>,
    virtual_env_active: bool,
    conda_env_active: bool,
    nix_shell_active: bool,
    note: String,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DurableTerminalState {
    Running,
    Idle,
    Canceled,
    Failed,
    StaleLost,
    Reset,
}

#[cfg(unix)]
struct CommandState {
    marker: String,
}

#[cfg(unix)]
#[derive(Default)]
struct OutputBuffer {
    bytes: VecDeque<u8>,
    total: u64,
    last_activity: Option<Instant>,
}

/// One absolute-offset slice of a session's output.
///
/// Offsets are absolute for the life of the live PTY: they count every byte
/// the reader thread ever appended, not the bytes still retained. That is what
/// lets a client resume from a cursor it stored earlier, and what lets this
/// type tell it the truth when the retained window has moved past it.
///
/// Field names match the `/v1/terminal/{name}/output` wire so the payload is a
/// projection, not a translation (the same vocabulary the jobs byte stream
/// uses).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputChunk {
    pub(crate) bytes: Vec<u8>,
    /// Absolute offset of `bytes[0]` in the session's lifetime output.
    pub(crate) offset: u64,
    /// Offset to pass as the next `cursor`.
    pub(crate) next_cursor: u64,
    /// Every byte the session has produced, retained or not.
    pub(crate) total: u64,
    /// Leading bytes the ring has permanently discarded.
    pub(crate) dropped: u64,
    /// True when the requested cursor predates the retained window. The bytes
    /// in between cannot be recovered from this process — report, not repair.
    pub(crate) gap: bool,
}

#[cfg(unix)]
impl OutputBuffer {
    fn append(&mut self, data: &[u8]) {
        self.last_activity = Some(Instant::now());
        self.total = self.total.saturating_add(data.len() as u64);
        self.bytes.extend(data);
        while self.bytes.len() > BUFFER_LIMIT {
            let _ = self.bytes.pop_front();
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.iter().copied().collect::<Vec<_>>()).into_owned()
    }

    /// Absolute offset of the oldest retained byte.
    fn oldest(&self) -> u64 {
        self.total.saturating_sub(self.bytes.len() as u64)
    }

    /// Read from an absolute cursor without consuming: the caller owns its own
    /// position, so two readers can replay the same bytes and a repeated read
    /// is idempotent. Does not advance `read_cursor` — the consuming
    /// tool-result path is untouched. This is the cursor arithmetic only;
    /// response-size policy belongs to the caller.
    ///
    /// A cursor past the head clamps to `total`: nothing is there yet, and
    /// echoing the future cursor back as `next_cursor` would make every byte
    /// produced before the stream reached it invisible to that client.
    fn read_since(&self, cursor: u64, max_bytes: usize) -> OutputChunk {
        let dropped = self.oldest();
        let gap = cursor < dropped;
        let start = cursor.max(dropped).min(self.total);
        let skip = usize::try_from(start - dropped).unwrap_or(usize::MAX);
        let take = max_bytes.min(self.bytes.len().saturating_sub(skip));
        let bytes = self.bytes.iter().skip(skip).take(take).copied().collect();
        OutputChunk {
            bytes,
            offset: start,
            next_cursor: start + take as u64,
            total: self.total,
            dropped,
            gap,
        }
    }
}

#[cfg(unix)]
pub(crate) type SharedSession = Arc<Mutex<TerminalSession>>;

#[cfg(unix)]
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SessionKey {
    workspace: PathBuf,
    name: String,
}

#[cfg(unix)]
static SESSIONS: OnceLock<Mutex<HashMap<SessionKey, SharedSession>>> = OnceLock::new();

#[cfg(unix)]
static RUNTIME_NONCE: OnceLock<String> = OnceLock::new();

#[cfg(unix)]
fn runtime_nonce() -> &'static str {
    RUNTIME_NONCE.get_or_init(|| Uuid::new_v4().to_string())
}

#[cfg(unix)]
fn sessions() -> &'static Mutex<HashMap<SessionKey, SharedSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(unix)]
fn session_key(name: &str, workspace: &Path) -> SessionKey {
    SessionKey {
        workspace: workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf()),
        name: name.to_string(),
    }
}

#[cfg(unix)]
fn durable_path(name: &str, workspace: &Path) -> Result<PathBuf, String> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(workspace.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(name.as_bytes());
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    #[cfg(test)]
    let state_dir = workspace.join(".codewhale-test-terminal-sessions");
    #[cfg(not(test))]
    let state_dir = codewhale_config::ensure_state_dir("terminal-sessions")
        .map_err(|error| format!("failed to resolve terminal session state directory: {error}"))?;
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("failed to create terminal session state directory: {error}"))?;
    Ok(state_dir.join(format!("{}.json", &digest[..32])))
}

#[cfg(unix)]
fn load_durable(path: &Path) -> Option<DurableTerminalRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(unix)]
fn persist_durable(path: &Path, record: &DurableTerminalRecord) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(record)
        .map_err(|error| format!("failed to encode terminal session state: {error}"))?;
    codewhale_util::utils::write_atomic(path, &payload)
        .map_err(|error| format!("failed to persist terminal session state: {error}"))
}

#[cfg(unix)]
fn pty_lacks_required_sandbox(
    policy: &codewhale_sandbox::SandboxPolicy,
    applied: codewhale_sandbox::SandboxType,
) -> bool {
    policy.should_sandbox() && matches!(applied, codewhale_sandbox::SandboxType::None)
}

#[cfg(unix)]
fn pty_sandbox_policy(
    context: &ToolContext,
) -> Result<codewhale_sandbox::SandboxPolicy, ToolError> {
    if context.sandbox_backend.is_some() {
        return Err(ToolError::not_available(
            "terminal PTY tools start a local shell, and this session runs commands in an external sandbox; run the command through the shell tool instead",
        ));
    }
    Ok(context.elevated_sandbox_policy.clone().unwrap_or_default())
}

#[cfg(unix)]
fn prepare_pty_shell(
    shell: &str,
    workspace: &std::path::Path,
    policy: codewhale_sandbox::SandboxPolicy,
    shell_manager: &crate::tools::shell::SharedShellManager,
) -> Result<(codewhale_sandbox::ExecEnv, bool), String> {
    let prepared = shell_manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .prepare_runner(
            shell,
            vec!["-i".to_string()],
            workspace,
            Duration::from_secs(24 * 60 * 60),
            Some(policy.clone()),
        );
    if pty_lacks_required_sandbox(&policy, prepared.sandbox_type) {
        return Err(
            "terminal PTY tools cannot run unsandboxed under a narrowed filesystem posture; use Full Access / ExternalSandbox or enable seatbelt/bwrap"
                .to_string(),
        );
    }
    if prepared.command.is_empty() {
        return Err("sandbox prepare returned an empty PTY command".to_string());
    }
    let confined = !matches!(prepared.sandbox_type, codewhale_sandbox::SandboxType::None);
    Ok((prepared, confined))
}

#[cfg(unix)]
fn create_session(
    name: &str,
    workspace: &std::path::Path,
    policy: codewhale_sandbox::SandboxPolicy,
    shell_manager: &crate::tools::shell::SharedShellManager,
) -> Result<SharedSession, String> {
    // Unit tests exercise the persistent-PTY contract, not a developer's
    // interactive shell startup files. Keep their deadlines deterministic and
    // avoid racing process-global HOME/SHELL overrides from parallel tests.
    #[cfg(test)]
    let shell = "/bin/sh".to_string();
    #[cfg(not(test))]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let durable_path = durable_path(name, &workspace)?;
    let previous = load_durable(&durable_path).map(|mut record| {
        // A persisted shell is historical evidence only. A random per-process
        // nonce makes PID reuse irrelevant and deliberately forbids reattach.
        record.state = DurableTerminalState::StaleLost;
        record.updated_at = chrono::Utc::now().to_rfc3339();
        Box::new(record)
    });
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {e}"))?;

    let (prepared, sandbox_confined) =
        prepare_pty_shell(&shell, &workspace, policy.clone(), shell_manager)?;
    let child = prepared
        .spawn_pty(pair.slave, &prepared.cwd)
        .map_err(|e| format!("failed to start shell {shell}: {e}"))?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to read PTY: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to write PTY: {e}"))?;
    let output = Arc::new(Mutex::new(OutputBuffer::default()));
    let reader_output = Arc::clone(&output);
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut output) = reader_output.lock() {
                        output.append(&buf[..n]);
                    }
                }
            }
        }
    });

    let durable = DurableTerminalRecord {
        schema_version: 1,
        session_id: Uuid::new_v4().to_string(),
        runtime_nonce: runtime_nonce().to_string(),
        process_id: std::process::id(),
        workspace: workspace.clone(),
        name: name.to_string(),
        shell,
        last_known_cwd: workspace,
        environment_summary: EnvironmentSummary {
            term: std::env::var("TERM").ok().filter(|value| !value.trim().is_empty()),
            virtual_env_active: std::env::var_os("VIRTUAL_ENV").is_some(),
            conda_env_active: std::env::var_os("CONDA_PREFIX").is_some(),
            nix_shell_active: std::env::var_os("IN_NIX_SHELL").is_some(),
            note: "Values and secrets are not persisted; in-shell environment changes are process-local."
                .to_string(),
        },
        state: DurableTerminalState::Idle,
        updated_at: chrono::Utc::now().to_rfc3339(),
        previous,
    };
    persist_durable(&durable_path, &durable)?;

    Ok(Arc::new(Mutex::new(TerminalSession {
        writer: Arc::new(Mutex::new(writer)),
        master: pair.master,
        child,
        output,
        read_cursor: 0,
        command: None,
        durable,
        durable_path,
        sandbox_confined,
        sandbox_policy: policy,
    })))
}

#[cfg(unix)]
pub(crate) fn get_or_create(
    name: &str,
    workspace: &std::path::Path,
    policy: codewhale_sandbox::SandboxPolicy,
    shell_manager: &crate::tools::shell::SharedShellManager,
) -> Result<SharedSession, String> {
    let key = session_key(name, workspace);
    let mut registry = sessions()
        .lock()
        .map_err(|_| "terminal session registry lock poisoned".to_string())?;
    if let Some(session) = registry.get(&key) {
        let session_guard = session
            .lock()
            .map_err(|_| "terminal session lock poisoned".to_string())?;
        if !can_reuse_under_policy(&session_guard, &policy) {
            return Err(
                "existing PTY session does not enforce the current sandbox policy; start a new session name under the current posture or use Full Access"
                    .to_string(),
            );
        }
        return Ok(Arc::clone(session));
    }
    let session = create_session(name, workspace, policy, shell_manager)?;
    registry.insert(key, Arc::clone(&session));
    Ok(session)
}

/// Restricted reuse requires the policy that actually created the process.
/// Exact comparison is deliberately conservative: widening a restricted policy
/// may require a new name too, but never weakens a running process's boundary.
#[cfg(unix)]
fn can_reuse_under_policy(
    session: &TerminalSession,
    policy: &codewhale_sandbox::SandboxPolicy,
) -> bool {
    !policy.should_sandbox() || (session.sandbox_confined && session.sandbox_policy == *policy)
}

#[cfg(unix)]
fn find(name: &str, workspace: &Path) -> Result<SharedSession, String> {
    let live = sessions()
        .lock()
        .map_err(|_| "terminal session registry lock poisoned".to_string())?
        .get(&session_key(name, workspace))
        .cloned();
    if let Some(live) = live {
        return Ok(live);
    }
    if let Ok(path) = durable_path(name, workspace)
        && let Some(mut record) = load_durable(&path)
    {
        record.state = DurableTerminalState::StaleLost;
        record.updated_at = chrono::Utc::now().to_rfc3339();
        let _ = persist_durable(&path, &record);
        return Err(format!(
            "terminal session '{name}' is stale/lost after restart (last cwd: {}); run terminal/run with this name to start a replacement while preserving the historical summary",
            record.last_known_cwd.display()
        ));
    }
    Err(format!(
        "terminal session '{name}' does not exist in workspace {}",
        workspace.display()
    ))
}

#[cfg(unix)]
fn write_bytes(session: &TerminalSession, bytes: &[u8]) -> io::Result<()> {
    let mut writer = session
        .writer
        .lock()
        .map_err(|_| io::Error::other("terminal PTY writer lock poisoned"))?;
    writer.write_all(bytes)?;
    writer.flush()
}

/// Raw input must not silently overflow the canonical line buffer. This
/// refuses the demonstrated oversized batch, not cumulative line editing
/// or a concurrent terminal-mode change. Internal shell source and completion
/// sentinels use the private writer instead.
#[cfg(unix)]
pub(crate) fn write_raw_input(session: &TerminalSession, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() >= 1024
        && session
            .master
            .get_termios()
            .is_none_or(|mode| mode.local_flags.bits() & libc::ICANON != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal input batch is too large for canonical or unknown input mode; no input was sent. Send fewer than 1024 bytes, or use an application in raw mode.",
        ));
    }
    write_bytes(session, bytes)
}

#[cfg(unix)]
fn output_snapshot(session: &TerminalSession) -> String {
    session
        .output
        .lock()
        .map(|output| output.text())
        .unwrap_or_default()
}

#[cfg(unix)]
fn take_output(session: &mut TerminalSession) -> String {
    let Ok(output) = session.output.lock() else {
        return String::new();
    };
    let chunk = output.read_since(session.read_cursor, usize::MAX);
    session.read_cursor = output.total;
    String::from_utf8_lossy(&chunk.bytes).into_owned()
}

/// Resize the live pty's window. The kernel updates its `winsize` and signals
/// the child, which is what makes an interactive app redraw at the new size.
#[cfg(unix)]
pub(crate) fn resize_session(
    session: &TerminalSession,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    session
        .master
        .resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("PTY resize failed: {e}"))
}

/// Absolute-offset, non-consuming read. `max_bytes` is clamped to
/// [`READ_LIMIT`] so one caller cannot ask for the whole retained window.
///
/// Known limitation: this reports a [`OutputChunk::gap`]; it does not repair
/// one. Bytes dropped by the ring are gone with the process, and nothing here
/// re-reads them from disk — the durable record is identity and lifecycle,
/// never output.
#[cfg(unix)]
pub(crate) fn read_session_since(
    session: &TerminalSession,
    cursor: u64,
    max_bytes: usize,
) -> Result<OutputChunk, String> {
    let output = session
        .output
        .lock()
        .map_err(|_| "terminal output lock poisoned".to_string())?;
    Ok(output.read_since(cursor, max_bytes.min(READ_LIMIT)))
}

/// Poll the shell without blocking; `None` means it is still running.
#[cfg(unix)]
pub(crate) fn session_exit_status(
    session: &mut TerminalSession,
) -> Result<Option<portable_pty::ExitStatus>, String> {
    session
        .child
        .try_wait()
        .map_err(|e| format!("PTY wait failed: {e}"))
}

/// Terminate the shell. The caller still observes the exit through
/// [`session_exit_status`].
#[cfg(unix)]
pub(crate) fn kill_session(session: &mut TerminalSession) -> Result<(), String> {
    session
        .child
        .kill()
        .map_err(|e| format!("PTY kill failed: {e}"))
}

/// Resolve a live session without creating one.
///
/// `/v1/terminal` attaches to shells the Engine already owns (the agent's
/// terminal tools create them). A request for a name that has no live session
/// is a 404, never a new process: an HTTP client must not be able to conjure a
/// shell the Engine does not know about.
#[cfg(unix)]
pub(crate) fn lookup(name: &str, workspace: &Path) -> Option<SharedSession> {
    let key = session_key(name, workspace);
    sessions()
        .lock()
        .ok()
        .and_then(|registry| registry.get(&key).map(Arc::clone))
}

/// A dock projection of the existing registry, never another session owner.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalSessionInfo {
    pub id: String,
    pub name: String,
    pub state: DurableTerminalState,
    pub exit_code: Option<u32>,
    pub updated_at: String,
    pub last_activity: Option<Instant>,
}

/// Called on the dock's blocking worker. Reads never consume tool output.
/// A selected identity that disappeared is not replaced by a reused name.
#[cfg(unix)]
pub(crate) fn inspect_workspace(
    workspace: &Path,
    selected: Option<&str>,
) -> Result<(Vec<TerminalSessionInfo>, Option<OutputChunk>), String> {
    let workspace = session_key("", workspace).workspace;
    let live = sessions()
        .lock()
        .map_err(|_| "terminal session registry lock poisoned".to_string())?
        .iter()
        .filter(|(key, _)| key.workspace == workspace)
        .map(|(_, session)| Arc::clone(session))
        .collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(live.len());
    for handle in live {
        let mut session = handle
            .lock()
            .map_err(|_| "terminal session lock poisoned".to_string())?;
        let exit_code = session_exit_status(&mut session)?.map(|status| status.exit_code());
        let state = match completion(&session) {
            Some((0, _)) => DurableTerminalState::Idle,
            Some((130, _)) => DurableTerminalState::Canceled,
            Some(_) => DurableTerminalState::Failed,
            None => session.durable.state,
        };
        entries.push((
            TerminalSessionInfo {
                id: session.durable.session_id.clone(),
                name: session.durable.name.clone(),
                state,
                exit_code,
                updated_at: session.durable.updated_at.clone(),
                last_activity: session
                    .output
                    .lock()
                    .map_err(|_| "terminal output lock poisoned".to_string())?
                    .last_activity,
            },
            handle.clone(),
        ));
    }
    entries.sort_by(|(a, _), (b, _)| {
        b.last_activity
            .cmp(&a.last_activity)
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.name.cmp(&b.name))
    });
    let selected = match selected {
        Some(id) => entries.iter().find(|(info, _)| info.id == id),
        None => entries.first(),
    };
    let output = selected
        .map(|(_, handle)| -> Result<OutputChunk, String> {
            let session = handle
                .lock()
                .map_err(|_| "terminal session lock poisoned".to_string())?;
            let output = session
                .output
                .lock()
                .map_err(|_| "terminal output lock poisoned".to_string())?;
            Ok(output.read_since(output.total.saturating_sub(READ_LIMIT as u64), READ_LIMIT))
        })
        .transpose()?;
    Ok((entries.into_iter().map(|(info, _)| info).collect(), output))
}

/// Human input is bound to the displayed incarnation, including across reset.
/// It follows the same sandbox-reuse guard as the model's terminal/run path.
#[cfg(unix)]
pub(crate) fn write_dock_input(
    workspace: &Path,
    name: &str,
    id: &str,
    bytes: &[u8],
    policy: &codewhale_sandbox::SandboxPolicy,
) -> Result<(), String> {
    let handle = lookup(name, workspace).ok_or("selected terminal is gone or was reset")?;
    let mut session = handle
        .lock()
        .map_err(|_| "terminal session lock poisoned".to_string())?;
    if session.durable.session_id != id {
        return Err(
            "selected terminal is gone or was reset; select its replacement explicitly".into(),
        );
    }
    if !can_reuse_under_policy(&session, policy) {
        return Err(
            "selected terminal does not enforce the current sandbox policy; start a fresh terminal"
                .into(),
        );
    }
    if session_exit_status(&mut session)?.is_some() {
        return Err("selected terminal has exited; start a fresh terminal".into());
    }
    if bytes == [3] && session.command.is_some() && completion(&session).is_none() {
        // SIGINT can discard the grouped command's completion suffix. Reuse
        // the confirmed interrupt path so model waiters and later runs see
        // the persistent shell become ready again.
        drop(session);
        return cancel_shared_session(&handle)
            .map(|_| ())
            .map_err(|error| error.to_string());
    }
    write_raw_input(&session, bytes).map_err(|error| error.to_string())
}

#[cfg(unix)]
pub(crate) fn resize_dock_session(
    session: &TerminalSession,
    id: &str,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    if session.durable.session_id != id {
        return Err("selected terminal is gone or was reset".into());
    }
    resize_session(session, rows, cols)
}

#[cfg(unix)]
fn prune_output(input: &str) -> String {
    if input.len() <= OUTPUT_LIMIT {
        return input.to_string();
    }
    let head = OUTPUT_LIMIT / 3;
    let tail = OUTPUT_LIMIT - head;
    let head_end = input
        .char_indices()
        .find(|(index, _)| *index >= head)
        .map_or(input.len(), |(index, _)| index);
    // The earliest boundary that still fits: the tail is where a build's
    // error and the command's completion marker are.
    let tail_start = input
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= head_end && input.len() - *index <= tail)
        .unwrap_or(input.len());
    format!(
        "{}\n… [output truncated: {} bytes omitted] …\n{}",
        &input[..head_end],
        tail_start - head_end,
        &input[tail_start..]
    )
}

#[cfg(unix)]
fn completion(session: &TerminalSession) -> Option<(i32, String)> {
    let state = session.command.as_ref()?;
    let output = output_snapshot(session);
    let marker = format!("\n{}:", state.marker);
    let line = output
        .rsplit(&marker)
        .next()
        .and_then(|tail| tail.lines().next())?;
    let (status, cwd) = line.split_once(':')?;
    Some((status.parse().ok()?, cwd.to_string()))
}

#[cfg(unix)]
fn start_command(session: &mut TerminalSession, command: &str) -> Result<(), String> {
    if session.command.is_some() && completion(session).is_none() {
        return Err("terminal session already has a running foreground command".to_string());
    }
    let marker = format!("__CODEWHALE_TERM_{}__", Uuid::new_v4().simple());
    // The command must run in the CURRENT shell — a subshell would discard
    // exactly the state (cd, exports, functions, activated envs) this tool
    // exists to preserve (EXEC-001). Parse the source and sentinel together,
    // but read source lines from a separate fd: interactive commands keep the
    // PTY as stdin, and aliases defined on earlier lines expand in every shell.
    // The random quoted delimiter prevents source expansion by the outer shell.
    let delimiter = format!("{marker}_SOURCE");
    let wrapped = format!(
        ". /dev/fd/9 9<<'{delimiter}'; __cw_status=$?; printf '\\n{marker}:%s:%s\\n' \"$__cw_status\" \"$PWD\"\n{command}\n{delimiter}\n"
    );
    write_bytes(session, wrapped.as_bytes())
        .map_err(|error| format!("PTY write failed: {error}"))?;
    session.command = Some(CommandState { marker });
    session.durable.state = DurableTerminalState::Running;
    session.durable.updated_at = chrono::Utc::now().to_rfc3339();
    persist_durable(&session.durable_path, &session.durable)?;
    Ok(())
}

#[cfg(unix)]
fn write_completion_sentinel(
    session: &TerminalSession,
    marker: &str,
    status: i32,
) -> Result<(), String> {
    let sentinel =
        format!("__cw_status={status}; printf '\\n{marker}:%s:%s\\n' \"$__cw_status\" \"$PWD\"\n");
    write_bytes(session, sentinel.as_bytes()).map_err(|error| format!("PTY write failed: {error}"))
}

#[cfg(unix)]
#[cfg(test)]
fn wait_session(session: &mut TerminalSession, timeout: Duration) -> (Option<(i32, String)>, bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(done) = completion(session) {
            return (Some(done), false);
        }
        if Instant::now() >= deadline {
            return (None, true);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait without monopolizing the per-session lock. `terminal/send` and
/// `terminal/cancel` must be able to acquire the lock while a foreground
/// command is active; otherwise interactive input and cancellation deadlock
/// behind the waiter.
#[cfg(unix)]
fn wait_shared_session(
    session: &SharedSession,
    timeout: Duration,
) -> Result<(Option<(i32, String)>, bool), ToolError> {
    let deadline = Instant::now() + timeout;
    loop {
        let done = {
            let session = session
                .lock()
                .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
            completion(&session)
        };
        if let Some(done) = done {
            return Ok((Some(done), false));
        }
        if Instant::now() >= deadline {
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Interrupt a foreground command and wait until the persistent shell confirms
/// that it is ready again.
///
/// Canonical PTYs may flush queued input when they process ETX. Resubmit the
/// completion sentinel until the shell acknowledges it so a busy host cannot
/// strand the session merely because the one post-interrupt write raced that
/// flush.
#[cfg(unix)]
fn cancel_shared_session(session: &SharedSession) -> Result<(i32, String), ToolError> {
    let marker = {
        let session = session
            .lock()
            .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
        if session.command.is_none() || completion(&session).is_some() {
            return Err(ToolError::execution_failed(
                "terminal session has no running foreground command",
            ));
        }
        write_bytes(&session, &[3])
            .map_err(|error| ToolError::execution_failed(format!("PTY write failed: {error}")))?;
        session
            .command
            .as_ref()
            .expect("running command checked above")
            .marker
            .clone()
    };
    let deadline = Instant::now() + CANCEL_CONFIRM_TIMEOUT;

    loop {
        std::thread::sleep(CANCEL_SENTINEL_RETRY_INTERVAL);
        let session = session
            .lock()
            .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
        if let Some(done) = completion(&session) {
            return Ok(done);
        }
        if Instant::now() >= deadline {
            return Err(ToolError::execution_failed(
                "terminal interrupt was sent, but cancellation was not confirmed within 2 seconds",
            ));
        }
        write_completion_sentinel(&session, &marker, 130).map_err(ToolError::execution_failed)?;
    }
}

#[cfg(unix)]
fn session_result(
    session: &mut TerminalSession,
    done: Option<(i32, String)>,
    timed_out: bool,
) -> ToolResult {
    let output = prune_output(&take_output(session));
    let finished = done.is_some();
    let (exit_code, cwd) = done.map_or((None, String::new()), |(code, cwd)| (Some(code), cwd));
    let status = if timed_out {
        "timed_out"
    } else if finished {
        "completed"
    } else {
        "running"
    };
    if !cwd.is_empty() {
        session.durable.last_known_cwd = PathBuf::from(&cwd);
    }
    session.durable.state = if timed_out || !finished {
        DurableTerminalState::Running
    } else if exit_code == Some(0) {
        DurableTerminalState::Idle
    } else if exit_code == Some(130) {
        DurableTerminalState::Canceled
    } else {
        DurableTerminalState::Failed
    };
    session.durable.updated_at = chrono::Utc::now().to_rfc3339();
    let persistence_error = persist_durable(&session.durable_path, &session.durable).err();
    let previous = session.durable.previous.as_deref().map(|record| {
        json!({
            "session_id": record.session_id,
            "state": record.state,
            "last_known_cwd": record.last_known_cwd,
            "updated_at": record.updated_at,
        })
    });
    ToolResult {
        content: output,
        // A successful send while the command is still running is itself a
        // successful tool operation. Completed commands still report their
        // real exit status, and timeouts remain unsuccessful.
        success: !timed_out && (!finished || exit_code == Some(0)),
        metadata: Some(json!({
            "status": status,
            "exit_code": exit_code,
            "cwd": cwd,
            "session_persistent": true,
            "durability": "live shell is process-local; identity and last-known summary persist",
            "terminal_session_id": session.durable.session_id,
            "terminal_state": session.durable.state,
            "state_path": session.durable_path,
            "previous_session": previous,
            "persistence_error": persistence_error,
        })),
    }
}

#[cfg(unix)]
fn session_name(input: &serde_json::Value, required: bool) -> Result<&str, ToolError> {
    match input.get("session").and_then(serde_json::Value::as_str) {
        Some(name) if !name.is_empty() => Ok(name),
        Some(_) => Err(ToolError::execution_failed("session must not be empty")),
        None if required => Err(ToolError::missing_field("session")),
        None => Ok("term-1"),
    }
}

#[cfg(unix)]
fn timeout_secs(input: &serde_json::Value, key: &str) -> Result<Duration, ToolError> {
    Ok(Duration::from_secs(
        optional_u64(input, key, DEFAULT_TIMEOUT_SECS)?.clamp(1, MAX_TIMEOUT_SECS),
    ))
}

fn shell_allowed(context: &ToolContext, name: &str) -> Result<(), ToolError> {
    crate::tools::execution_envelope::enforce_tool_denial(context, name, &json!({}))?;
    if matches!(name, "terminal/run" | "terminal/send" | "terminal/reset")
        && context.shell_policy != codewhale_agent_contract::worker_profile::ShellPolicy::Full
    {
        return Err(ToolError::permission_denied(
            "Persistent terminal execution and input require full shell permission.",
        ));
    }
    if context.shell_policy.allows_shell() {
        Ok(())
    } else {
        Err(ToolError::execution_failed(
            "Shell tools are disabled by the active permission profile.",
        ))
    }
}

#[cfg(not(unix))]
fn unsupported() -> ToolResult {
    ToolResult::error("Stateful terminal sessions are currently supported on Unix only.")
}

macro_rules! terminal_tool_common {
    ($name:literal, $description:literal) => {
        fn name(&self) -> &'static str {
            $name
        }
        fn description(&self) -> &'static str {
            $description
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![
                ToolCapability::ExecutesCode,
                ToolCapability::RequiresApproval,
            ]
        }
        fn approval_requirement(&self) -> ApprovalRequirement {
            ApprovalRequirement::Required
        }
    };
}

pub struct TerminalRunTool;
#[async_trait]
impl ToolSpec for TerminalRunTool {
    terminal_tool_common!(
        "terminal/run",
        "Run a command in a persistent PTY shell session. cd, exports, shell functions, and activated environments persist across calls in this process. Identity and a non-secret last-known summary persist across restarts; prior shells are surfaced as stale/lost and are never reattached. On timeout the wait is abandoned but the command keeps running in the session (use terminal/wait or cancel). (Unix only)"
    );
    fn input_schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"command":{"type":"string"},"session":{"type":"string","default":"term-1"},"timeout_secs":{"type":"integer","default":120}},"required":["command"]})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        shell_allowed(context, self.name())?;
        #[cfg(unix)]
        {
            let command = required_str(&input, "command")?.to_string();
            let name = session_name(&input, false)?.to_string();
            let session = get_or_create(
                &name,
                &context.workspace,
                pty_sandbox_policy(context)?,
                &context.shell_manager,
            )
            .map_err(ToolError::execution_failed)?;
            let timeout = timeout_secs(&input, "timeout_secs")?;
            return tokio::task::spawn_blocking(move || {
                {
                    let mut session = session.lock().map_err(|_| {
                        ToolError::execution_failed("terminal session lock poisoned")
                    })?;
                    start_command(&mut session, &command).map_err(ToolError::execution_failed)?;
                }
                let (done, timed_out) = wait_shared_session(&session, timeout)?;
                let mut session = session
                    .lock()
                    .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
                Ok(session_result(&mut session, done, timed_out))
            })
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Ok(unsupported())
        }
    }
}

pub struct TerminalSendTool;
#[async_trait]
impl ToolSpec for TerminalSendTool {
    terminal_tool_common!(
        "terminal/send",
        "Send raw input to a live persistent terminal session. Use a literal ETX control byte to interrupt an interactive process. A prior-process shell is reported as stale/lost rather than reattached. (Unix only)"
    );
    fn input_schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"session":{"type":"string"},"text":{"type":"string"},"wait_ms":{"type":"integer","default":250}},"required":["session","text"]})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        shell_allowed(context, self.name())?;
        #[cfg(unix)]
        {
            let name = session_name(&input, true)?.to_string();
            let text = required_str(&input, "text")?.as_bytes().to_vec();
            let session = find(&name, &context.workspace).map_err(ToolError::execution_failed)?;
            let wait = Duration::from_millis(optional_u64(&input, "wait_ms", 250)?.min(60_000));
            return tokio::task::spawn_blocking(move || {
                {
                    let session = session.lock().map_err(|_| {
                        ToolError::execution_failed("terminal session lock poisoned")
                    })?;
                    write_raw_input(&session, &text)
                        .map_err(|error| ToolError::execution_failed(error.to_string()))?;
                }
                std::thread::sleep(wait);
                let mut session = session
                    .lock()
                    .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
                let done = completion(&session);
                Ok(session_result(&mut session, done, false))
            })
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Ok(unsupported())
        }
    }
}

pub struct TerminalWaitTool;
#[async_trait]
impl ToolSpec for TerminalWaitTool {
    terminal_tool_common!(
        "terminal/wait",
        "Wait for the current foreground command in a live persistent terminal session and return buffered output. A prior-process shell is reported as stale/lost rather than reattached. Output buffer holds at most 512KiB; older bytes are dropped silently. (Unix only)"
    );
    fn input_schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"session":{"type":"string"},"timeout_secs":{"type":"integer","default":120}},"required":["session"]})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        shell_allowed(context, self.name())?;
        #[cfg(unix)]
        {
            let name = session_name(&input, true)?.to_string();
            let session = find(&name, &context.workspace).map_err(ToolError::execution_failed)?;
            let timeout = timeout_secs(&input, "timeout_secs")?;
            return tokio::task::spawn_blocking(move || {
                let (done, timed_out) = wait_shared_session(&session, timeout)?;
                let mut session = session
                    .lock()
                    .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
                Ok(session_result(&mut session, done, timed_out))
            })
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Ok(unsupported())
        }
    }
}

pub struct TerminalCancelTool;
#[async_trait]
impl ToolSpec for TerminalCancelTool {
    terminal_tool_common!(
        "terminal/cancel",
        "Interrupt the running foreground command with ETX. The live terminal session survives and can be reused; its non-secret summary persists. (Unix only)"
    );
    fn input_schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"session":{"type":"string"}},"required":["session"]})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        shell_allowed(context, self.name())?;
        #[cfg(unix)]
        {
            let name = session_name(&input, true)?.to_string();
            let session = find(&name, &context.workspace).map_err(ToolError::execution_failed)?;
            return tokio::task::spawn_blocking(move || {
                let done = cancel_shared_session(&session)?;
                let mut session = session
                    .lock()
                    .map_err(|_| ToolError::execution_failed("terminal session lock poisoned"))?;
                let mut result = session_result(&mut session, Some(done), false);
                result.success = true;
                if let Some(metadata) = result.metadata.as_mut() {
                    metadata["status"] = json!("canceled");
                    metadata["canceled"] = json!(true);
                }
                Ok(result)
            })
            .await
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Ok(unsupported())
        }
    }
}

pub struct TerminalResetTool;
#[async_trait]
impl ToolSpec for TerminalResetTool {
    terminal_tool_common!(
        "terminal/reset",
        "Kill and recreate a persistent terminal session with a fresh environment. This loses live cd, exports, functions, activated environments, and running work while retaining the prior historical summary. (Unix only)"
    );
    fn input_schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"session":{"type":"string"}},"required":["session"]})
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        shell_allowed(context, self.name())?;
        #[cfg(unix)]
        {
            let name = session_name(&input, true)?.to_string();
            let old = find(&name, &context.workspace).map_err(ToolError::execution_failed)?;
            let workspace = context.workspace.clone();
            let policy = pty_sandbox_policy(context)?;
            let shell_manager = context.shell_manager.clone();
            return tokio::task::spawn_blocking(move || {
                if let Ok(mut old) = old.lock() { let _ = old.child.kill(); }
                if let Ok(mut old) = old.lock() {
                    old.durable.state = DurableTerminalState::Reset;
                    old.durable.updated_at = chrono::Utc::now().to_rfc3339();
                    let _ = persist_durable(&old.durable_path, &old.durable);
                }
                let fresh = create_session(&name, &workspace, policy, &shell_manager)
                    .map_err(ToolError::execution_failed)?;
                sessions().lock().map_err(|_| ToolError::execution_failed("terminal session registry lock poisoned"))?.insert(session_key(&name, &workspace), fresh);
                Ok(ToolResult { content: format!("Reset terminal session '{name}'. Lost shell state and any running command."), success: true, metadata: Some(json!({"session":name,"reset":true,"lost_state":["cwd","environment","functions","activated environments","running command"]})) })
            }).await.map_err(|e| ToolError::execution_failed(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Ok(unsupported())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    fn test_shell_manager() -> crate::tools::shell::SharedShellManager {
        crate::tools::shell::new_shared_shell_manager(std::path::PathBuf::from("/tmp"))
    }

    use super::*;

    /// Dock tests own a workspace so parallel tests cannot share their registry
    /// entries or durable summaries. Kill and reap every owned shell on drop.
    struct DockWorkspace(tempfile::TempDir);

    impl DockWorkspace {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        fn session(&self, name: &str) -> SharedSession {
            get_or_create(
                name,
                self.path(),
                codewhale_sandbox::SandboxPolicy::DangerFullAccess,
                &test_shell_manager(),
            )
            .unwrap()
        }
    }

    impl Drop for DockWorkspace {
        fn drop(&mut self) {
            let workspace = session_key("", self.path()).workspace;
            let mut owned = Vec::new();
            if let Ok(mut registry) = sessions().lock() {
                registry.retain(|key, session| {
                    if key.workspace == workspace {
                        owned.push(Arc::clone(session));
                        false
                    } else {
                        true
                    }
                });
            }
            for session in owned {
                if let Ok(mut session) = session.lock()
                    && session.child.kill().is_ok()
                {
                    let _ = session.child.wait();
                }
            }
        }
    }

    fn fresh(name: &str) -> SharedSession {
        let session = get_or_create(
            name,
            std::path::Path::new("/tmp"),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        let mut session_guard = session.lock().unwrap();
        let _ = session_guard.child.kill();
        drop(session_guard);
        let replacement = create_session(
            name,
            std::path::Path::new("/tmp"),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        sessions().lock().unwrap().insert(
            session_key(name, Path::new("/tmp")),
            Arc::clone(&replacement),
        );
        replacement
    }

    fn run(session: &SharedSession, command: &str, timeout: Duration) -> ToolResult {
        let mut session = session.lock().unwrap();
        start_command(&mut session, command).unwrap();
        let (done, timed_out) = wait_session(&mut session, timeout);
        session_result(&mut session, done, timed_out)
    }

    #[test]
    fn prune_output_keeps_the_tail_and_counts_what_it_dropped() {
        let input = format!(
            "first-line\n{}\nerror: the last line é\n",
            "x".repeat(OUTPUT_LIMIT * 2)
        );
        let pruned = prune_output(&input);
        let (head, rest) = pruned
            .split_once("\n… [output truncated: ")
            .expect("truncation marker");
        let (omitted, tail) = rest.split_once(" bytes omitted] …\n").expect("count");
        let omitted: usize = omitted.parse().expect("omitted count");
        assert!(head.starts_with("first-line"));
        assert!(tail.ends_with("error: the last line é\n"), "{tail:?}");
        assert!(tail.len() > OUTPUT_LIMIT / 2, "tail kept only {tail:?}");
        assert_eq!(head.len() + omitted + tail.len(), input.len());
    }

    // The whole family is compiled and registered on Unix only, a run's
    // timeout abandons the wait without stopping the command, and the output
    // ring silently drops the oldest bytes past 512KiB — the descriptions
    // must say all three so the model can plan around them.
    #[test]
    fn terminal_descriptions_disclose_platform_and_wait_semantics() {
        let descriptions = [
            (TerminalRunTool.name(), TerminalRunTool.description()),
            (TerminalSendTool.name(), TerminalSendTool.description()),
            (TerminalWaitTool.name(), TerminalWaitTool.description()),
            (TerminalCancelTool.name(), TerminalCancelTool.description()),
            (TerminalResetTool.name(), TerminalResetTool.description()),
        ];
        for (name, description) in descriptions {
            assert!(
                description.contains("(Unix only)"),
                "{name} must disclose its platform restriction: {description}"
            );
        }
        assert!(
            TerminalRunTool
                .description()
                .contains("the command keeps running in the session"),
            "terminal/run must disclose that a timeout abandons the wait, not the command"
        );
        assert!(
            TerminalWaitTool.description().contains("512KiB"),
            "terminal/wait must disclose the retained-output bound"
        );
    }

    #[test]
    #[cfg(unix)]
    fn cd_persists_between_runs() {
        let session = fresh("test-cd");
        let _ = run(
            &session,
            "mkdir -p /tmp/cw-term-cd-proof && cd /tmp/cw-term-cd-proof",
            Duration::from_secs(3),
        );
        // A separate run must still be inside the directory — this is the
        // whole point of the stateful session (EXEC-001).
        let result = run(&session, "pwd", Duration::from_secs(3));
        assert!(result.content.contains("cw-term-cd-proof"), "{}", {
            &result.content
        });
    }

    #[test]
    #[cfg(unix)]
    fn export_persists_and_sessions_are_isolated() {
        let one = fresh("test-env-one");
        let two = fresh("test-env-two");
        let _ = run(&one, "export CW_TERM_TEST=present", Duration::from_secs(3));
        assert!(
            run(&one, "printf %s $CW_TERM_TEST", Duration::from_secs(3))
                .content
                .contains("present")
        );
        assert!(
            !run(
                &two,
                "printf %s ${CW_TERM_TEST-unset}",
                Duration::from_secs(3)
            )
            .content
            .contains("present")
        );
    }

    #[test]
    #[cfg(unix)]
    fn reset_replaces_shell_environment() {
        let session = fresh("test-reset");
        let _ = run(
            &session,
            "export CW_TERM_RESET=present",
            Duration::from_secs(3),
        );
        assert!(
            run(&session, "printf %s $CW_TERM_RESET", Duration::from_secs(3))
                .content
                .contains("present")
        );
        let _ = session.lock().unwrap().child.kill();
        let replacement = create_session(
            "test-reset",
            std::path::Path::new("/tmp"),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        sessions().lock().unwrap().insert(
            session_key("test-reset", Path::new("/tmp")),
            Arc::clone(&replacement),
        );
        assert!(
            !run(
                &replacement,
                "printf %s ${CW_TERM_RESET-unset}",
                Duration::from_secs(3)
            )
            .content
            .contains("present")
        );
    }

    #[test]
    #[cfg(unix)]
    fn timeout_leaves_session_alive() {
        let session = fresh("test-timeout");
        let result = run(
            &session,
            "printf before; sleep 2",
            Duration::from_millis(100),
        );
        assert_eq!(result.metadata.as_ref().unwrap()["status"], "timed_out");
        let result = {
            let mut session_guard = session.lock().unwrap();
            let (done, timed_out) = wait_session(&mut session_guard, Duration::from_secs(3));
            session_result(&mut session_guard, done, timed_out)
        };
        assert_eq!(result.metadata.as_ref().unwrap()["status"], "completed");
        let result = run(&session, "printf after", Duration::from_secs(3));
        assert!(result.content.contains("after"));
    }

    #[test]
    #[cfg(unix)]
    fn cancel_interrupts_sleep_and_session_survives() {
        let session = fresh("test-cancel");
        // Prove the interactive shell is initialized before interrupting it.
        // A SIGINT delivered while `sh -i` is still starting can kill the
        // shell itself, which later surfaces as EIO on the next PTY write
        // (hosted macOS run 34716492759 under load).
        assert!(
            run(&session, "printf ready", Duration::from_secs(10))
                .content
                .contains("ready")
        );
        let worker = Arc::clone(&session);
        {
            let mut guard = worker.lock().unwrap();
            // The quoted split keeps the marker out of the echoed command
            // line, so seeing it proves the shell reached this command.
            start_command(&mut guard, "printf 'st''arted'; sleep 10").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let started = output_snapshot(&session.lock().unwrap()).contains("started");
            if started {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "shell never reached the sleep command"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let handle = std::thread::spawn(move || {
            let (done, timed_out) = wait_shared_session(&worker, Duration::from_secs(30)).unwrap();
            let mut guard = worker.lock().unwrap();
            session_result(&mut guard, done, timed_out)
        });
        std::thread::sleep(Duration::from_millis(150));
        let done = cancel_shared_session(&session).unwrap();
        let result = handle.join().unwrap();
        assert_eq!(done.0, 130);
        assert_eq!(result.metadata.as_ref().unwrap()["exit_code"], 130);
        assert!(
            run(&session, "printf alive", Duration::from_secs(3))
                .content
                .contains("alive")
        );
    }

    #[test]
    fn narrowed_posture_without_a_backend_fails_closed() {
        assert!(pty_lacks_required_sandbox(
            &codewhale_sandbox::SandboxPolicy::read_only(),
            codewhale_sandbox::SandboxType::None,
        ));
        assert!(!pty_lacks_required_sandbox(
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            codewhale_sandbox::SandboxType::None,
        ));
    }

    #[test]
    fn full_access_prepares_an_unsandboxed_pty() {
        let (prepared, confined) = prepare_pty_shell(
            "/bin/sh",
            Path::new("/tmp"),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .expect("full access may start a PTY");
        assert!(!confined);
        assert_eq!(prepared.command[0], "/bin/sh");
        assert_eq!(prepared.command[1], "-i");
    }

    #[test]
    #[cfg(unix)]
    fn session_names_are_scoped_to_workspace() {
        let first_workspace = tempfile::tempdir().unwrap();
        let second_workspace = tempfile::tempdir().unwrap();
        let first = get_or_create(
            "shared-name",
            first_workspace.path(),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        let second = get_or_create(
            "shared-name",
            second_workspace.path(),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(find("shared-name", first_workspace.path()).is_ok());
        assert!(find("shared-name", second_workspace.path()).is_ok());
        assert!(find("shared-name", Path::new("/tmp")).is_err());
    }

    #[test]
    fn dock_snapshot_scopes_identities_and_leaves_the_model_cursor_untouched() {
        let one = DockWorkspace::new();
        let two = DockWorkspace::new();
        let first = one.session("shared-name");
        let second = two.session("shared-name");
        let first_id = first.lock().unwrap().durable.session_id.clone();
        let second_id = second.lock().unwrap().durable.session_id.clone();
        // Stop the output producer before seeding a deterministic retained tail.
        // This verifies the dock projection independently of shell throughput.
        {
            let mut session = first.lock().unwrap();
            session.child.kill().unwrap();
            session.child.wait().unwrap();
            session.read_cursor = 23;
            // The detached PTY reader may still drain its final bytes; give
            // this projection fixture an independent buffer after reaping.
            session.output = Arc::new(Mutex::new(OutputBuffer::default()));
            let mut output = session.output.lock().unwrap();
            output.append(&vec![b'x'; BUFFER_LIMIT + 41]);
            output.append(b"dock-tail-proof");
        }
        let (entries, output) = inspect_workspace(one.path(), Some(&first_id)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, first_id);
        assert!(entries[0].last_activity.is_some());
        let output = output.unwrap();
        assert_eq!(output.bytes.len(), READ_LIMIT);
        assert_eq!(output.offset, output.total - READ_LIMIT as u64);
        assert!(output.bytes.ends_with(b"dock-tail-proof"));
        assert!(output.dropped > 0);
        assert_eq!(first.lock().unwrap().read_cursor, 23);
        // A foreign UUID never chooses a same-named local shell or falls back.
        let (entries, output) = inspect_workspace(one.path(), Some(&second_id)).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(output.is_none());
        let error = write_dock_input(
            two.path(),
            "shared-name",
            &first_id,
            b"printf wrong-workspace\n",
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap_err();
        assert!(error.contains("reset"), "{error}");
    }

    #[test]
    fn dock_never_attaches_to_a_reset_name_without_selecting_its_new_identity() {
        let workspace = DockWorkspace::new();
        let old = workspace.session("replaceable");
        let old_id = old.lock().unwrap().durable.session_id.clone();
        {
            let mut old = old.lock().unwrap();
            old.child.kill().unwrap();
            old.child.wait().unwrap();
        }
        let replacement = create_session(
            "replaceable",
            workspace.path(),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        let new_id = replacement.lock().unwrap().durable.session_id.clone();
        sessions().lock().unwrap().insert(
            session_key("replaceable", workspace.path()),
            Arc::clone(&replacement),
        );
        let (entries, output) = inspect_workspace(workspace.path(), Some(&old_id)).unwrap();
        assert_eq!(entries[0].id, new_id);
        assert!(output.is_none());
        let error = write_dock_input(
            workspace.path(),
            "replaceable",
            &old_id,
            b"touch stale-input-must-not-run\n",
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap_err();
        assert!(
            error.contains("select its replacement explicitly"),
            "{error}"
        );
        assert!(resize_dock_session(&replacement.lock().unwrap(), &old_id, 40, 80).is_err());
        assert!(!workspace.path().join("stale-input-must-not-run").exists());
        // Refusal preserves the replacement and its environment instead of
        // silently resetting it to recover from the stale selection.
        assert_eq!(
            lookup("replaceable", workspace.path())
                .unwrap()
                .lock()
                .unwrap()
                .durable
                .session_id,
            new_id
        );
    }

    #[test]
    fn dock_and_model_reuse_refuse_changed_restricted_policies() {
        use codewhale_sandbox::SandboxPolicy;
        let workspace = DockWorkspace::new();
        let shell = workspace.session("posture");
        let id = shell.lock().unwrap().durable.session_id.clone();
        let narrowed = SandboxPolicy::read_only();
        assert!(
            get_or_create(
                "posture",
                workspace.path(),
                narrowed.clone(),
                &test_shell_manager()
            )
            .is_err()
        );
        assert!(write_dock_input(workspace.path(), "posture", &id, b"x", &narrowed).is_err());

        // The process creation boundary is tested separately against real
        // sandbox backends. Seed its recorded policy here so this regression
        // stays portable even on hosts without seatbelt/bwrap.
        let original = SandboxPolicy::workspace_with_roots(
            vec![workspace.path().join("extra-write-root")],
            true,
        );
        {
            let mut shell = shell.lock().unwrap();
            shell.sandbox_confined = true;
            shell.sandbox_policy = original.clone();
        }
        let changed = [
            SandboxPolicy::read_only(),
            SandboxPolicy::workspace_with_roots(vec![], true),
            SandboxPolicy::workspace_with_roots(
                vec![workspace.path().join("extra-write-root")],
                false,
            ),
        ];
        for policy in changed {
            assert!(
                get_or_create(
                    "posture",
                    workspace.path(),
                    policy.clone(),
                    &test_shell_manager()
                )
                .is_err()
            );
            assert!(write_dock_input(workspace.path(), "posture", &id, b"x", &policy).is_err());
        }
        assert!(Arc::ptr_eq(
            &get_or_create("posture", workspace.path(), original, &test_shell_manager()).unwrap(),
            &shell
        ));
        assert_eq!(shell.lock().unwrap().durable.session_id, id);
    }

    #[test]
    fn dock_input_refuses_an_exited_shell() {
        let workspace = DockWorkspace::new();
        let shell = workspace.session("exited");
        let id = {
            let mut shell = shell.lock().unwrap();
            let id = shell.durable.session_id.clone();
            shell.child.kill().unwrap();
            shell.child.wait().unwrap();
            id
        };
        let error = write_dock_input(
            workspace.path(),
            "exited",
            &id,
            b"must-not-run\n",
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap_err();
        assert!(error.contains("has exited"), "{error}");
    }

    #[test]
    fn raw_input_refuses_oversized_canonical_batches_without_touching_pending_read() {
        let workspace = DockWorkspace::new();
        let shell = workspace.session("paste-guard");
        let id = {
            let mut shell = shell.lock().unwrap();
            start_command(
                &mut shell,
                "stty icanon; printf 'paste-gu''ard-ready\\n'; IFS= read -r answer; printf 'answer=%s\\n' \"$answer\"",
            )
            .unwrap();
            shell.durable.session_id.clone()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output_snapshot(&shell.lock().unwrap()).contains("paste-guard-ready") {
            assert!(Instant::now() < deadline, "shell did not reach its read");
            std::thread::sleep(Duration::from_millis(25));
        }
        let error = write_raw_input(&shell.lock().unwrap(), &vec![b'x'; 64 * 1024]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("no input was sent"));
        // The dock must use the same boundary, including its exact lower edge.
        let error = write_dock_input(
            workspace.path(),
            "paste-guard",
            &id,
            &[b'x'; 1024],
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap_err();
        assert!(error.contains("no input was sent"), "{error}");
        write_dock_input(
            workspace.path(),
            "paste-guard",
            &id,
            b"human-answer\n",
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap();
        let (done, timed_out) = wait_shared_session(&shell, Duration::from_secs(10)).unwrap();
        assert!(!timed_out, "refused paste stranded the pending read");
        assert_eq!(done.unwrap().0, 0);
        let output = output_snapshot(&shell.lock().unwrap());
        assert!(output.contains("answer=human-answer"), "{output}");
        assert!(
            !output.contains("xxxxxxxx"),
            "refused input leaked into the PTY"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_send_wait_releases_the_session_for_dock_snapshot_and_input() {
        let workspace = DockWorkspace::new();
        let shell = workspace.session("send-wait");
        let (id, output) = {
            let mut shell = shell.lock().unwrap();
            start_command(
                &mut shell,
                "stty icanon echo; printf 'send-wa''it-ready\\n'; IFS= read -r answer; printf 'answer=%s\\n' \"$answer\"",
            )
            .unwrap();
            (shell.durable.session_id.clone(), Arc::clone(&shell.output))
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output.lock().unwrap().text().contains("send-wait-ready") {
            assert!(Instant::now() < deadline, "shell did not reach its read");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let context = ToolContext::new(workspace.path());
        let send = tokio::spawn(async move {
            TerminalSendTool
                .execute(
                    json!({"session":"send-wait", "text":"model-", "wait_ms":3000}),
                    &context,
                )
                .await
        });
        // Observe the PTY reader directly: taking the session mutex here would
        // hide the old bug by waiting for terminal/send to finish first.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output.lock().unwrap().text().contains("model-") {
            assert!(Instant::now() < deadline, "terminal/send did not write");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            !send.is_finished(),
            "terminal/send already finished its wait"
        );
        let path = workspace.path().to_path_buf();
        let mut dock = tokio::task::spawn_blocking(move || {
            let snapshot = inspect_workspace(&path, Some(&id))?;
            write_dock_input(
                &path,
                "send-wait",
                &id,
                b"human\n",
                &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            )?;
            Ok::<_, String>(snapshot)
        });
        let quick_dock = tokio::time::timeout(Duration::from_secs(1), &mut dock).await;
        let responsive = quick_dock.is_ok() && !send.is_finished();
        // Join both workers even on the old lock-held path before asserting,
        // so a failed regression cannot leave work using the owned fixture.
        let (entries, snapshot) = match quick_dock {
            Ok(result) => result,
            Err(_) => dock.await,
        }
        .unwrap()
        .unwrap();
        let result = send.await.unwrap().unwrap();
        assert!(responsive, "terminal/send held the session during wait_ms");
        assert_eq!(entries[0].state, DurableTerminalState::Running);
        assert!(String::from_utf8_lossy(&snapshot.unwrap().bytes).contains("model-"));
        assert!(result.content.contains("answer=model-human"), "{result:?}");
        assert_eq!(result.metadata.unwrap()["exit_code"], 0);
    }

    #[test]
    fn interactive_command_keeps_its_completion_marker_out_of_stdin() {
        let workspace = DockWorkspace::new();
        let shell = workspace.session("interactive");
        let id = {
            let mut shell = shell.lock().unwrap();
            start_command(
                &mut shell,
                "printf 'ready-for-human\\n'; IFS= read -r answer; printf 'answer=%s\\n' \"$answer\"\n# trailing comment",
            )
            .unwrap();
            shell.durable.session_id.clone()
        };
        let (done, timed_out) = wait_shared_session(&shell, Duration::from_millis(200)).unwrap();
        assert!(timed_out && done.is_none());
        write_dock_input(
            workspace.path(),
            "interactive",
            &id,
            b"human-answer\n",
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap();
        let (done, timed_out) = wait_shared_session(&shell, Duration::from_secs(10)).unwrap();
        assert!(!timed_out, "interactive command lost its completion marker");
        assert_eq!(done.unwrap().0, 0);
        assert!(output_snapshot(&shell.lock().unwrap()).contains("answer=human-answer"));
        let result = run(
            &shell,
            "cat <<'CW_EOF'\nheredoc-proof\nCW_EOF\nfalse # preserve status",
            Duration::from_secs(10),
        );
        assert!(result.content.contains("heredoc-proof"));
        assert_eq!(result.metadata.unwrap()["exit_code"], 1);
        let alias = run(
            &shell,
            "alias cw_dock_alias='printf alias-proof'\ncw_dock_alias",
            Duration::from_secs(10),
        );
        assert!(alias.content.contains("alias-proof"));
        assert_eq!(alias.metadata.unwrap()["exit_code"], 0);
    }

    #[test]
    fn dock_input_can_interrupt_a_command_while_the_model_waits() {
        let workspace = DockWorkspace::new();
        let shell = workspace.session("waiting");
        assert!(
            run(&shell, "printf ready", Duration::from_secs(10))
                .content
                .contains("ready")
        );
        let id = {
            let mut shell = shell.lock().unwrap();
            start_command(&mut shell, "printf 'dock-wa''it-ready'; sleep 30").unwrap();
            shell.durable.session_id.clone()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output_snapshot(&shell.lock().unwrap()).contains("dock-wait-ready") {
            assert!(Instant::now() < deadline, "shell did not start its command");
            std::thread::sleep(Duration::from_millis(25));
        }
        let waiter_shell = Arc::clone(&shell);
        let waiter = std::thread::spawn(move || {
            wait_shared_session(&waiter_shell, Duration::from_secs(10)).unwrap()
        });
        write_dock_input(
            workspace.path(),
            "waiting",
            &id,
            &[3],
            &codewhale_sandbox::SandboxPolicy::DangerFullAccess,
        )
        .unwrap();
        let (done, timed_out) = waiter.join().unwrap();
        assert!(!timed_out, "dock input was blocked behind the model waiter");
        assert_eq!(done.unwrap().0, 130);
        assert!(
            run(&shell, "printf 'still-alive'", Duration::from_secs(3))
                .content
                .contains("still-alive")
        );
    }

    #[test]
    #[cfg(unix)]
    fn durable_summary_marks_prior_process_shell_stale_and_preserves_history() {
        let workspace = tempfile::tempdir().unwrap();
        let canonical_workspace = workspace.path().canonicalize().unwrap();
        let name = format!("restart-proof-{}", Uuid::new_v4());
        let first = create_session(
            &name,
            workspace.path(),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        let first_record = first.lock().unwrap().durable.clone();
        assert_eq!(first_record.state, DurableTerminalState::Idle);
        assert_eq!(first_record.last_known_cwd, canonical_workspace);
        assert!(!first_record.session_id.is_empty());

        // Removing only the process-local registry entry models a restart.
        // The durable record remains, but cannot be used to reattach.
        sessions()
            .lock()
            .unwrap()
            .remove(&session_key(&name, workspace.path()));
        let stale = match find(&name, workspace.path()) {
            Ok(_) => panic!("persisted session must not be reattached"),
            Err(error) => error,
        };
        assert!(stale.contains("stale/lost"), "{stale}");
        assert!(stale.contains("start a replacement"), "{stale}");

        let replacement = create_session(
            &name,
            workspace.path(),
            codewhale_sandbox::SandboxPolicy::DangerFullAccess,
            &test_shell_manager(),
        )
        .unwrap();
        let replacement = replacement.lock().unwrap();
        assert_ne!(replacement.durable.session_id, first_record.session_id);
        let previous = replacement.durable.previous.as_deref().unwrap();
        assert_eq!(previous.session_id, first_record.session_id);
        assert_eq!(previous.state, DurableTerminalState::StaleLost);
        assert_eq!(previous.last_known_cwd, canonical_workspace);
        assert_ne!(replacement.durable.runtime_nonce, "");
    }

    #[test]
    #[cfg(unix)]
    fn output_is_capped_with_notice() {
        let session = fresh("test-output-cap");
        let result = run(&session, "yes x | head -n 100000", Duration::from_secs(3));
        assert!(result.content.len() <= OUTPUT_LIMIT + 100);
        assert!(result.content.contains("output truncated"));
    }

    /// The cursor contract the Engine byte stream rides on: offsets are
    /// absolute for the life of the PTY, reads never consume, and a cursor the
    /// retained window has moved past is reported rather than silently
    /// answered from the middle of the stream.
    #[test]
    #[cfg(unix)]
    fn bounded_replay_is_absolute_non_consuming_and_reports_its_gap() {
        let mut buffer = OutputBuffer::default();
        buffer.append(b"hello");
        let first = buffer.read_since(0, 4);
        assert_eq!(first.bytes, b"hell");
        assert_eq!(first.offset, 0);
        assert_eq!(first.next_cursor, 4);
        assert_eq!(first.total, 5);
        assert_eq!(first.dropped, 0);
        assert!(!first.gap);
        // A second read at the same cursor returns the same bytes: the caller
        // owns the position, so replay is idempotent.
        assert_eq!(buffer.read_since(0, 4), first);
        let rest = buffer.read_since(first.next_cursor, 4);
        assert_eq!(rest.bytes, b"o");
        assert_eq!(rest.offset, 4);
        assert_eq!(rest.next_cursor, 5);

        // Past the ring: the first 32 bytes are gone and the chunk says so.
        let mut wrapped = OutputBuffer::default();
        wrapped.append(&vec![b'x'; BUFFER_LIMIT + 32]);
        let lost = wrapped.read_since(0, 8);
        assert!(lost.gap, "a cursor below the retained window is a gap");
        assert_eq!(lost.dropped, 32);
        assert_eq!(
            lost.offset, 32,
            "the chunk starts at the oldest retained byte"
        );
        assert_eq!(lost.total, BUFFER_LIMIT as u64 + 32);
        assert_eq!(lost.bytes, vec![b'x'; 8]);
        assert_eq!(lost.next_cursor, 40);

        // A cursor at the head is not a gap and returns nothing new.
        let current = wrapped.read_since(BUFFER_LIMIT as u64 + 32, 8);
        assert!(!current.gap);
        assert!(current.bytes.is_empty());
        assert_eq!(current.next_cursor, BUFFER_LIMIT as u64 + 32);

        // A cursor past the head clamps to it instead of being echoed back:
        // a client that continues from `next_cursor` must not skip the bytes
        // the stream produces before it reaches the bogus position.
        let beyond = wrapped.read_since(BUFFER_LIMIT as u64 + 4096, 8);
        assert!(!beyond.gap);
        assert!(beyond.bytes.is_empty());
        assert_eq!(beyond.offset, BUFFER_LIMIT as u64 + 32);
        assert_eq!(beyond.next_cursor, BUFFER_LIMIT as u64 + 32);
    }

    /// The session-level entry point an Engine byte stream will call: absolute
    /// cursor, non-consuming, clamped, and honest about a cursor ahead of the
    /// stream.
    #[test]
    #[cfg(unix)]
    fn session_read_since_is_non_consuming_and_clamped() {
        let session = fresh("test-read-since");
        let _ = run(
            &session,
            "printf 'cw-replay-proof\\n'",
            Duration::from_secs(3),
        );
        // The tool-result path already consumed this output through its own
        // cursor; an absolute cursor still reads it, which is the point.
        let printed = read_session_since(&session.lock().unwrap(), 0, usize::MAX).unwrap();
        assert!(
            String::from_utf8_lossy(&printed.bytes).contains("cw-replay-proof"),
            "{}",
            String::from_utf8_lossy(&printed.bytes)
        );
        let again = read_session_since(&session.lock().unwrap(), 0, usize::MAX).unwrap();
        assert_eq!(again.bytes, printed.bytes, "a replay read must not consume");

        // A cursor ahead of the stream is not a gap: nothing was lost, there
        // is simply nothing there yet — and the answer clamps to the head so
        // continuing from it cannot skip what arrives next.
        let ahead =
            read_session_since(&session.lock().unwrap(), printed.next_cursor + 4096, 16).unwrap();
        assert!(!ahead.gap);
        assert!(ahead.bytes.is_empty());
        assert_eq!(ahead.next_cursor, ahead.total);

        // More than one response's worth of output proves the clamp.
        let large = fresh("test-read-since-clamp");
        let _ = run(&large, "yes x | head -n 100000", Duration::from_secs(3));
        let chunk = read_session_since(&large.lock().unwrap(), 0, usize::MAX).unwrap();
        assert_eq!(chunk.bytes.len(), READ_LIMIT);
        assert!(!chunk.gap);
        assert_eq!(chunk.next_cursor, READ_LIMIT as u64);
    }

    /// Resize has to reach the kernel, not just a field: `get_size` reads the
    /// window back from the pty, and the shell reports it through `stty`.
    #[test]
    #[cfg(unix)]
    fn resize_reaches_the_kernel_and_the_live_shell() {
        let session = fresh("test-resize");
        {
            let guard = session.lock().unwrap();
            assert_eq!(guard.master.get_size().unwrap().rows, 24);
        }
        resize_session(&session.lock().unwrap(), 40, 100).unwrap();
        let size = session.lock().unwrap().master.get_size().unwrap();
        assert_eq!((size.rows, size.cols), (40, 100));
        let result = run(&session, "stty size", Duration::from_secs(3));
        assert!(
            result.content.contains("40 100"),
            "the shell should see the new window: {}",
            result.content
        );
    }

    /// Exit is observable: a killed shell reports a status instead of looking
    /// alive forever (the "pretending to reattach" failure the durable record
    /// is written to avoid).
    #[test]
    #[cfg(unix)]
    fn killed_shell_reports_an_exit_status() {
        let session = fresh("test-exit-status");
        {
            let mut guard = session.lock().unwrap();
            assert!(
                session_exit_status(&mut guard).unwrap().is_none(),
                "fresh shell is alive"
            );
            kill_session(&mut guard).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let exited = session_exit_status(&mut session.lock().unwrap())
                .unwrap()
                .is_some();
            if exited {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "a killed shell must report its exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn new_session_does_not_inherit_parent_secret_env() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _env_lock = lock_test_env();
        let _secret = EnvVarGuard::set("CODEWHALE_TEST_PTY_SECRET", "pty-secret-value");
        let session = fresh("test-env-scrub");
        let result = run(
            &session,
            "printf 'se''cret=%s ho''me=%s' \"${CODEWHALE_TEST_PTY_SECRET-unset}\" \"${HOME-none}\"",
            Duration::from_secs(10),
        );
        assert!(
            !result.content.contains("pty-secret-value"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("secret=unset"),
            "{}",
            result.content
        );
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert!(
                result.content.contains(&format!("home={home}")),
                "allowlisted variables still reach the shell: {}",
                result.content
            );
        }
    }
}

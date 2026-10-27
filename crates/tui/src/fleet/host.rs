//! Fleet worker host adapters.
//!
//! Adapters own process boundaries for worker hosts. The manager can lease and
//! observe work through this trait without knowing whether the worker is a
//! local child process or an SSH-backed remote command.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use codewhale_protocol::fleet::FleetHostSpec;
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(unix)]
use std::sync::OnceLock;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows::core::PCWSTR;

const DEFAULT_LOG_LIMIT_BYTES: usize = 64 * 1024;
const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 10;
const WORKER_STOP_GRACE: Duration = Duration::from_millis(750);

pub type FleetHostResult<T> = Result<T, FleetHostError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetHostErrorKind {
    Retryable,
    Terminal,
    Configuration,
}

#[derive(Debug, Error)]
#[error("{kind:?}: {message}")]
pub struct FleetHostError {
    pub kind: FleetHostErrorKind,
    pub message: String,
}

impl FleetHostError {
    fn retryable(message: impl Into<String>) -> Self {
        Self {
            kind: FleetHostErrorKind::Retryable,
            message: message.into(),
        }
    }

    fn terminal(message: impl Into<String>) -> Self {
        Self {
            kind: FleetHostErrorKind::Terminal,
            message: message.into(),
        }
    }

    fn configuration(message: impl Into<String>) -> Self {
        Self {
            kind: FleetHostErrorKind::Configuration,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetWorkerCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl FleetWorkerCommand {
    pub fn new<S, I, A>(program: S, args: I) -> Self
    where
        S: Into<String>,
        I: IntoIterator<Item = A>,
        A: Into<String>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FleetWorkerStartRequest {
    pub worker_id: String,
    pub attempt: Option<super::executor::FleetExecutorAttempt>,
    pub command: FleetWorkerCommand,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub env_allowlist: BTreeSet<String>,
    pub log_limit_bytes: usize,
}

impl FleetWorkerStartRequest {
    pub fn new(worker_id: impl Into<String>, command: FleetWorkerCommand) -> Self {
        Self {
            worker_id: worker_id.into(),
            attempt: None,
            command,
            cwd: None,
            env: BTreeMap::new(),
            env_allowlist: BTreeSet::new(),
            log_limit_bytes: DEFAULT_LOG_LIMIT_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetWorkerHandle {
    pub transport_receipt: Option<codewhale_protocol::fleet::FleetArtifactRef>,
    pub worker_id: String,
    pub host_kind: FleetHostKind,
    pub pid: Option<u32>,
    pub log_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetHostKind {
    LocalProcess,
    Ssh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetHostWorkerState {
    Running,
    /// The dispatcher stopped but the owned process session/job is not yet empty.
    Draining,
    Exited,
    Failed,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetHostWorkerStatus {
    pub worker_id: String,
    pub state: FleetHostWorkerState,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub memory_mb: Option<u64>,
    pub retryable: bool,
}

pub trait FleetHostAdapter {
    fn start_worker(
        &mut self,
        request: FleetWorkerStartRequest,
    ) -> FleetHostResult<FleetWorkerHandle>;
    fn read_status(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus>;
    fn read_logs(&self, worker_id: &str, max_bytes: usize) -> FleetHostResult<String>;
    fn interrupt_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus>;
    fn restart_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetWorkerHandle>;
    fn stop_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus>;
    fn cleanup_worker(&mut self, worker_id: &str) -> FleetHostResult<()>;
}

#[derive(Debug)]
pub struct LocalProcessFleetHostAdapter {
    workspace: PathBuf,
    processes: BTreeMap<String, LocalWorkerProcess>,
}

#[derive(Debug)]
struct LocalWorkerProcess {
    request: FleetWorkerStartRequest,
    child: Child,
    #[cfg(unix)]
    session_id: libc::pid_t,
    #[cfg(unix)]
    parent_death_writer: Option<std::io::PipeWriter>,
    #[cfg(windows)]
    windows_job: FleetWindowsJob,
    host_kind: FleetHostKind,
    log_path: PathBuf,
    stopped: bool,
    last_exit: Option<ExitStatus>,
    last_memory_mb: Option<u64>,
    /// When `ps` last sampled this worker. Status polls can run every few
    /// milliseconds; memory is display data, so one sample a second is ample.
    last_memory_sample: Option<std::time::Instant>,
}

/// Minimum spacing between `ps` memory samples for one worker.
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

impl LocalProcessFleetHostAdapter {
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
            processes: BTreeMap::new(),
        }
    }

    fn start_with_kind(
        &mut self,
        request: FleetWorkerStartRequest,
        host_kind: FleetHostKind,
    ) -> FleetHostResult<FleetWorkerHandle> {
        validate_worker_id(&request.worker_id)?;
        if self.processes.contains_key(&request.worker_id) {
            let status = self.read_status(&request.worker_id)?;
            if matches!(status.state, FleetHostWorkerState::Running) {
                return Err(FleetHostError::terminal(format!(
                    "worker {} is already running",
                    request.worker_id
                )));
            }
            // A draining worker's dispatcher exited but its tree still runs;
            // forgetting it here would overlap a replacement with it.
            if matches!(status.state, FleetHostWorkerState::Draining) {
                return Err(FleetHostError::retryable(format!(
                    "worker {} is still draining; stop it before starting a replacement",
                    request.worker_id
                )));
            }
            self.processes.remove(&request.worker_id);
        }

        let env = worker_env(&request.env, &request.env_allowlist)?;
        let log_path = self.log_path_for(&request.worker_id, host_kind);
        let log = open_worker_log(&self.workspace, &log_path)?;
        let stderr = log
            .try_clone()
            .map_err(|err| FleetHostError::retryable(format!("cloning worker log: {err}")))?;

        let mut command = Command::new(&request.command.program);
        // Parent-death watch (R7): the worker's stdin is the read end of a pipe
        // whose write end lives only in this adapter process. If the manager
        // dies (crash, kill, power loss), the kernel closes the write end, the
        // worker sees stdin EOF, and `--parent-death-watch` shuts the worker
        // tree down instead of letting it spend forever. Windows workers are
        // contained in a Job Object that the OS terminates on parent death.
        #[cfg(unix)]
        let (parent_death_writer, stdin) = {
            let (reader, writer) = std::io::pipe().map_err(|err| {
                FleetHostError::retryable(format!("creating parent-death pipe: {err}"))
            })?;
            (Some(writer), std::process::Stdio::from(reader))
        };
        #[cfg(not(unix))]
        let stdin = Stdio::null();
        command
            .args(&request.command.args)
            .stdin(stdin)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .env_clear()
            .envs(env);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }

        // Fleet owns the complete worker tree, not only the dispatcher PID.
        // `codewhale` spawns `codewhale-tui`, which can in turn spawn tool
        // processes; isolating the root prevents a stop from signalling the
        // operator's own process group.
        #[cfg(unix)]
        // SAFETY: `setsid` is async-signal-safe and the closure does not touch
        // allocator or parent-held state between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }

        let child = command.spawn().map_err(|err| {
            classify_spawn_error(err, format!("starting worker {}", request.worker_id))
        })?;
        #[cfg(windows)]
        let (child, windows_job) = attach_fleet_windows_job(child).map_err(|err| {
            FleetHostError::retryable(format!(
                "containing worker {} in a Windows Job Object: {err}",
                request.worker_id
            ))
        })?;
        let pid = child.id();
        let handle = FleetWorkerHandle {
            transport_receipt: None,
            worker_id: request.worker_id.clone(),
            host_kind,
            pid: Some(pid),
            log_path: log_path.clone(),
        };
        self.processes.insert(
            request.worker_id.clone(),
            LocalWorkerProcess {
                request,
                child,
                // SAFETY: `setsid` is async-signal-safe and the closure does not
                // touch allocator or parent-held state between fork and exec.
                #[cfg(unix)]
                session_id: pid as libc::pid_t,
                #[cfg(unix)]
                parent_death_writer,
                #[cfg(windows)]
                windows_job,
                host_kind,
                log_path,
                stopped: false,
                last_exit: None,
                last_memory_mb: None,
                last_memory_sample: None,
            },
        );
        Ok(handle)
    }

    fn log_path_for(&self, worker_id: &str, host_kind: FleetHostKind) -> PathBuf {
        let host_dir = match host_kind {
            FleetHostKind::LocalProcess => "local",
            FleetHostKind::Ssh => "ssh",
        };
        self.workspace
            .join(".codewhale")
            .join("fleet-host")
            .join(host_dir)
            .join(format!("{}.log", safe_path_segment(worker_id)))
    }
}

impl FleetHostAdapter for LocalProcessFleetHostAdapter {
    fn start_worker(
        &mut self,
        request: FleetWorkerStartRequest,
    ) -> FleetHostResult<FleetWorkerHandle> {
        self.start_with_kind(request, FleetHostKind::LocalProcess)
    }

    fn read_status(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        let process = self
            .processes
            .get_mut(worker_id)
            .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
        if let Some(status) = process.last_exit {
            if local_worker_tree_alive(process)? {
                return Ok(FleetHostWorkerStatus {
                    worker_id: worker_id.to_string(),
                    state: FleetHostWorkerState::Draining,
                    pid: Some(process.child.id()),
                    exit_code: status.code(),
                    memory_mb: process.last_memory_mb,
                    retryable: true,
                });
            }
            return Ok(status_from_exit(
                worker_id,
                Some(process.child.id()),
                status,
                process.stopped,
                process.last_memory_mb,
            ));
        }
        match process.child.try_wait() {
            Ok(None) => {
                let pid = process.child.id();
                let due = process
                    .last_memory_sample
                    .is_none_or(|sampled| sampled.elapsed() >= MEMORY_SAMPLE_INTERVAL);
                let memory_mb = if process.host_kind == FleetHostKind::LocalProcess && due {
                    process.last_memory_sample = Some(std::time::Instant::now());
                    sample_process_memory_mb(pid)
                } else {
                    None
                };
                process.last_memory_mb = memory_mb.or(process.last_memory_mb);
                Ok(FleetHostWorkerStatus {
                    worker_id: worker_id.to_string(),
                    state: FleetHostWorkerState::Running,
                    pid: Some(pid),
                    exit_code: None,
                    // Report the retained value, not the raw sample: a
                    // transient ps failure must not flicker a live worker's
                    // memory to None (the Exited arm already does this).
                    memory_mb: process.last_memory_mb,
                    retryable: false,
                })
            }
            Ok(Some(status)) => {
                process.last_exit = Some(status);
                if local_worker_tree_alive(process)? {
                    return Ok(FleetHostWorkerStatus {
                        worker_id: worker_id.to_string(),
                        state: FleetHostWorkerState::Draining,
                        pid: Some(process.child.id()),
                        exit_code: status.code(),
                        memory_mb: process.last_memory_mb,
                        retryable: true,
                    });
                }
                Ok(status_from_exit(
                    worker_id,
                    Some(process.child.id()),
                    status,
                    process.stopped,
                    process.last_memory_mb,
                ))
            }
            Err(err) => Err(FleetHostError::retryable(format!(
                "reading worker {worker_id} status: {err}"
            ))),
        }
    }

    fn read_logs(&self, worker_id: &str, max_bytes: usize) -> FleetHostResult<String> {
        let process = self
            .processes
            .get(worker_id)
            .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
        let max_bytes = max_bytes.min(process.request.log_limit_bytes.max(1));
        read_bounded_log(&self.workspace, &process.log_path, max_bytes)
    }

    fn interrupt_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        {
            let process = self
                .processes
                .get_mut(worker_id)
                .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
            // The direct dispatcher may already be reaped while delegated
            // session/job descendants remain. Interrupt the containment
            // boundary unconditionally.
            interrupt_worker_tree(process)?;
        }
        wait_for_exit(self, worker_id, WORKER_STOP_GRACE)
    }

    fn restart_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetWorkerHandle> {
        let request = self
            .processes
            .get(worker_id)
            .map(|process| process.request.clone())
            .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
        restart_after_confirmed_stop(self, worker_id, |adapter| {
            adapter.processes.remove(worker_id);
            adapter.start_worker(request)
        })
    }

    fn stop_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        {
            let process = self
                .processes
                .get_mut(worker_id)
                .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
            process.stopped = true;
            if process.last_exit.is_none() {
                match process.child.try_wait() {
                    Ok(Some(status)) => {
                        process.last_exit = Some(status);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        return Err(FleetHostError::retryable(format!(
                            "reading worker {worker_id} status before stop: {err}"
                        )));
                    }
                }
            }
            // Always tear down the containment boundary. A dispatcher can
            // exit before a delegated TUI/tool child, so direct-child status
            // is not proof that the complete worker tree is gone.
            stop_worker_tree(process).map_err(|err| FleetHostError {
                kind: err.kind,
                message: format!("stopping worker {worker_id}: {}", err.message),
            })?;
        }
        self.read_status(worker_id)
    }

    fn cleanup_worker(&mut self, worker_id: &str) -> FleetHostResult<()> {
        if self.processes.contains_key(worker_id) {
            // Cleanup is the final containment boundary. Even when the direct
            // dispatcher already exited, delegated children may still occupy
            // its Unix session or Windows Job Object.
            let _ = self.stop_worker(worker_id)?;
        }
        self.processes.remove(worker_id);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SshFleetHostConfig {
    pub tailscale_node_id: Option<String>,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
    pub known_hosts: Option<PathBuf>,
    pub host_key_fingerprint: Option<String>,
    pub working_directory: PathBuf,
    pub env_allowlist: BTreeSet<String>,
    pub codewhale_binary: String,
    pub ssh_binary: String,
    pub connect_timeout_seconds: u64,
}

impl SshFleetHostConfig {
    pub fn new(host: impl Into<String>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            tailscale_node_id: None,
            user: None,
            port: None,
            identity: None,
            known_hosts: None,
            host_key_fingerprint: None,
            working_directory: working_directory.into(),
            env_allowlist: BTreeSet::new(),
            codewhale_binary: "codewhale".to_string(),
            ssh_binary: "ssh".to_string(),
            connect_timeout_seconds: DEFAULT_CONNECT_TIMEOUT_SECONDS,
        }
    }

    pub fn from_host_spec(spec: &FleetHostSpec) -> FleetHostResult<Self> {
        if let FleetHostSpec::Tailscale {
            node_id,
            user,
            working_directory,
            codewhale_binary,
        } = spec
        {
            validate_ssh_destination_part("node id", node_id)?;
            validate_ssh_destination_part("user", user)?;
            if node_id.is_empty() || user.is_empty() {
                return Err(FleetHostError::configuration(
                    "Tailscale requires an explicit node id and OS user",
                ));
            }
            let mut config = Self::new(node_id.clone(), working_directory.clone());
            config.tailscale_node_id = Some(node_id.clone());
            config.user = Some(user.clone());
            config.codewhale_binary = codewhale_binary.clone();
            config.validate()?;
            return Ok(config);
        }
        let FleetHostSpec::Ssh {
            host,
            port,
            user,
            identity,
            known_hosts,
            host_key_fingerprint,
            working_directory,
            env_allowlist,
            codewhale_binary,
        } = spec
        else {
            return Err(FleetHostError::configuration(
                "expected SSH Fleet host spec",
            ));
        };
        let working_directory = working_directory.clone().ok_or_else(|| {
            FleetHostError::configuration("SSH Fleet host spec requires working_directory")
        })?;
        let codewhale_binary = codewhale_binary.clone().ok_or_else(|| {
            FleetHostError::configuration("SSH Fleet host spec requires codewhale_binary")
        })?;
        let mut config = Self::new(host.clone(), working_directory);
        config.port = *port;
        config.user = user.clone();
        config.identity = identity.clone();
        config.known_hosts = known_hosts.clone();
        config.host_key_fingerprint = host_key_fingerprint.clone();
        config.env_allowlist = env_allowlist.iter().cloned().collect();
        config.codewhale_binary = codewhale_binary;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> FleetHostResult<()> {
        if self.host.trim().is_empty() {
            return Err(FleetHostError::configuration(
                "SSH Fleet host requires an explicit host",
            ));
        }
        // The destination is a single ssh argument. A leading '-' would be read
        // as an ssh option, and whitespace/control characters would change what
        // ssh or the remote shell receives.
        validate_ssh_destination_part("host", &self.host)?;
        if let Some(user) = self.user.as_ref().filter(|user| !user.trim().is_empty()) {
            validate_ssh_destination_part("user", user)?;
        }
        if self.codewhale_binary.trim().is_empty() {
            return Err(FleetHostError::configuration(
                "SSH Fleet host requires an explicit codewhale binary path",
            ));
        }
        if self.working_directory.as_os_str().is_empty() {
            return Err(FleetHostError::configuration(
                "SSH Fleet host requires an explicit working directory",
            ));
        }
        // Fingerprint-only verification is not implemented by the OpenSSH adapter.
        // Refuse a configured pin rather than silently substituting another trust source.
        if self.host_key_fingerprint.is_some() {
            return Err(FleetHostError::configuration(
                "SSH Fleet host_key_fingerprint is unsupported; configure known_hosts instead",
            ));
        }
        if self
            .known_hosts
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(FleetHostError::configuration(
                "SSH Fleet known_hosts must not be empty",
            ));
        }
        if self
            .known_hosts
            .as_ref()
            .is_some_and(|path| path.to_string_lossy().contains(['"', '\n', '\r']))
        {
            return Err(FleetHostError::configuration(
                "SSH Fleet known_hosts contains unsupported characters",
            ));
        }
        validate_env_allowlist(&self.env_allowlist)
    }

    fn target(&self) -> String {
        self.user
            .as_ref()
            .filter(|user| !user.trim().is_empty())
            .map(|user| format!("{user}@{}", self.host))
            .unwrap_or_else(|| self.host.clone())
    }
}

#[derive(Debug)]
pub struct SshFleetHostAdapter {
    config: SshFleetHostConfig,
    local: LocalProcessFleetHostAdapter,
    tailnet_host_keys: Option<tempfile::NamedTempFile>,
    tailnet_snapshot: Option<std::sync::Arc<super::tailnet::TailnetSnapshot>>,
}

impl SshFleetHostAdapter {
    pub fn new(workspace: impl AsRef<Path>, config: SshFleetHostConfig) -> FleetHostResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            local: LocalProcessFleetHostAdapter::new(workspace),
            tailnet_host_keys: None,
            tailnet_snapshot: None,
        })
    }

    pub(crate) fn set_tailnet_snapshot(
        &mut self,
        snapshot: Option<std::sync::Arc<super::tailnet::TailnetSnapshot>>,
    ) {
        self.tailnet_snapshot = snapshot;
    }

    pub fn build_ssh_command(
        &self,
        request: &FleetWorkerStartRequest,
    ) -> FleetHostResult<FleetWorkerCommand> {
        self.config.validate()?;
        let env = filtered_env(&request.env, &self.config.env_allowlist)?;
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=yes".to_string(),
            "-o".to_string(),
            format!("ConnectTimeout={}", self.config.connect_timeout_seconds),
        ];
        if self.config.tailscale_node_id.is_some() {
            if self.tailnet_host_keys.is_none() {
                return Err(FleetHostError::configuration(
                    "Tailscale node must be resolved before SSH launch",
                ));
            }
            args.extend([
                "-F".to_string(),
                if cfg!(windows) { "NUL" } else { "/dev/null" }.to_string(),
            ]);
            args.extend([
                "-o".to_string(),
                "ProxyCommand=tailscale nc %h %p".to_string(),
            ]);
            args.extend(["-o".to_string(), "CanonicalizeHostname=no".to_string()]);
            args.extend(["-o".to_string(), "UpdateHostKeys=no".to_string()]);
        }
        if let Some(known_hosts) = &self.config.known_hosts {
            args.push("-o".to_string());
            args.push(format!("UserKnownHostsFile=\"{}\"", known_hosts.display()));
            args.push("-o".to_string());
            args.push("GlobalKnownHostsFile=none".to_string());
            args.push("-o".to_string());
            // IgnoreUnknown predates OpenSSH 7.x. Older clients have no
            // KnownHostsCommand; newer ones must disable that extra trust source.
            args.push("IgnoreUnknown=KnownHostsCommand".to_string());
            args.push("-o".to_string());
            args.push("KnownHostsCommand=none".to_string());
            args.push("-o".to_string());
            args.push("VerifyHostKeyDNS=no".to_string());
        }
        for key in env.keys() {
            args.push("-o".to_string());
            args.push(format!("SendEnv={key}"));
        }
        if let Some(port) = self.config.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        if let Some(identity) = &self.config.identity {
            args.push("-i".to_string());
            args.push(identity.display().to_string());
        }
        // End option parsing so the destination can never be read as an option.
        args.push("--".to_string());
        args.push(self.config.target());
        args.push(self.remote_command(request));
        Ok(FleetWorkerCommand::new(
            self.config.ssh_binary.clone(),
            args,
        ))
    }

    fn ssh_start_request(
        &self,
        request: FleetWorkerStartRequest,
    ) -> FleetHostResult<FleetWorkerStartRequest> {
        let command = self.build_ssh_command(&request)?;
        let mut env = ssh_client_env();
        env.extend(filtered_env(&request.env, &self.config.env_allowlist)?);
        let env_allowlist = env.keys().cloned().collect();
        Ok(FleetWorkerStartRequest {
            worker_id: request.worker_id,
            attempt: request.attempt,
            command,
            cwd: None,
            env,
            env_allowlist,
            log_limit_bytes: request.log_limit_bytes,
        })
    }

    fn remote_command(&self, request: &FleetWorkerStartRequest) -> String {
        let mut parts = vec![
            "cd".to_string(),
            shell_quote(&self.config.working_directory.display().to_string()),
            "&&".to_string(),
            "exec".to_string(),
            shell_quote(&self.config.codewhale_binary),
        ];
        parts.extend(request.command.args.iter().map(|arg| shell_quote(arg)));
        parts.join(" ")
    }
}

impl FleetHostAdapter for SshFleetHostAdapter {
    fn start_worker(
        &mut self,
        request: FleetWorkerStartRequest,
    ) -> FleetHostResult<FleetWorkerHandle> {
        let mut transport_receipt = None;
        if let Some(node_id) = self.config.tailscale_node_id.as_deref() {
            let attempt = request.attempt.as_ref().ok_or_else(|| {
                FleetHostError::configuration("Tailscale worker requires a Fleet attempt context")
            })?;
            let snapshot = self.tailnet_snapshot.as_ref().ok_or_else(|| {
                FleetHostError::configuration(
                    "Tailscale requires a fresh asynchronous inventory before launch",
                )
            })?;
            let node = snapshot
                .resolve(node_id)
                .map_err(|error| FleetHostError::configuration(error.to_string()))?;
            let (address, keys) = super::tailnet::ssh_destination(node)
                .map_err(|error| FleetHostError::configuration(error.to_string()))?;
            let bytes = serde_json::to_vec_pretty(&serde_json::json!({
                "kind": "tailnet_node_identity",
                "phase": "checked_before_ssh_launch",
                "run_id": attempt.run_id,
                "task_id": attempt.task_id,
                "attempt": attempt.attempt,
                "node_id": node.id,
                "address": address,
                "user": self.config.user,
                "checked_at": chrono::Utc::now().to_rfc3339(),
                "inventory_observed_at": snapshot.observed_at,
                "host_key_source": "local_tailscale_peer_map",
                "authentication_confirmed": false,
            }))
            .map_err(|error| FleetHostError::configuration(error.to_string()))?;
            let hash = codewhale_runtime::hashing::sha256_hex(&bytes);
            transport_receipt = Some(
                super::task_spec::write_fleet_artifact_ref(
                    &self.local.workspace,
                    &attempt.run_id,
                    &attempt.task_id,
                    &request.worker_id,
                    codewhale_protocol::fleet::FleetArtifactKind::Receipt,
                    &format!("{hash}.json"),
                    &bytes,
                    Some("application/json"),
                )
                .map_err(|error| FleetHostError::configuration(error.to_string()))?,
            );
            self.config.host = address;
            self.config.known_hosts = Some(keys.path().to_path_buf());
            self.tailnet_host_keys = Some(keys);
        }
        let request = self.ssh_start_request(request)?;
        let mut handle = self.local.start_with_kind(request, FleetHostKind::Ssh)?;
        handle.transport_receipt = transport_receipt;
        Ok(handle)
    }

    fn read_status(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        self.local.read_status(worker_id)
    }

    fn read_logs(&self, worker_id: &str, max_bytes: usize) -> FleetHostResult<String> {
        self.local.read_logs(worker_id, max_bytes)
    }

    fn interrupt_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        self.local.interrupt_worker(worker_id)
    }

    fn restart_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetWorkerHandle> {
        if self.config.tailscale_node_id.is_some() {
            return Err(FleetHostError::configuration(
                "Tailscale workers require a fresh Fleet attempt and node identity check",
            ));
        }
        let request = self
            .local
            .processes
            .get(worker_id)
            .map(|process| process.request.clone())
            .ok_or_else(|| FleetHostError::terminal(format!("unknown worker {worker_id}")))?;
        restart_after_confirmed_stop(self, worker_id, |adapter| {
            adapter.local.processes.remove(worker_id);
            adapter.local.start_with_kind(request, FleetHostKind::Ssh)
        })
    }

    fn stop_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
        self.local.stop_worker(worker_id)
    }

    fn cleanup_worker(&mut self, worker_id: &str) -> FleetHostResult<()> {
        self.local.cleanup_worker(worker_id)
    }
}

/// Restart policy shared by the process-backed adapters. The previous worker
/// is released — and `replace` spawns its successor — only once a stop has
/// confirmed the whole worker tree is gone. A failed or unconfirmed stop keeps
/// the old handle so status, logs, and cleanup still reach a worker that may
/// be running, and no replacement starts beside it (duplicate execution). The
/// process-tree lifecycle in `stop_worker` stays the authority on "gone".
///
/// Known limitations:
/// - For [`SshFleetHostAdapter`], "confirmed stopped" proves only that the
///   local `ssh` client's process tree is gone. The remote `codewhale`
///   process is not observed: if the remote side does not tear the command
///   down when the connection drops, a restart can start a second remote
///   worker beside it.
/// - `start_worker` on an id the adapter still holds refuses only `Running`
///   and `Draining` (see `LocalProcessFleetHostAdapter::start_with_kind`),
///   while this restart also refuses `Unknown`. The process-backed adapters
///   never report `Unknown`, so the gap is latent; an adapter that does must
///   refuse `Unknown` in its start path too, or a start can overlap a worker
///   in an unknown state.
fn restart_after_confirmed_stop<A: FleetHostAdapter>(
    adapter: &mut A,
    worker_id: &str,
    replace: impl FnOnce(&mut A) -> FleetHostResult<FleetWorkerHandle>,
) -> FleetHostResult<FleetWorkerHandle> {
    let stopped = adapter.stop_worker(worker_id).map_err(|err| FleetHostError {
        kind: err.kind,
        message: format!(
            "restart of worker {worker_id} refused: the previous worker was not confirmed stopped ({})",
            err.message
        ),
    })?;
    if matches!(
        stopped.state,
        FleetHostWorkerState::Running
            | FleetHostWorkerState::Draining
            | FleetHostWorkerState::Unknown
    ) {
        return Err(FleetHostError::retryable(format!(
            "restart of worker {worker_id} refused: the previous worker is still {:?} after stop",
            stopped.state
        )));
    }
    replace(adapter)
}

fn open_worker_log(workspace: &Path, path: &Path) -> FleetHostResult<File> {
    // Pinned, no-follow open under the workspace: a link in `.codewhale` or at
    // the log's name is refused instead of redirecting the worker's output.
    let relative = path.strip_prefix(workspace).map_err(|_| {
        FleetHostError::retryable(format!(
            "worker log {} is outside the workspace",
            path.display()
        ))
    })?;
    let file = codewhale_util::files::WorkspaceFile::open(workspace, relative, true)
        .and_then(|target| target.open_write(false))
        .map_err(|err| FleetHostError::retryable(format!("opening worker log: {err}")))?;
    // Validated handle first, truncation second.
    file.set_len(0)
        .map_err(|err| FleetHostError::retryable(format!("truncating worker log: {err}")))?;
    Ok(file)
}

fn read_bounded_log(workspace: &Path, path: &Path, max_bytes: usize) -> FleetHostResult<String> {
    // The worker may still hold its log open for writing.
    let mut file =
        codewhale_util::fs_confined::open_read_shared(workspace, path).map_err(|err| {
            FleetHostError::retryable(format!("opening worker log {}: {err}", path.display()))
        })?;
    let len = file
        .metadata()
        .map_err(|err| FleetHostError::retryable(format!("reading worker log metadata: {err}")))?
        .len();
    let max_bytes = max_bytes.max(1) as u64;
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))
            .map_err(|err| FleetHostError::retryable(format!("seeking worker log: {err}")))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| FleetHostError::retryable(format!("reading worker log: {err}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn status_from_exit(
    worker_id: &str,
    pid: Option<u32>,
    status: ExitStatus,
    stopped: bool,
    memory_mb: Option<u64>,
) -> FleetHostWorkerStatus {
    let success = status.success();
    FleetHostWorkerStatus {
        worker_id: worker_id.to_string(),
        state: if stopped {
            FleetHostWorkerState::Stopped
        } else if success {
            FleetHostWorkerState::Exited
        } else {
            FleetHostWorkerState::Failed
        },
        pid,
        exit_code: status.code(),
        memory_mb,
        retryable: !success && !stopped,
    }
}

#[cfg(unix)]
fn sample_process_memory_mb(pid: u32) -> Option<u64> {
    // Resolve `ps` via PATH like every other external command in the
    // codebase: /bin/ps does not exist on NixOS and some minimal containers,
    // which would silently report permanent None for live workers. Restricted
    // sandboxes may also deny process-table inspection with EPERM; treat that
    // as unavailable rather than panicking or inventing a sample.
    if !process_table_inspection_available() {
        return None;
    }
    let output = match Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
    {
        Ok(output) => output,
        Err(err) if is_permission_denied(&err) => {
            mark_process_table_unavailable();
            return None;
        }
        Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    let rss_kb = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    (rss_kb > 0).then_some(rss_kb.div_ceil(1024))
}

#[cfg(not(unix))]
fn sample_process_memory_mb(_pid: u32) -> Option<u64> {
    None
}

fn classify_spawn_error(err: std::io::Error, context: String) -> FleetHostError {
    match err.kind() {
        std::io::ErrorKind::NotFound => FleetHostError::configuration(format!("{context}: {err}")),
        std::io::ErrorKind::PermissionDenied => {
            FleetHostError::terminal(format!("{context}: {err}"))
        }
        _ => FleetHostError::retryable(format!("{context}: {err}")),
    }
}

fn wait_for_exit(
    adapter: &mut LocalProcessFleetHostAdapter,
    worker_id: &str,
    timeout: Duration,
) -> FleetHostResult<FleetHostWorkerStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = adapter.read_status(worker_id)?;
        if !matches!(
            status.state,
            FleetHostWorkerState::Running | FleetHostWorkerState::Draining
        ) {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn local_worker_tree_alive(process: &LocalWorkerProcess) -> FleetHostResult<bool> {
    Ok(!unix_session_members(process.session_id, Some(process.session_id))?.is_empty())
}

#[cfg(windows)]
fn local_worker_tree_alive(process: &LocalWorkerProcess) -> FleetHostResult<bool> {
    process.windows_job.has_active_processes().map_err(|err| {
        FleetHostError::retryable(format!("querying Windows worker job activity: {err}"))
    })
}

#[cfg(not(any(unix, windows)))]
fn local_worker_tree_alive(_process: &LocalWorkerProcess) -> FleetHostResult<bool> {
    Ok(false)
}

#[cfg(unix)]
fn interrupt_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    shutdown_unix_worker_session(process, &[libc::SIGINT, libc::SIGTERM])
}

#[cfg(windows)]
fn interrupt_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    process.windows_job.terminate().map_err(|err| {
        FleetHostError::retryable(format!("interrupting Windows worker tree: {err}"))
    })
}

#[cfg(not(any(unix, windows)))]
fn interrupt_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    process
        .child
        .kill()
        .map_err(|err| FleetHostError::retryable(format!("interrupting worker: {err}")))
}

#[cfg(unix)]
fn stop_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    shutdown_unix_worker_session(process, &[libc::SIGTERM])
}

#[cfg(windows)]
fn stop_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    process.windows_job.terminate().map_err(|err| {
        FleetHostError::retryable(format!("terminating Windows worker job: {err}"))
    })?;
    if process.last_exit.is_none() {
        process.last_exit =
            Some(process.child.wait().map_err(|err| {
                FleetHostError::retryable(format!("reaping Windows worker: {err}"))
            })?);
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn stop_worker_tree(process: &mut LocalWorkerProcess) -> FleetHostResult<()> {
    process
        .child
        .kill()
        .map_err(|err| FleetHostError::retryable(format!("killing worker: {err}")))?;
    process.last_exit = Some(
        process
            .child
            .wait()
            .map_err(|err| FleetHostError::retryable(format!("reaping worker: {err}")))?,
    );
    Ok(())
}

#[cfg(unix)]
fn shutdown_unix_worker_session(
    process: &mut LocalWorkerProcess,
    graceful_signals: &[libc::c_int],
) -> FleetHostResult<()> {
    let mut signal_errors = Vec::new();
    let known_leader = process.session_id;
    for signal in graceful_signals {
        signal_errors.extend(signal_unix_session(
            process.session_id,
            *signal,
            Some(known_leader),
        )?);
        if wait_for_unix_session_exit(process, WORKER_STOP_GRACE)? {
            return Ok(());
        }
    }

    signal_errors.extend(signal_unix_session(
        process.session_id,
        libc::SIGKILL,
        Some(known_leader),
    )?);
    if wait_for_unix_session_exit(process, WORKER_STOP_GRACE)? {
        return Ok(());
    }

    // Without a process table we can only reason about the tracked session
    // leader/dispatcher. Prefer an honest degraded success once that known
    // pid is gone instead of looping forever on ps EPERM.
    if !process_table_inspection_available() {
        if process.last_exit.is_some() && !unix_pid_exists(process.session_id) {
            return Ok(());
        }
        return Err(FleetHostError::retryable(format!(
            "Fleet session {} still has a live tracked leader after SIGKILL and process-table inspection is unavailable{}",
            process.session_id,
            if signal_errors.is_empty() {
                String::new()
            } else {
                format!("; signal errors: {}", signal_errors.join("; "))
            }
        )));
    }

    let alive = unix_session_members(process.session_id, Some(known_leader))?;
    Err(FleetHostError::retryable(format!(
        "Fleet session {} still has live processes after SIGKILL: {alive:?}{}",
        process.session_id,
        if signal_errors.is_empty() {
            String::new()
        } else {
            format!("; signal errors: {}", signal_errors.join("; "))
        }
    )))
}

#[cfg(unix)]
fn wait_for_unix_session_exit(
    process: &mut LocalWorkerProcess,
    timeout: Duration,
) -> FleetHostResult<bool> {
    let deadline = Instant::now() + timeout;
    let known_leader = process.session_id;
    loop {
        if process.last_exit.is_none() {
            process.last_exit = process.child.try_wait().map_err(|err| {
                FleetHostError::retryable(format!("checking Fleet dispatcher exit: {err}"))
            })?;
        }
        if process.last_exit.is_some() {
            let members = unix_session_members(process.session_id, Some(known_leader))?;
            if members.is_empty() {
                return Ok(true);
            }
            // When process-table inspection is denied we can only track the
            // known session leader. Treat an empty known-pid set as success.
            if !process_table_inspection_available()
                && members.iter().all(|pid| !unix_pid_exists(*pid))
            {
                return Ok(true);
            }
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn unix_session_members(
    session_id: libc::pid_t,
    known_pids: Option<libc::pid_t>,
) -> FleetHostResult<Vec<libc::pid_t>> {
    match unix_process_ids() {
        Ok(pids) => {
            let mut members = Vec::new();
            for pid in pids {
                if pid > 0 {
                    // Revalidate against the kernel after parsing the snapshot. A PID
                    // reused by an unrelated process must never receive our signal.
                    // SAFETY: getsid(2) dereferences no pointers.
                    if unsafe { libc::getsid(pid) } == session_id {
                        members.push(pid);
                    }
                }
            }
            Ok(members)
        }
        Err(err) if process_table_error_is_unavailable(&err) => {
            // Restricted sandboxes may deny full process-table walks. Fall back
            // to the known session leader so stop/interrupt still reaches the
            // tracked dispatcher without inventing a process census.
            Ok(known_pids
                .into_iter()
                .filter(|pid| *pid > 0 && unix_pid_in_session(*pid, session_id))
                .collect())
        }
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn unix_pid_in_session(pid: libc::pid_t, session_id: libc::pid_t) -> bool {
    // SAFETY: getsid(2) dereferences no pointers.
    unsafe { libc::getsid(pid) == session_id }
}

#[cfg(unix)]
fn unix_pid_exists(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(2) dereferences no pointers; signal 0 sends nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn is_permission_denied(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn process_table_error_is_unavailable(err: &FleetHostError) -> bool {
    err.message.contains("process-table inspection unavailable")
        || err.message.contains("Operation not permitted")
        || err.message.contains("Permission denied")
        || err.message.contains("EPERM")
}

#[cfg(unix)]
fn mark_process_table_unavailable() {
    // Record the denial when nothing has been cached yet. OnceLock cannot flip
    // a prior true; live call sites still degrade on the immediate EPERM path.
    let _ = process_table_probe_cell().get_or_init(|| false);
}

#[cfg(unix)]
fn process_table_probe_cell() -> &'static OnceLock<bool> {
    static PROCESS_TABLE_AVAILABLE: OnceLock<bool> = OnceLock::new();
    &PROCESS_TABLE_AVAILABLE
}

/// Returns whether full process-table inspection (`ps` / `/proc`) works here.
/// Cached after the first probe so tests and production share one answer.
#[cfg(unix)]
pub(crate) fn process_table_inspection_available() -> bool {
    *process_table_probe_cell().get_or_init(|| match unix_process_ids_uncached() {
        Ok(_) => true,
        Err(err) if process_table_error_is_unavailable(&err) => false,
        // Missing `ps` binary is also unavailable inspection, not a transient
        // retryable blip for memory sampling / session census.
        Err(err)
            if err.message.contains("os error 2")
                || err.message.contains("No such file")
                || err.message.contains("not found") =>
        {
            false
        }
        Err(_) => false,
    })
}

#[cfg(all(unix, target_os = "linux"))]
fn unix_process_ids() -> FleetHostResult<Vec<libc::pid_t>> {
    unix_process_ids_uncached()
}

#[cfg(all(unix, target_os = "linux"))]
fn unix_process_ids_uncached() -> FleetHostResult<Vec<libc::pid_t>> {
    let entries = std::fs::read_dir("/proc").map_err(|err| {
        if is_permission_denied(&err) {
            FleetHostError::retryable(format!(
                "listing Fleet session through /proc: process-table inspection unavailable: {err}"
            ))
        } else {
            FleetHostError::retryable(format!("listing Fleet session through /proc: {err}"))
        }
    })?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
        .collect())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_process_ids() -> FleetHostResult<Vec<libc::pid_t>> {
    if let Some(available) = process_table_probe_cell().get()
        && !*available
    {
        return Err(FleetHostError::retryable(
            "listing Fleet session with ps: process-table inspection unavailable",
        ));
    }
    match unix_process_ids_uncached() {
        Ok(pids) => {
            let _ = process_table_probe_cell().get_or_init(|| true);
            Ok(pids)
        }
        Err(err) => {
            if process_table_error_is_unavailable(&err) {
                let _ = process_table_probe_cell().get_or_init(|| false);
            }
            Err(err)
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_process_ids_uncached() -> FleetHostResult<Vec<libc::pid_t>> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid="])
        .output()
        .map_err(|err| {
            if is_permission_denied(&err) {
                FleetHostError::retryable(format!(
                    "listing Fleet session with ps: process-table inspection unavailable: {err}"
                ))
            } else {
                FleetHostError::retryable(format!("listing Fleet session with ps: {err}"))
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let denied = stderr.contains("Operation not permitted")
            || stderr.contains("Permission denied")
            || output.status.code() == Some(1)
                && stderr.to_ascii_lowercase().contains("not permitted");
        if denied {
            return Err(FleetHostError::retryable(format!(
                "listing Fleet session with ps: process-table inspection unavailable: {stderr}"
            )));
        }
        return Err(FleetHostError::retryable(format!(
            "listing Fleet session with ps exited {:?}",
            output.status.code()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect())
}

#[cfg(unix)]
fn signal_unix_session(
    session_id: libc::pid_t,
    signal: libc::c_int,
    known_leader: Option<libc::pid_t>,
) -> FleetHostResult<Vec<String>> {
    // SAFETY: getsid(2) dereferences no pointers.
    let own_session = unsafe { libc::getsid(0) };
    if session_id <= 0 || session_id == own_session {
        return Err(FleetHostError::terminal(format!(
            "refusing to signal unsafe Fleet session {session_id}"
        )));
    }

    let mut errors = Vec::new();
    // Prefer known leader first so stop/interrupt still works when the full
    // process table cannot be enumerated under a restricted sandbox.
    let mut candidates = unix_session_members(session_id, known_leader)?;
    if candidates.is_empty()
        && let Some(leader) = known_leader.filter(|pid| *pid > 0)
    {
        candidates.push(leader);
    }
    for pid in candidates {
        // Verify identity again immediately before signalling. Session IDs
        // remain stable across reparenting and separate process groups.
        // SAFETY: getsid(2) dereferences no pointers.
        if unsafe { libc::getsid(pid) } != session_id {
            // Leader may already be gone; still try kill on known leader when
            // getsid fails only with ESRCH-equivalent absence.
            if Some(pid) != known_leader || !unix_pid_exists(pid) {
                continue;
            }
        }
        // SAFETY: kill(2) dereferences no pointers.
        if unsafe { libc::kill(pid, signal) } != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                errors.push(format!("pid {pid}: {err}"));
            }
        }
    }
    Ok(errors)
}

#[cfg(unix)]
fn unix_pid_is_running(pid: libc::pid_t) -> bool {
    if !unix_pid_exists(pid) {
        return false;
    }

    // `kill(pid, 0)` also succeeds for zombies. The Fleet containment code
    // has already finished its job once a descendant is dead; on macOS an
    // orphan can remain visible as a zombie briefly while launchd reaps it.
    // Ask `ps` for the process state so test assertions do not mistake that
    // transient kernel bookkeeping for a live leaked worker. If `ps` itself
    // is denied, stay conservative and treat the PID as running.
    if !process_table_inspection_available() {
        return true;
    }
    match Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .is_some_and(|state| !state.starts_with('Z')),
        // A failed status does not prove exit: preserve the positive kernel
        // visibility result and let the bounded waiter retry.
        Ok(_) => true,
        Err(err) if is_permission_denied(&err) => {
            let _ = process_table_probe_cell().get_or_init(|| false);
            true
        }
        Err(_) => true,
    }
}

#[cfg(all(unix, test))]
fn wait_for_unix_pid_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !unix_pid_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct FleetWindowsJob {
    handle: HANDLE,
}

#[cfg(windows)]
// SAFETY: Job handles are process-wide kernel handles. The adapter owns this
// wrapper exclusively and mutates workers through `&mut self`.
unsafe impl Send for FleetWindowsJob {}

#[cfg(windows)]
// SAFETY: The wrapper exposes only kernel job operations; shared access does
// not mutate Rust-owned memory.
unsafe impl Sync for FleetWindowsJob {}

#[cfg(windows)]
impl FleetWindowsJob {
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
            AssignProcessToJobObject(job.handle, HANDLE(child.as_raw_handle()))
                .map_err(windows_io_error)?;
        }
        Ok(job)
    }

    fn terminate(&self) -> std::io::Result<()> {
        // SAFETY: `self.handle` is a live owned job handle.
        unsafe { TerminateJobObject(self.handle, 1).map_err(windows_io_error) }
    }

    fn has_active_processes(&self) -> std::io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `accounting` is live with matching size.
        unsafe {
            QueryInformationJobObject(
                Some(self.handle),
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )
            .map_err(windows_io_error)?;
        }
        Ok(accounting.ActiveProcesses > 0)
    }
}

#[cfg(windows)]
impl Drop for FleetWindowsJob {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is owned here; Drop runs once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn attach_fleet_windows_job(mut child: Child) -> std::io::Result<(Child, FleetWindowsJob)> {
    match FleetWindowsJob::attach_to_child(&child) {
        Ok(job) => Ok((child, job)),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(err)
        }
    }
}

#[cfg(windows)]
fn windows_io_error(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error)
}

fn filtered_env(
    env: &BTreeMap<String, String>,
    allowlist: &BTreeSet<String>,
) -> FleetHostResult<BTreeMap<String, String>> {
    validate_env_allowlist(allowlist)?;
    Ok(env
        .iter()
        .filter(|(key, _)| allowlist.contains(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

/// Characters OpenSSH itself refuses in a command-line user or host name
/// because an `ssh_config` `%h`/`%r` expansion can hand them to a shell.
const SSH_DESTINATION_METACHARACTERS: &str = "'`\"$\\;&<>|(){}";

fn validate_ssh_destination_part(label: &str, value: &str) -> FleetHostResult<()> {
    let allowed = |ch: char| {
        if label == "host" {
            // Host names, IPv4/IPv6 literals (with a zone id) and ssh_config
            // aliases. `@` would move the user/host split.
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':' | '%')
        } else {
            ch.is_ascii_graphic() && !SSH_DESTINATION_METACHARACTERS.contains(ch)
        }
    };
    if value.starts_with('-') || !value.chars().all(allowed) {
        return Err(FleetHostError::configuration(format!(
            "SSH Fleet {label} must not start with '-' or contain whitespace, control, non-ASCII or shell characters"
        )));
    }
    Ok(())
}

fn validate_env_allowlist(allowlist: &BTreeSet<String>) -> FleetHostResult<()> {
    for key in allowlist {
        if !is_safe_env_key(key) {
            return Err(FleetHostError::configuration(format!(
                "Fleet host env allowlist key {key} looks secret-bearing; pass secrets through config providers, not worker argv/env"
            )));
        }
    }
    Ok(())
}

fn is_safe_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    ![
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "CREDENTIAL",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn ssh_client_env() -> BTreeMap<String, String> {
    ["HOME", "PATH", "SSH_AUTH_SOCK"]
        .into_iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| (key.to_string(), value))
        })
        .collect()
}

fn process_base_env() -> BTreeMap<String, String> {
    base_env_from(std::env::vars_os())
}

/// The worker's inherited environment: the repository's scrubbed child-env
/// allowlist (`child_env`), so proxy routing, CA bundles, temp directories,
/// Windows system/profile roots, locale and toolchain paths survive
/// `env_clear()` while provider keys and other secret-shaped names do not.
/// Proxy URLs keep their credentials for the worker process itself; see
/// [`codewhale_runtime::child_env::sanitized_runtime_env_from`] for that policy.
fn base_env_from<K, V>(parent: impl IntoIterator<Item = (K, V)>) -> BTreeMap<String, String>
where
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut env: BTreeMap<String, String> =
        codewhale_runtime::child_env::sanitized_runtime_env_from(parent)
            .into_iter()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
    force_worker_telemetry_off(&mut env);
    env
}

/// Fleet workers are an implementation detail of the parent session, not
/// sessions of their own — the parent already accounts for the dispatch.
///
/// The spawn path `env_clear()`s and rebuilds from [`process_base_env`], so an
/// operator's opt-out would otherwise never reach a worker at all. Hard-off
/// here means a worker can never emit, can never inherit an ambient "on", and
/// can never write telemetry state into the operator's home.
fn force_worker_telemetry_off(env: &mut BTreeMap<String, String>) {
    env.insert("CODEWHALE_TELEMETRY".to_string(), "false".to_string());
    env.insert("DEEPSEEK_TELEMETRY".to_string(), "false".to_string());
}

/// Build the complete environment a worker is spawned with.
///
/// The caller's allowlisted entries are merged over the process base, then
/// telemetry is forced off again: an allowlist that happens to name
/// `CODEWHALE_TELEMETRY` must not be able to switch a worker back on.
fn worker_env(
    request_env: &BTreeMap<String, String>,
    allowlist: &BTreeSet<String>,
) -> FleetHostResult<BTreeMap<String, String>> {
    let mut env = process_base_env();
    env.extend(filtered_env(request_env, allowlist)?);
    force_worker_telemetry_off(&mut env);
    Ok(env)
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn validate_worker_id(worker_id: &str) -> FleetHostResult<()> {
    if worker_id.trim().is_empty() {
        return Err(FleetHostError::configuration("worker id cannot be empty"));
    }
    Ok(())
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Worker output is opened through the pinned no-follow writer: a link in
    /// `.codewhale` or at the log's name is refused, and a plain log is
    /// truncated and readable back through the same confinement.
    #[cfg(unix)]
    #[test]
    fn worker_logs_are_never_opened_through_links() {
        use std::io::Write as _;
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let log = workspace
            .path()
            .join(".codewhale")
            .join("fleet-host")
            .join("local")
            .join("w1.log");

        let mut file = open_worker_log(workspace.path(), &log).expect("plain log opens");
        file.write_all(b"first run output").unwrap();
        drop(file);
        assert_eq!(
            read_bounded_log(workspace.path(), &log, 1024).unwrap(),
            "first run output"
        );
        drop(open_worker_log(workspace.path(), &log).expect("reopen truncates"));
        assert_eq!(read_bounded_log(workspace.path(), &log, 1024).unwrap(), "");

        // A linked directory and a linked file are both refused.
        let linked_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(linked_root.path().join(".codewhale")).unwrap();
        symlink(
            outside.path(),
            linked_root.path().join(".codewhale").join("fleet-host"),
        )
        .unwrap();
        let linked_dir_log = linked_root
            .path()
            .join(".codewhale")
            .join("fleet-host")
            .join("local")
            .join("w1.log");
        assert!(open_worker_log(linked_root.path(), &linked_dir_log).is_err());

        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "do not read").unwrap();
        let parent = log.parent().unwrap();
        symlink(&secret, parent.join("w2.log")).unwrap();
        assert!(open_worker_log(workspace.path(), &parent.join("w2.log")).is_err());
        assert!(read_bounded_log(workspace.path(), &parent.join("w2.log"), 1024).is_err());
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "do not read");
    }
    use tempfile::TempDir;

    #[cfg(unix)]
    fn skip_if_process_table_unavailable() -> bool {
        if process_table_inspection_available() {
            return false;
        }
        eprintln!("skipping: process-table inspection unavailable (ps/proc denied or missing)");
        true
    }

    #[cfg(unix)]
    #[test]
    fn sample_process_memory_reports_nonzero_for_self() {
        if skip_if_process_table_unavailable() {
            return;
        }
        // The current test process is alive, so its RSS must sample to Some(>0).
        let mb = sample_process_memory_mb(std::process::id());
        assert!(
            matches!(mb, Some(v) if v > 0),
            "expected Some(>0) MB for the live self process, got {mb:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sample_process_memory_is_none_for_dead_pid() {
        // Use a PID beyond every mainstream kernel's default pid ceiling
        // (Linux pid_max default 4M/32k, macOS ~99998, BSDs 99999): PID 0 is
        // kernel_task on macOS and semantically special to `ps -p`, so it is
        // not a portable "no such process" probe. When process-table inspection
        // is denied the sampler also returns None — same observable result.
        assert_eq!(sample_process_memory_mb(999_999_999), None);
    }

    fn shell_command(script: &str) -> FleetWorkerCommand {
        if cfg!(windows) {
            FleetWorkerCommand::new("cmd", ["/C", script])
        } else {
            FleetWorkerCommand::new("sh", ["-c", script])
        }
    }

    #[cfg(unix)]
    const DESCENDANT_HELPER_TEST: &str =
        "fleet::host::tests::fleet_host_stop_reaps_dispatcher_descendants";

    #[cfg(unix)]
    fn run_descendant_helper_if_requested() -> bool {
        let Ok(mode) = std::env::var("FLEET_DESCENDANT_HELPER") else {
            return false;
        };
        let test_binary = std::env::current_exe().expect("current test binary");
        let pid_file = std::env::var("FLEET_DESCENDANT_PID_FILE").expect("helper pid file");
        match mode.as_str() {
            "dispatcher" | "detached-dispatcher" => {
                let mut command = Command::new(&test_binary);
                command
                    .args(["--exact", DESCENDANT_HELPER_TEST, "--nocapture"])
                    .env("FLEET_DESCENDANT_HELPER", "worker")
                    .env("FLEET_DESCENDANT_PID_FILE", &pid_file);
                if mode == "detached-dispatcher" {
                    command.spawn().expect("spawn detached dispatcher child");
                    std::process::exit(0);
                }
                let status = command.status().expect("spawn dispatcher child");
                std::process::exit(status.code().unwrap_or(1));
            }
            "worker" => {
                let mut command = Command::new(&test_binary);
                command
                    .args(["--exact", DESCENDANT_HELPER_TEST, "--nocapture"])
                    .env("FLEET_DESCENDANT_HELPER", "tool")
                    .env("FLEET_DESCENDANT_PID_FILE", &pid_file);
                // Real shell tools deliberately own a separate process group.
                // This makes a root-group-only Fleet stop leak the helper.
                command.process_group(0);
                let status = command.status().expect("spawn worker tool");
                std::process::exit(status.code().unwrap_or(1));
            }
            "tool" => {
                // A shell tool can ignore graceful signals and live in its own
                // process group. Fleet's session boundary must still reap it.
                unsafe {
                    libc::signal(libc::SIGINT, libc::SIG_IGN);
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                }
                std::fs::write(&pid_file, std::process::id().to_string()).expect("write tool pid");
                thread::sleep(Duration::from_secs(30));
                true
            }
            other => panic!("unknown descendant helper mode {other}"),
        }
    }

    #[cfg(unix)]
    fn start_dispatcher_tree(
        adapter: &mut LocalProcessFleetHostAdapter,
        tmp: &TempDir,
        worker_id: &str,
        helper_mode: &str,
    ) -> (libc::pid_t, libc::pid_t) {
        let pid_file = tmp.path().join(format!("{worker_id}-tool.pid"));
        let test_binary = std::env::current_exe().expect("current test binary");
        let mut request = FleetWorkerStartRequest::new(
            worker_id,
            FleetWorkerCommand::new(
                test_binary.display().to_string(),
                ["--exact", DESCENDANT_HELPER_TEST, "--nocapture"],
            ),
        );
        request.env.insert(
            "FLEET_DESCENDANT_HELPER".to_string(),
            helper_mode.to_string(),
        );
        request.env.insert(
            "FLEET_DESCENDANT_PID_FILE".to_string(),
            pid_file.display().to_string(),
        );
        request.env_allowlist = BTreeSet::from([
            "FLEET_DESCENDANT_HELPER".to_string(),
            "FLEET_DESCENDANT_PID_FILE".to_string(),
        ]);

        let handle = adapter.start_worker(request).expect("start dispatcher");
        let root_pid = handle.pid.expect("dispatcher pid") as libc::pid_t;
        let tool_pid = wait_for_valid_pid_file(&pid_file, Duration::from_secs(5));
        if helper_mode != "detached-dispatcher" {
            assert!(unix_pid_is_running(root_pid));
        }
        assert!(unix_pid_is_running(tool_pid));
        (root_pid, tool_pid)
    }

    #[cfg(unix)]
    fn wait_for_host_state(
        adapter: &mut LocalProcessFleetHostAdapter,
        worker_id: &str,
        expected: FleetHostWorkerState,
        timeout: Duration,
    ) -> FleetHostWorkerStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let status = adapter.read_status(worker_id).expect("worker status");
            if status.state == expected || Instant::now() >= deadline {
                return status;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[cfg(unix)]
    fn wait_for_valid_pid_file(pid_file: &Path, timeout: Duration) -> libc::pid_t {
        let deadline = Instant::now() + timeout;
        let mut last_observation = "file not created".to_string();
        loop {
            match std::fs::read_to_string(pid_file) {
                Ok(contents) => {
                    let trimmed = contents.trim();
                    match trimmed.parse::<libc::pid_t>() {
                        Ok(pid) if pid > 0 => return pid,
                        _ => last_observation = format!("invalid contents {trimmed:?}"),
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => last_observation = format!("read failed: {err}"),
            }
            if Instant::now() >= deadline {
                panic!(
                    "separate-group tool never published a valid PID to {} ({last_observation})",
                    pid_file.display()
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[cfg(unix)]
    #[test]
    fn pid_file_wait_ignores_created_but_incomplete_file() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("worker.pid");
        std::fs::write(&pid_file, "pid=").unwrap();
        let expected_pid = std::process::id() as libc::pid_t;
        let writer_path = pid_file.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            std::fs::write(writer_path, expected_pid.to_string()).unwrap();
        });

        assert_eq!(
            wait_for_valid_pid_file(&pid_file, Duration::from_secs(1)),
            expected_pid
        );
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_pid_running_treats_zombie_as_exited() {
        if skip_if_process_table_unavailable() {
            return;
        }
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn short-lived child");
        let pid = child.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(2);
        let saw_zombie = loop {
            let state = match Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
            {
                Ok(output) => output,
                Err(err) if is_permission_denied(&err) => {
                    mark_process_table_unavailable();
                    child.wait().ok();
                    eprintln!("skipping: ps denied while inspecting zombie state");
                    return;
                }
                Err(err) => panic!("inspect child state: {err}"),
            };
            let is_zombie = String::from_utf8_lossy(&state.stdout)
                .split_whitespace()
                .next()
                .is_some_and(|state| state.starts_with('Z'));
            if is_zombie {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };

        let reported_running = unix_pid_is_running(pid);
        child.wait().expect("reap zombie child");
        assert!(saw_zombie, "child never became a zombie");
        assert!(!reported_running, "zombie was reported as running");
    }

    fn wait_for_log(
        adapter: &LocalProcessFleetHostAdapter,
        worker_id: &str,
        needle: &str,
    ) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let logs = adapter.read_logs(worker_id, 4096).unwrap();
            if logs.contains(needle) || Instant::now() > deadline {
                return logs;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn fleet_host_local_adapter_starts_reads_bounded_logs_and_stops() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let script = if cfg!(windows) {
            "echo 0123456789abcdef& ping -n 30 127.0.0.1 >NUL"
        } else {
            "printf 0123456789abcdef; sleep 30"
        };
        let mut request = FleetWorkerStartRequest::new("local-1", shell_command(script));
        let line_ending_bytes = if cfg!(windows) { 2 } else { 0 };
        request.log_limit_bytes = 16 + line_ending_bytes;

        let handle = adapter.start_worker(request).unwrap();
        #[cfg(unix)]
        let direct_pid = handle.pid.expect("local worker pid");
        assert_eq!(handle.host_kind, FleetHostKind::LocalProcess);
        assert!(handle.pid.is_some());
        let status = adapter.read_status("local-1").unwrap();
        assert_eq!(status.state, FleetHostWorkerState::Running);

        let logs = wait_for_log(&adapter, "local-1", "abcdef");
        let logs = logs.trim_end_matches(&['\r', '\n'][..]);
        assert!(logs.ends_with("0123456789abcdef"), "{logs:?}");
        let bounded = adapter.read_logs("local-1", 6 + line_ending_bytes).unwrap();
        let bounded = bounded.trim_end_matches(&['\r', '\n'][..]);
        assert!(bounded.ends_with("abcdef"), "{bounded:?}");

        let status = adapter.stop_worker("local-1").unwrap();
        assert_eq!(status.state, FleetHostWorkerState::Stopped);
        #[cfg(unix)]
        assert!(
            wait_for_unix_pid_exit(direct_pid as libc::pid_t, Duration::from_secs(1)),
            "stopped direct worker was not reaped"
        );
        adapter.cleanup_worker("local-1").unwrap();
        assert_eq!(
            adapter.read_status("local-1").unwrap_err().kind,
            FleetHostErrorKind::Terminal
        );
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_stop_reaps_dispatcher_descendants() {
        if run_descendant_helper_if_requested() {
            return;
        }
        // Full-session reaping of separate process-group tools requires a
        // process-table walk; without it production still signals the known
        // session leader and these assertions cannot be proven.
        if skip_if_process_table_unavailable() {
            return;
        }

        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let (root_pid, tool_pid) =
            start_dispatcher_tree(&mut adapter, &tmp, "dispatcher-tree", "dispatcher");

        let status = adapter
            .stop_worker("dispatcher-tree")
            .expect("stop complete worker tree");

        assert_eq!(status.state, FleetHostWorkerState::Stopped);
        assert!(
            wait_for_unix_pid_exit(root_pid, Duration::from_secs(1)),
            "dispatcher survived stop"
        );
        assert!(
            wait_for_unix_pid_exit(tool_pid, Duration::from_secs(1)),
            "separate-process-group tool survived stop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_interrupt_reaps_dispatcher_descendants() {
        if skip_if_process_table_unavailable() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let (root_pid, tool_pid) =
            start_dispatcher_tree(&mut adapter, &tmp, "interrupt-tree", "dispatcher");

        let status = adapter
            .interrupt_worker("interrupt-tree")
            .expect("interrupt complete worker session");

        assert_ne!(status.state, FleetHostWorkerState::Running);
        assert!(
            wait_for_unix_pid_exit(root_pid, Duration::from_secs(1)),
            "dispatcher survived interrupt"
        );
        assert!(
            wait_for_unix_pid_exit(tool_pid, Duration::from_secs(1)),
            "separate-process-group tool survived interrupt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_reports_draining_after_dispatcher_exits_with_live_descendant() {
        if skip_if_process_table_unavailable() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let (root_pid, tool_pid) = start_dispatcher_tree(
            &mut adapter,
            &tmp,
            "draining-dispatcher-tree",
            "detached-dispatcher",
        );

        assert!(
            wait_for_unix_pid_exit(root_pid, Duration::from_secs(3)),
            "dispatcher did not exit"
        );
        let status = wait_for_host_state(
            &mut adapter,
            "draining-dispatcher-tree",
            FleetHostWorkerState::Draining,
            Duration::from_secs(3),
        );
        assert_eq!(status.state, FleetHostWorkerState::Draining);
        assert!(
            unix_pid_is_running(tool_pid),
            "descendant exited before draining check"
        );

        let stopped = adapter
            .stop_worker("draining-dispatcher-tree")
            .expect("stop draining worker tree");
        assert_eq!(stopped.state, FleetHostWorkerState::Stopped);
        assert!(
            wait_for_unix_pid_exit(tool_pid, Duration::from_secs(1)),
            "draining descendant survived bounded stop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_cleanup_reaps_session_after_dispatcher_exits() {
        if skip_if_process_table_unavailable() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let (root_pid, tool_pid) = start_dispatcher_tree(
            &mut adapter,
            &tmp,
            "exited-dispatcher-tree",
            "detached-dispatcher",
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = adapter.read_status("exited-dispatcher-tree").unwrap();
            if status.state != FleetHostWorkerState::Running || Instant::now() >= deadline {
                assert_ne!(status.state, FleetHostWorkerState::Running);
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            wait_for_unix_pid_exit(root_pid, Duration::from_secs(1)),
            "dispatcher should have exited"
        );
        assert!(
            unix_pid_is_running(tool_pid),
            "delegated tool exited too early"
        );

        adapter
            .cleanup_worker("exited-dispatcher-tree")
            .expect("clean up surviving dispatcher session");

        assert!(
            wait_for_unix_pid_exit(tool_pid, Duration::from_secs(1)),
            "tool survived after its dispatcher exited"
        );
        assert_eq!(
            adapter
                .read_status("exited-dispatcher-tree")
                .unwrap_err()
                .kind,
            FleetHostErrorKind::Terminal
        );
    }

    #[test]
    fn fleet_host_local_adapter_restarts_worker_with_same_request() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let script = if cfg!(windows) {
            "echo restart-ready & ping -n 30 127.0.0.1 >NUL"
        } else {
            "printf restart-ready; sleep 30"
        };
        let request = FleetWorkerStartRequest::new("local-restart", shell_command(script));
        let first = adapter.start_worker(request).unwrap();
        let restarted = adapter.restart_worker("local-restart").unwrap();

        assert_eq!(restarted.worker_id, first.worker_id);
        assert_eq!(restarted.host_kind, FleetHostKind::LocalProcess);
        assert_ne!(restarted.pid, first.pid);
        let logs = wait_for_log(&adapter, "local-restart", "restart-ready");
        assert!(logs.contains("restart-ready"));
        adapter.stop_worker("local-restart").unwrap();
    }

    /// A host whose stop outcome is scripted: `None` fails the stop, `Some`
    /// reports that state. Only the restart path is exercised.
    struct ScriptedStopHost {
        stop_state: Option<FleetHostWorkerState>,
        owned: bool,
        spawned: usize,
    }

    impl FleetHostAdapter for ScriptedStopHost {
        fn start_worker(
            &mut self,
            request: FleetWorkerStartRequest,
        ) -> FleetHostResult<FleetWorkerHandle> {
            self.spawned += 1;
            self.owned = true;
            Ok(FleetWorkerHandle {
                transport_receipt: None,
                worker_id: request.worker_id,
                host_kind: FleetHostKind::LocalProcess,
                pid: None,
                log_path: PathBuf::new(),
            })
        }
        fn read_status(&mut self, _: &str) -> FleetHostResult<FleetHostWorkerStatus> {
            unreachable!("restart must not consult status outside stop")
        }
        fn read_logs(&self, _: &str, _: usize) -> FleetHostResult<String> {
            unreachable!()
        }
        fn interrupt_worker(&mut self, _: &str) -> FleetHostResult<FleetHostWorkerStatus> {
            unreachable!()
        }
        fn restart_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetWorkerHandle> {
            restart_after_confirmed_stop(self, worker_id, |host| {
                host.owned = false;
                host.start_worker(FleetWorkerStartRequest::new(
                    worker_id,
                    shell_command("true"),
                ))
            })
        }
        fn stop_worker(&mut self, worker_id: &str) -> FleetHostResult<FleetHostWorkerStatus> {
            let Some(state) = self.stop_state else {
                return Err(FleetHostError::retryable(
                    "Fleet session still has a live tracked leader after SIGKILL",
                ));
            };
            Ok(FleetHostWorkerStatus {
                worker_id: worker_id.to_string(),
                state,
                pid: Some(4242),
                exit_code: None,
                memory_mb: None,
                retryable: false,
            })
        }
        fn cleanup_worker(&mut self, _: &str) -> FleetHostResult<()> {
            unreachable!()
        }
    }

    #[test]
    fn fleet_host_restart_keeps_the_old_worker_when_stop_is_unconfirmed() {
        for stop_state in [None, Some(FleetHostWorkerState::Draining)] {
            let mut host = ScriptedStopHost {
                stop_state,
                owned: true,
                spawned: 0,
            };
            let err = host
                .restart_worker("overlap")
                .expect_err("an unconfirmed stop must refuse the restart");
            assert!(err.message.contains("refused"), "{}", err.message);
            assert_eq!(
                host.spawned, 0,
                "no replacement may start beside a surviving worker ({stop_state:?})"
            );
            assert!(host.owned, "the old worker's handle is kept");
        }

        let mut host = ScriptedStopHost {
            stop_state: Some(FleetHostWorkerState::Stopped),
            owned: true,
            spawned: 0,
        };
        host.restart_worker("overlap")
            .expect("a confirmed stop restarts");
        assert_eq!(host.spawned, 1);
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_local_adapter_reports_running_worker_memory_usage() {
        if skip_if_process_table_unavailable() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let request =
            FleetWorkerStartRequest::new("local-memory", shell_command("printf ready; sleep 30"));

        adapter.start_worker(request).unwrap();
        let _ = wait_for_log(&adapter, "local-memory", "ready");

        let status = adapter.read_status("local-memory").unwrap();

        assert_eq!(status.state, FleetHostWorkerState::Running);
        assert!(
            status.memory_mb.is_some_and(|memory_mb| memory_mb > 0),
            "running local worker status should include RSS memory_mb, got {status:?}"
        );

        adapter.stop_worker("local-memory").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fleet_host_stop_signals_known_leader_without_process_table() {
        // Even when full session census is unavailable, stop must still reach
        // the tracked session leader/dispatcher pid.
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let request = FleetWorkerStartRequest::new(
            "known-leader-stop",
            shell_command("printf ready; sleep 30"),
        );
        let handle = adapter.start_worker(request).unwrap();
        let pid = handle.pid.expect("pid") as libc::pid_t;
        let _ = wait_for_log(&adapter, "known-leader-stop", "ready");

        let status = adapter
            .stop_worker("known-leader-stop")
            .expect("stop known leader without process table");
        assert_eq!(status.state, FleetHostWorkerState::Stopped);
        assert!(
            wait_for_unix_pid_exit(pid, Duration::from_secs(2)),
            "known session leader survived stop without process-table census"
        );
        adapter.cleanup_worker("known-leader-stop").unwrap();
    }

    #[test]
    fn fleet_host_ssh_kind_does_not_report_local_process_memory() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = LocalProcessFleetHostAdapter::new(tmp.path());
        let script = if cfg!(windows) {
            "echo ready & ping -n 30 127.0.0.1 >NUL"
        } else {
            "printf ready; sleep 30"
        };
        let request = FleetWorkerStartRequest::new("ssh-memory", shell_command(script));

        adapter
            .start_with_kind(request, FleetHostKind::Ssh)
            .unwrap();
        let _ = wait_for_log(&adapter, "ssh-memory", "ready");

        let status = adapter.read_status("ssh-memory").unwrap();

        assert_eq!(status.state, FleetHostWorkerState::Running);
        assert_eq!(status.memory_mb, None);

        adapter.stop_worker("ssh-memory").unwrap();
    }

    #[test]
    fn fleet_host_rejects_secret_like_env_allowlist_keys() {
        let mut env = BTreeMap::new();
        env.insert("DEEPSEEK_API_KEY".to_string(), "secret".to_string());
        let allowlist = BTreeSet::from(["DEEPSEEK_API_KEY".to_string()]);

        let err = filtered_env(&env, &allowlist).unwrap_err();

        assert_eq!(err.kind, FleetHostErrorKind::Configuration);
        assert!(err.message.contains("looks secret-bearing"));
    }

    #[test]
    fn fleet_host_ssh_command_uses_sendenv_without_argv_secret_values() {
        let tmp = TempDir::new().unwrap();
        let mut config = SshFleetHostConfig::new("builder.example.test", "/srv/codewhale");
        config.user = Some("fleet".to_string());
        config.port = Some(2222);
        config.identity = Some(PathBuf::from("/tmp/fleet_id"));
        config.codewhale_binary = "/usr/local/bin/codewhale".to_string();
        config.env_allowlist = BTreeSet::from(["FLEET_PROFILE".to_string()]);
        let adapter = SshFleetHostAdapter::new(tmp.path(), config).unwrap();
        let mut request = FleetWorkerStartRequest::new(
            "ssh-1",
            FleetWorkerCommand::new("codewhale", ["fleet-worker", "noop"]),
        );
        request.env.insert(
            "FLEET_PROFILE".to_string(),
            "super-secret-profile-value".to_string(),
        );

        let command = adapter.build_ssh_command(&request).unwrap();
        let argv = command.args.join(" ");

        assert_eq!(command.program, "ssh");
        assert!(argv.contains("BatchMode=yes"));
        assert!(argv.contains("SendEnv=FLEET_PROFILE"));
        assert!(argv.contains("fleet@builder.example.test"));
        assert!(argv.contains("/usr/local/bin/codewhale"));
        assert!(argv.contains("fleet-worker"));
        assert!(!argv.contains("super-secret-profile-value"));
        // Option parsing ends right before the destination.
        let target = command
            .args
            .iter()
            .position(|arg| arg == "fleet@builder.example.test")
            .expect("destination argument");
        assert_eq!(command.args[target - 1], "--");
    }

    #[test]
    fn fleet_host_ssh_refuses_option_like_or_malformed_destination() {
        let tmp = TempDir::new().unwrap();
        for host in [
            "-oProxyCommand=true",
            "builder example.test",
            "builder\nexample.test",
            "builder\0example.test",
            "builder;true",
            "builder$(true)",
            "builder`true`",
            "fleet@builder.example.test",
            "builder\u{202e}.example.test",
        ] {
            let config = SshFleetHostConfig::new(host, "/srv/codewhale");
            let err = SshFleetHostAdapter::new(tmp.path(), config)
                .expect_err("malformed SSH host must be refused");
            assert_eq!(err.kind, FleetHostErrorKind::Configuration, "{host:?}");
            assert!(err.message.contains("SSH Fleet host"), "{}", err.message);
        }
        for user in [
            "-oProxyCommand=true",
            "fleet user",
            "fleet\tuser",
            "fleet\0user",
            "fleet;true",
            "fleet$(true)",
            "fleet|true",
            "fl\u{e9}et",
        ] {
            let mut config = SshFleetHostConfig::new("builder.example.test", "/srv/codewhale");
            config.user = Some(user.to_string());
            let err = SshFleetHostAdapter::new(tmp.path(), config)
                .expect_err("malformed SSH user must be refused");
            assert_eq!(err.kind, FleetHostErrorKind::Configuration, "{user:?}");
            assert!(err.message.contains("SSH Fleet user"), "{}", err.message);
        }
        let spec = FleetHostSpec::Ssh {
            host: "-oProxyCommand=true".to_string(),
            port: None,
            user: None,
            identity: None,
            known_hosts: None,
            host_key_fingerprint: None,
            working_directory: Some(PathBuf::from("/srv/codewhale")),
            env_allowlist: Vec::new(),
            codewhale_binary: Some("codewhale".to_string()),
        };
        assert!(SshFleetHostConfig::from_host_spec(&spec).is_err());

        // Ordinary destinations stay accepted, including a directory-style
        // user name and IPv6 literals with a zone id.
        for (user, host) in [
            (Some("fleet"), "builder.example.test"),
            (Some("alice@corp.example"), "10.0.0.7"),
            (None, "fe80::1%en0"),
            (Some("ci_bot-2"), "build_box-01"),
        ] {
            let mut config = SshFleetHostConfig::new(host, "/srv/codewhale");
            config.user = user.map(str::to_string);
            SshFleetHostAdapter::new(tmp.path(), config)
                .unwrap_or_else(|err| panic!("{user:?}@{host} must be accepted: {err:?}"));
        }
    }

    #[test]
    fn worker_base_env_uses_the_child_env_allowlist_and_keeps_proxy_route() {
        let parent = [
            ("HTTPS_PROXY", "http://fleet:pass@proxy.example.test:8080"),
            ("no_proxy", "localhost"),
            ("SSL_CERT_FILE", "/etc/ssl/corp.pem"),
            ("CURL_CA_BUNDLE", "/etc/ssl/corp.pem"),
            ("REQUESTS_CA_BUNDLE", "/etc/ssl/corp.pem"),
            ("NODE_EXTRA_CA_CERTS", "/etc/ssl/corp.pem"),
            ("PATHEXT", ".COM;.EXE;.BAT"),
            ("WINDIR", "C:\\Windows"),
            ("ProgramFiles", "C:\\Program Files"),
            ("USERPROFILE", "C:\\Users\\fleet"),
            ("TEMP", "C:\\Temp"),
            ("USER", "fleet"),
            ("TERM", "xterm-256color"),
            ("CARGO_HOME", "/home/fleet/.cargo"),
            ("CARGO_TARGET_DIR", "/tmp/target"),
            ("DEEPSEEK_API_KEY", "secret"),
            ("GITHUB_TOKEN", "secret"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("CARGO_REGISTRY_TOKEN", "secret"),
            ("DATABASE_URL", "postgres://u:secret@db/app"),
            ("CODEWHALE_TELEMETRY", "true"),
        ];
        let env = base_env_from(parent);
        for (key, value) in &parent[..15] {
            assert_eq!(
                env.get(*key).map(String::as_str),
                Some(*value),
                "{key} must reach the worker unchanged"
            );
        }
        for key in [
            "DEEPSEEK_API_KEY",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "CARGO_REGISTRY_TOKEN",
            "DATABASE_URL",
        ] {
            assert!(!env.contains_key(key), "{key} must not reach the worker");
        }
        assert_eq!(
            env.get("CODEWHALE_TELEMETRY").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn runtime_surface_hardening_ssh_requires_known_host_verification() {
        let tmp = TempDir::new().unwrap();
        let request = FleetWorkerStartRequest::new(
            "ssh-1",
            FleetWorkerCommand::new("codewhale", ["fleet-worker"]),
        );
        for known_hosts in [None, Some(PathBuf::from("/tmp/fleet keys/known_hosts"))] {
            let mut config = SshFleetHostConfig::new("builder.example.test", "/srv/codewhale");
            config.known_hosts = known_hosts.clone();
            let adapter = SshFleetHostAdapter::new(tmp.path(), config).unwrap();
            let command = adapter.build_ssh_command(&request).unwrap();
            assert!(
                command
                    .args
                    .contains(&"StrictHostKeyChecking=yes".to_string())
            );
            if let Some(path) = known_hosts {
                assert!(
                    command
                        .args
                        .contains(&format!("UserKnownHostsFile=\"{}\"", path.display()))
                );
                assert!(
                    command
                        .args
                        .contains(&"GlobalKnownHostsFile=none".to_string())
                );
                let ignore = command
                    .args
                    .iter()
                    .position(|arg| arg == "IgnoreUnknown=KnownHostsCommand")
                    .expect("OpenSSH 7.x must ignore the newer KnownHostsCommand option");
                let command_option = command
                    .args
                    .iter()
                    .position(|arg| arg == "KnownHostsCommand=none")
                    .expect("disable additional known-host sources on newer clients");
                assert!(
                    ignore < command_option,
                    "IgnoreUnknown applies only to later options"
                );
                assert!(command.args.contains(&"VerifyHostKeyDNS=no".to_string()));
            }
        }
        let mut config = SshFleetHostConfig::new("builder.example.test", "/srv/codewhale");
        config.host_key_fingerprint = Some("SHA256:configured-pin".to_string());
        let err = SshFleetHostAdapter::new(tmp.path(), config).unwrap_err();
        assert_eq!(err.kind, FleetHostErrorKind::Configuration);
        assert!(err.message.contains("configure known_hosts"));
    }

    #[test]
    fn runtime_surface_review_documented_ssh_host_loads() {
        for docs in [
            include_str!("../../../../docs/FLEET.md"),
            include_str!("../../../../docs/zh_hans/FLEET.md"),
        ] {
            // Match the actual example, not a translated section heading.
            // Windows checkouts may carry CRLF line endings.
            let docs = docs.replace("\r\n", "\n");
            let example = docs
                .split("```json\n")
                .skip(1)
                .filter_map(|block| {
                    serde_json::from_str::<serde_json::Value>(block.split("```").next()?).ok()
                })
                .find(|value| value["id"] == "builder-1")
                .expect("documented SSH worker example");
            let host: FleetHostSpec = serde_json::from_value(example["host"].clone()).unwrap();
            let config = SshFleetHostConfig::from_host_spec(&host)
                .expect("documented host must load without migration errors");
            assert!(config.known_hosts.is_some());
        }
    }

    #[test]
    fn fleet_host_ssh_config_requires_explicit_safe_fields() {
        let tmp = TempDir::new().unwrap();
        let mut config = SshFleetHostConfig::new("", "/srv/codewhale");
        config.env_allowlist = BTreeSet::from(["SAFE_FLAG".to_string()]);

        let err = SshFleetHostAdapter::new(tmp.path(), config).unwrap_err();

        assert_eq!(err.kind, FleetHostErrorKind::Configuration);
        assert!(err.message.contains("explicit host"));
    }

    #[test]
    fn fleet_host_ssh_config_maps_from_protocol_host_spec() {
        let spec = FleetHostSpec::Ssh {
            host: "builder.example.test".to_string(),
            port: Some(2222),
            user: Some("fleet".to_string()),
            identity: Some(PathBuf::from("/tmp/fleet_id")),
            known_hosts: None,
            host_key_fingerprint: None,
            working_directory: Some(PathBuf::from("/srv/codewhale")),
            env_allowlist: vec!["FLEET_PROFILE".to_string()],
            codewhale_binary: Some("/usr/local/bin/codewhale".to_string()),
        };

        let config = SshFleetHostConfig::from_host_spec(&spec).unwrap();

        assert_eq!(config.host, "builder.example.test");
        assert_eq!(config.port, Some(2222));
        assert_eq!(config.user.as_deref(), Some("fleet"));
        assert_eq!(config.working_directory, PathBuf::from("/srv/codewhale"));
        assert!(config.env_allowlist.contains("FLEET_PROFILE"));
        assert_eq!(config.codewhale_binary, "/usr/local/bin/codewhale");
    }

    #[test]
    fn worker_env_forces_telemetry_off_even_when_the_allowlist_says_otherwise() {
        // The spawn path env_clear()s and rebuilds from this map, so anything
        // absent here simply does not exist inside the worker.
        let base = process_base_env();
        assert_eq!(
            base.get("CODEWHALE_TELEMETRY").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            base.get("DEEPSEEK_TELEMETRY").map(String::as_str),
            Some("false")
        );

        // A caller that allowlists the switch cannot switch it back on.
        let request_env = BTreeMap::from([
            ("CODEWHALE_TELEMETRY".to_string(), "true".to_string()),
            ("DEEPSEEK_TELEMETRY".to_string(), "1".to_string()),
            ("FLEET_PROFILE".to_string(), "builder".to_string()),
        ]);
        let allowlist = BTreeSet::from([
            "CODEWHALE_TELEMETRY".to_string(),
            "DEEPSEEK_TELEMETRY".to_string(),
            "FLEET_PROFILE".to_string(),
        ]);

        let env = worker_env(&request_env, &allowlist).expect("worker env");
        assert_eq!(
            env.get("CODEWHALE_TELEMETRY").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            env.get("DEEPSEEK_TELEMETRY").map(String::as_str),
            Some("false")
        );
        // Unrelated allowlisted entries still come through.
        assert_eq!(
            env.get("FLEET_PROFILE").map(String::as_str),
            Some("builder")
        );
    }

    /// The env map above is only a claim about a function. This dispatches a
    /// real worker through the real spawn path and reads what the worker
    /// process actually received, because that is the environment a Codewhale
    /// worker would resolve telemetry from.
    ///
    /// Also asserts the operator's own home stays clean: a worker is an
    /// implementation detail of the parent session, and the parent already
    /// accounts for the dispatch, so a worker that wrote telemetry state into
    /// `$CODEWHALE_HOME` would double-count every fleet run.
    #[cfg(unix)]
    #[test]
    fn fleet_worker_env_carries_telemetry_off() {
        let fixture = TempDir::new().expect("fixture root");
        let workspace = fixture.path().join("workspace");
        let operator_home = fixture.path().join("operator-codewhale-home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&operator_home).expect("operator home");
        let receipt = fixture.path().join("worker-env.txt");

        let mut adapter = LocalProcessFleetHostAdapter::new(&workspace);
        let mut request = FleetWorkerStartRequest::new(
            "telemetry-env-probe",
            FleetWorkerCommand::new(
                "/bin/sh",
                ["-c".to_string(), format!("env > {}", receipt.display())],
            ),
        );
        // A caller that both sets and allowlists the switch still cannot turn
        // a worker on.
        request
            .env
            .insert("CODEWHALE_TELEMETRY".to_string(), "true".to_string());
        request.env.insert(
            "CODEWHALE_HOME".to_string(),
            operator_home.display().to_string(),
        );
        request
            .env_allowlist
            .insert("CODEWHALE_TELEMETRY".to_string());
        request.env_allowlist.insert("CODEWHALE_HOME".to_string());

        adapter.start_worker(request).expect("start worker");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut dumped = None;
        while std::time::Instant::now() < deadline {
            if let Ok(contents) = std::fs::read_to_string(&receipt)
                && contents.contains("CODEWHALE_TELEMETRY=")
                && contents.contains("DEEPSEEK_TELEMETRY=")
            {
                dumped = Some(contents);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let dumped = dumped.unwrap_or_else(|| {
            std::fs::read_to_string(&receipt).expect("worker must dump its environment")
        });

        let value = |key: &str| {
            dumped
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .map(str::to_string)
        };
        assert_eq!(
            value("CODEWHALE_TELEMETRY").as_deref(),
            Some("false"),
            "worker environment:\n{dumped}"
        );
        assert_eq!(
            value("DEEPSEEK_TELEMETRY").as_deref(),
            Some("false"),
            "worker environment:\n{dumped}"
        );
        assert!(
            !operator_home.join("telemetry").exists(),
            "a fleet worker must not write telemetry state into the operator's home"
        );
    }
}

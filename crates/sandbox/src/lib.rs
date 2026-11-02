//! Sandbox module for secure command execution.
//!
//! This module provides sandboxing capabilities for shell commands executed by
//! CodeWhale. Sandboxing restricts what system resources a command can access,
//! preventing accidental or malicious damage to the system.
//!
//! # Platform Support
//!
//! - **macOS**: Uses Seatbelt (`sandbox-exec`) when the runtime probe succeeds
//! - **Linux**: Uses bubblewrap by default whenever `/usr/bin/bwrap` is
//!   installed and a probe shows it can create its namespaces; `prefer_bwrap
//!   = false` opts out. The extension host uses bubblewrap on the same
//!   condition, probed per launch
//!   (`extension_host::supervisor::plan_launch`).
//! - **OpenHarmony**: No local Linux sandbox is advertised. Bubblewrap
//!   and Linux `prctl` hardening are gated out under `target_env =
//!   "ohos"`.
//! - **Windows**: No OS sandbox is advertised yet. The planned first helper
//!   contract is process-tree containment only via a Windows Job Object; it
//!   must not claim filesystem, network, registry, or AppContainer isolation.
//!
//! # Usage
//!
//! ```rust,ignore
//! use codewhale_sandbox::{SandboxManager, CommandSpec, SandboxPolicy};
//!
//! let manager = SandboxManager::new();
//! let spec = CommandSpec::shell("ls -la", PathBuf::from("."), Duration::from_secs(30))
//!     .with_policy(SandboxPolicy::default());
//!
//! let exec_env = manager.prepare(&spec);
//! // exec_env.command now contains the sandboxed command
//! ```

pub mod backend;
pub mod opensandbox;
pub mod policy;
pub mod process_hardening;
pub mod read_guard;
pub mod shell_dispatcher;

#[cfg(target_os = "macos")]
pub mod seatbelt;

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub mod bwrap;

#[cfg(target_os = "windows")]
pub mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub use policy::SandboxPolicy;

/// Public OS-sandbox capability labels consumed by the website facts
/// generator. Keep this list limited to wrappers that the command execution
/// path can actually select and apply.
// EXTERNAL CONTRACT — zero Rust references by design: the website's docs
// drift gate parses this const out of the source text (web/lib/facts-drift.ts
// and web/scripts/facts-lib.mjs match the literal declaration). Deleting or
// renaming it silently breaks that gate.
pub const PUBLIC_SANDBOX_BACKENDS: &[&str] = &[
    "seatbelt (macOS, when available)",
    "bubblewrap (Linux, default when installed and working)",
];

/// Specification for a command to be executed, potentially within a sandbox.
///
/// This struct captures all the information needed to execute a command:
/// the program and arguments, working directory, environment variables,
/// timeout, and sandbox policy.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The program to execute (e.g., "sh", "python", "cargo").
    pub program: String,

    /// Arguments to pass to the program.
    pub args: Vec<String>,

    /// Working directory for the command.
    pub cwd: PathBuf,

    /// Additional environment variables to set.
    pub env: HashMap<String, String>,

    /// Maximum execution time before the command is killed.
    pub timeout: Duration,

    /// Sandbox policy controlling resource access.
    pub sandbox_policy: SandboxPolicy,

    /// Optional justification for why this command needs to run.
    /// Used for logging and audit purposes.
    pub justification: Option<String>,

    /// The shell command exactly as requested, before the dispatcher adds
    /// shell-specific wrapping (encoding prefixes, exit-code capture, temp
    /// `-File` scripts). Authoritative for display; `None` for specs built
    /// directly from a program + args.
    pub requested_command: Option<String>,
}

impl CommandSpec {
    /// Create a `CommandSpec` for running a shell command via the platform shell.
    pub fn shell(command: &str, cwd: PathBuf, timeout: Duration) -> Self {
        let dispatcher = crate::shell_dispatcher::global_dispatcher();

        #[cfg(windows)]
        let (program, args) = {
            // Force UTF-8 output. cmd.exe uses chcp; PowerShell sets the
            // console output encoding directly. See issue #982. Key on the
            // PowerShell family so a custom PowerShell path keeps the same
            // output contract as the two detected variants (#6745).
            let kind = dispatcher.kind();
            let cmd = if kind.is_powershell() {
                format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}")
            } else if matches!(kind, crate::shell_dispatcher::ShellKind::Cmd) {
                format!("chcp 65001 >NUL & {command}")
            } else {
                command.to_string()
            };
            dispatcher.build_command_parts(&cmd)
        };
        #[cfg(not(windows))]
        let (program, args) = dispatcher.build_command_parts(command);

        let env = {
            #[cfg(windows)]
            {
                windows_shell_default_env()
            }
            #[cfg(not(windows))]
            {
                HashMap::new()
            }
        };

        Self {
            program,
            args,
            cwd,
            env,
            timeout,
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
            requested_command: Some(command.to_string()),
        }
    }

    /// Create a `CommandSpec` for running a program directly.
    pub fn program(program: &str, args: Vec<String>, cwd: PathBuf, timeout: Duration) -> Self {
        Self {
            program: program.to_string(),
            args,
            cwd,
            env: HashMap::new(),
            timeout,
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
            requested_command: None,
        }
    }

    /// Set the sandbox policy for this command.
    pub fn with_policy(mut self, policy: SandboxPolicy) -> Self {
        self.sandbox_policy = policy;
        self
    }

    /// Add environment variables without discarding shell defaults.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        for (key, value) in env {
            self = self.with_env_var(&key, &value);
        }
        self
    }

    /// Add a single environment variable.
    pub fn with_env_var(mut self, key: &str, value: &str) -> Self {
        #[cfg(windows)]
        self.env
            .retain(|existing, _| !existing.eq_ignore_ascii_case(key));
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    /// Set a justification for this command (for logging/audit).
    #[cfg(test)]
    pub fn with_justification(mut self, justification: &str) -> Self {
        self.justification = Some(justification.to_string());
        self
    }

    /// Get the original command as a single string (for display).
    pub fn display_command(&self) -> String {
        if let Some(requested) = &self.requested_command {
            return requested.clone();
        }
        if self.args.len() == 2
            && self.args[0] == "-c"
            && matches!(
                self.program.as_str(),
                "sh" | "bash" | "/bin/sh" | "/bin/bash" | "/usr/bin/sh" | "/usr/bin/bash"
            )
        {
            // For shell commands, show the actual command
            self.args[1].clone()
        } else if self.args.len() == 2
            && self.args[0] == "-c"
            && !self.program.eq_ignore_ascii_case("cmd")
            && !self.program.eq_ignore_ascii_case("pwsh")
            && !self.program.eq_ignore_ascii_case("pwsh.exe")
            && !self.program.eq_ignore_ascii_case("powershell")
            && !self.program.eq_ignore_ascii_case("powershell.exe")
        {
            self.args[1].clone()
        } else if self.program.eq_ignore_ascii_case("cmd")
            && self.args.len() == 2
            && self.args[0].eq_ignore_ascii_case("/C")
        {
            // Strip the `chcp 65001 >NUL & ` prefix we add on Windows for
            // UTF-8 output (issue #982).
            let raw = &self.args[1];
            raw.strip_prefix("chcp 65001 >NUL & ")
                .unwrap_or(raw)
                .to_string()
        } else if {
            let program = self.program.to_ascii_lowercase();
            program == "pwsh"
                || program == "pwsh.exe"
                || program == "powershell"
                || program == "powershell.exe"
        } && self.args.len() >= 3
            && self.args[0].eq_ignore_ascii_case("-NoProfile")
            && self.args[1].eq_ignore_ascii_case("-Command")
        {
            // Strip the PowerShell encoding prefix.
            let raw = &self.args[2];
            raw.strip_prefix("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; ")
                .unwrap_or(raw)
                .to_string()
        } else {
            // For other commands, join program and args
            let mut parts = vec![self.program.clone()];
            parts.extend(self.args.clone());
            parts.join(" ")
        }
    }
}

#[cfg(any(test, windows))]
fn windows_shell_default_env() -> HashMap<String, String> {
    HashMap::from([("PYTHONIOENCODING".to_string(), "utf-8".to_string())])
}

/// The type of sandbox being used for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxType {
    /// No sandboxing - command runs with full permissions.
    #[default]
    None,

    /// macOS Seatbelt (sandbox-exec) sandboxing.
    #[cfg(target_os = "macos")]
    MacosSeatbelt,

    /// Linux bubblewrap namespace sandboxing.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    LinuxBubblewrap,

    /// Windows process-containment helper.
    ///
    /// Not advertised until a helper enforces Job Object cleanup. This does
    /// not imply filesystem, network, registry, or AppContainer isolation.
    #[cfg(target_os = "windows")]
    Windows,
}

impl std::fmt::Display for SandboxType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxType::None => write!(f, "none"),
            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => write!(f, "macos-seatbelt"),
            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            SandboxType::LinuxBubblewrap => write!(f, "linux-bwrap"),
            #[cfg(target_os = "windows")]
            SandboxType::Windows => write!(f, "windows-sandbox"),
        }
    }
}

/// The execution environment after sandbox transformation.
///
/// This contains the actual command to run (which may include sandbox wrapper
/// commands) and all necessary environment configuration.
#[derive(Debug)]
pub struct ExecEnv {
    /// The full command to execute (may include sandbox wrapper).
    pub command: Vec<String>,

    /// Working directory for execution.
    pub cwd: PathBuf,

    /// Environment variables to set.
    pub env: HashMap<String, String>,

    /// Timeout for the command.
    pub timeout: Duration,

    /// The type of sandbox being used.
    pub sandbox_type: SandboxType,

    /// The original policy (for reference).
    pub policy: SandboxPolicy,
}

impl ExecEnv {
    /// Get the program to execute (first element of command).
    pub fn program(&self) -> &str {
        self.command
            .first()
            .map_or("sh", std::string::String::as_str)
    }

    /// Get the arguments (all elements after the first).
    pub fn args(&self) -> &[String] {
        if self.command.len() > 1 {
            &self.command[1..]
        } else {
            &[]
        }
    }

    pub fn spawn_background(
        &self,
        mut command: std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        if self.sandbox_type == SandboxType::LinuxBubblewrap {
            return bwrap::spawn(move || command.spawn())?;
        }
        command.spawn()
    }

    #[cfg(not(target_env = "ohos"))]
    pub fn spawn_pty(
        &self,
        slave: Box<dyn portable_pty::SlavePty + Send>,
        cwd: &std::path::Path,
    ) -> anyhow::Result<Box<dyn portable_pty::Child + Send + Sync>> {
        let mut command = portable_pty::CommandBuilder::new(self.program());
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        if self.sandbox_type == SandboxType::LinuxBubblewrap {
            anyhow::ensure!(
                self.program() == bwrap::BWRAP_PATH,
                "sandboxed PTY requires the configured bubblewrap wrapper"
            );
            let args = self.args();
            let separator = args
                .iter()
                .position(|arg| arg == "--")
                .filter(|index| index + 1 < args.len())
                .ok_or_else(|| anyhow::anyhow!("sandboxed PTY command is missing its target"))?;
            let setsid = ["/usr/bin/setsid", "/bin/setsid"]
                .into_iter()
                .find(|path| bwrap::is_executable(std::path::Path::new(path)))
                .ok_or_else(|| {
                    anyhow::anyhow!("sandboxed PTY requires util-linux setsid; install util-linux")
                })?;
            command.args(&args[..=separator]);
            command.args([setsid, "--ctty", "--wait", "--"]);
            command.args(&args[separator + 1..]);
            command.set_controlling_tty(false);
        } else {
            command.args(self.args());
        }
        #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
        command.args(self.args());
        command.cwd(cwd);
        codewhale_runtime::child_env::apply_to_pty_command(
            &mut command,
            codewhale_runtime::child_env::string_map_env(&self.env),
        );
        let launch = move || slave.spawn_command(command);
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        if self.sandbox_type == SandboxType::LinuxBubblewrap {
            return bwrap::spawn(launch)?;
        }
        launch()
    }

    /// Check if this execution is sandboxed.
    pub fn is_sandboxed(&self) -> bool {
        !matches!(self.sandbox_type, SandboxType::None)
    }
}

/// Detect what sandbox technology is available on the current platform.
#[cfg(test)]
pub fn get_platform_sandbox() -> Option<SandboxType> {
    get_platform_sandbox_with_bwrap_preference(false)
}

/// Detect the sandbox wrapper the configured command path can actually use.
///
/// Linux bubblewrap is on by default via `Config::prefers_bwrap` (an
/// explicit `prefer_bwrap = false` opts out) and only selected when a real
/// wrapped probe run proves it works on this host. Source-only sandbox
/// prototypes do not make commands sandboxed unless the child launch path
/// applies them.
pub fn get_platform_sandbox_with_bwrap_preference(prefer_bwrap: bool) -> Option<SandboxType> {
    #[cfg(target_os = "macos")]
    {
        if seatbelt::is_available() {
            return Some(SandboxType::MacosSeatbelt);
        }
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        if prefer_bwrap && bwrap::is_available() {
            return Some(SandboxType::LinuxBubblewrap);
        }
    }

    #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
    let _ = prefer_bwrap;

    #[cfg(target_os = "windows")]
    {
        if windows::is_available() {
            return Some(SandboxType::Windows);
        }
    }

    None
}

/// Check if sandboxing is available on this platform.
#[cfg(test)]
pub fn is_sandbox_available() -> bool {
    get_platform_sandbox().is_some()
}

pub fn bwrap_setup_failure(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("bwrap:"))
}

/// Manager for sandbox operations.
///
/// User-configured bwrap bind-mount extensions (#5410).
///
/// The default `--ro-bind / /` already exposes the host filesystem
/// read-only, so extra read-only roots are rarely needed; they exist for
/// setups where a policy or a future default narrows the root bind. Device
/// roots cover host device nodes that must stay writable (e.g. `/dev/null`
/// for redirection) — under a read-only root bind `open(O_WRONLY)` on such
/// nodes fails with `EROFS`, which is the original #5410 report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BwrapMountExtensions {
    /// Extra host paths to bind read-only inside the sandbox. Non-existent
    /// or non-directory paths are skipped silently (same rule as writable
    /// roots — a sandbox must never fail to start because config went
    /// stale).
    pub read_only_roots: Vec<PathBuf>,
    /// Host device-node paths to bind read-write (e.g. `/dev/null`).
    /// Non-existent paths are skipped; paths that exist but are not
    /// character/block devices are skipped too — this key must never become
    /// a general writable-root escape hatch.
    pub device_roots: Vec<PathBuf>,
}

impl BwrapMountExtensions {
    /// Resolve configured paths against the live filesystem, returning
    /// `(read_only_mounts, device_mounts)` as canonical paths that exist and
    /// satisfy each key's constraints. The bwrap module only exists on
    /// Linux, so the resolution inlines the same two checks its
    /// `existing_directory` performs (canonicalize + is_dir) — the type is
    /// carried on every platform because `SandboxManager` is.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn resolve(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let read_only = self
            .read_only_roots
            .iter()
            .filter_map(|path| bwrap::existing_directory_shim(path))
            .filter(|path| path != std::path::Path::new("/"))
            .collect();
        let devices = self
            .device_roots
            .iter()
            .filter_map(|path| {
                let canonical = path.canonicalize().ok()?;
                let meta = std::fs::metadata(&canonical).ok()?;
                use std::os::unix::fs::FileTypeExt;
                let file_type = meta.file_type();
                (file_type.is_char_device() || file_type.is_block_device()).then_some(canonical)
            })
            .collect();
        (read_only, devices)
    }
}

/// Expand a leading `~` or `~/` to the user's home directory. Paths without
/// the prefix (and any path when no home directory is resolvable) pass
/// through unchanged.
fn expand_home_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if text == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

/// The bubblewrap arguments that hide `path` from a sandboxed command, aimed
/// at its resolved location because bubblewrap cannot mount through a
/// symlink: an empty tmpfs over an existing directory, `/dev/null` bound over
/// an existing file, nothing for a path that does not exist (there is nothing
/// to deny). Blocking. Platform-neutral so it is exercised on every host; only
/// the Linux bwrap builder emits it.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
fn bwrap_mask_args(path: &std::path::Path) -> Vec<String> {
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return Vec::new();
    };
    let spelled = resolved.to_string_lossy().into_owned();
    if resolved.is_dir() {
        vec!["--tmpfs".to_string(), spelled]
    } else {
        vec!["--ro-bind".to_string(), "/dev/null".to_string(), spelled]
    }
}

/// Arguments appended after the read deny-list masks so `exceptions` inside
/// a masked directory stay visible. Only a denied directory that holds an
/// existing exception opens up again: each such exception is bound read-only
/// again, then each (canonical) writable root strictly inside such a
/// directory is bound writable again, then every denied path strictly inside
/// what was bound again is masked again, so an exception never re-exposes a
/// denied path. Any other denied directory stays masked over whatever lies
/// inside it, writable roots included. Empty, without touching the
/// filesystem, when there are no exceptions (every caller but the extension
/// host). Blocking (resolves paths).
///
/// This is how the extension host denies a Codewhale home whole — including
/// entries created after it started, which a per-entry mask cannot cover —
/// while its bundle, data dir and plugin code stay reachable.
///
/// Known limits: an exception or writable root that does not exist when the
/// command starts stays hidden for its lifetime; a protected descendant
/// (`.codewhale` inside a writable root) or an extra read-only root inside a
/// masked directory is not re-applied. Only the extension host sets
/// exceptions, and its data dir has neither.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
fn bwrap_exception_args(
    denied: &[PathBuf],
    exceptions: &[PathBuf],
    writable_roots: &[PathBuf],
) -> Vec<String> {
    if exceptions.is_empty() {
        return Vec::new();
    }
    let canonical = |path: &PathBuf| std::fs::canonicalize(path).ok();
    let strictly_inside = |path: &std::path::Path, roots: &[PathBuf]| {
        roots
            .iter()
            .any(|root| path != root.as_path() && path.starts_with(root))
    };
    let exceptions: Vec<PathBuf> = exceptions.iter().filter_map(canonical).collect();
    let opened: Vec<PathBuf> = denied
        .iter()
        .filter(|path| path.is_dir())
        .filter_map(canonical)
        .filter(|dir| {
            exceptions
                .iter()
                .any(|exception| strictly_inside(exception, std::slice::from_ref(dir)))
        })
        .collect();
    let mut rebound: Vec<PathBuf> = Vec::new();
    let mut args = Vec::new();
    let candidates = exceptions
        .iter()
        .map(|path| ("--ro-bind", path))
        .chain(writable_roots.iter().map(|path| ("--bind", path)));
    for (flag, path) in candidates {
        if strictly_inside(path, &opened) && !rebound.contains(path) {
            let spelled = path.to_string_lossy().into_owned();
            args.extend([flag.to_string(), spelled.clone(), spelled]);
            rebound.push(path.clone());
        }
    }
    for path in denied {
        if canonical(path).is_some_and(|resolved| strictly_inside(&resolved, &rebound)) {
            args.extend(bwrap_mask_args(path));
        }
    }
    args
}

/// The `SandboxManager` is responsible for:
/// - Detecting available sandbox technologies
/// - Transforming `CommandSpecs` into sandboxed `ExecEnvs`
/// - Detecting sandbox denials from command output
#[derive(Debug, Default)]
pub struct SandboxManager {
    /// Cached sandbox availability check.
    sandbox_available: Option<bool>,

    /// Force a specific sandbox type (for testing).
    forced_sandbox: Option<SandboxType>,

    /// When true and bwrap is executable on Linux, route commands through
    /// bubblewrap (#2184).
    prefer_bwrap: bool,

    /// User-configured bwrap bind-mount extensions (#5410): extra
    /// read-only roots and writable device nodes.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    bwrap_extensions: BwrapMountExtensions,

    /// Opt-in read deny-list (S1, #5568): paths sandboxed commands must not
    /// be able to read even though the sandbox otherwise grants full-disk
    /// read (Seatbelt appends last-match-wins deny rules; bubblewrap masks
    /// each path). Empty by default — today's behavior unchanged.
    #[cfg(any(
        test,
        target_os = "macos",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    denied_read_subpaths: Vec<PathBuf>,

    /// Paths inside a denied directory that stay visible (bubblewrap only,
    /// `bwrap_exception_args`; Seatbelt ignores this list). Empty by
    /// default.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    denied_read_exceptions: Vec<PathBuf>,
    #[cfg(any(
        target_os = "macos",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    readable_paths: Option<Vec<PathBuf>>,
}

impl SandboxManager {
    /// Create a new `SandboxManager`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new `SandboxManager` with bwrap preference (#2184).
    ///
    /// When `prefer_bwrap` is true and `/usr/bin/bwrap` is executable on Linux,
    /// exec_shell commands will be routed through bubblewrap.
    pub fn with_bwrap_preference(prefer_bwrap: bool) -> Self {
        Self {
            prefer_bwrap,
            ..Self::default()
        }
    }

    /// Set the bwrap preference (#2184).
    pub fn set_prefer_bwrap(&mut self, prefer: bool) {
        self.prefer_bwrap = prefer;
        self.sandbox_available = None;
    }

    /// Set user-configured bwrap mount extensions (#5410): extra read-only
    /// roots and writable device nodes such as `/dev/null`.
    pub fn set_bwrap_extensions(&mut self, extensions: BwrapMountExtensions) {
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            self.bwrap_extensions = extensions;
        }
        #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
        let _ = extensions;
    }

    /// Set the opt-in read deny-list (S1, #5568). A leading `~` in a path
    /// expands to the user's home directory here, and each existing path is
    /// ALSO recorded in canonicalized form when that differs: macOS Seatbelt
    /// matches the kernel-resolved path, so a rule written against
    /// `/var/...` alone never fires for the real `/private/var/...` file —
    /// the deny must name both spellings to actually deny.
    pub fn set_denied_read_subpaths(&mut self, paths: Vec<PathBuf>) {
        let mut resolved: Vec<PathBuf> = Vec::with_capacity(paths.len());
        for path in paths.into_iter().map(expand_home_prefix) {
            if let Ok(canonical) = std::fs::canonicalize(&path)
                && canonical != path
                && !resolved.contains(&canonical)
            {
                resolved.push(canonical);
            }
            if !resolved.contains(&path) {
                resolved.push(path);
            }
        }
        #[cfg(any(
            test,
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        ))]
        {
            self.denied_read_subpaths = resolved;
        }
        #[cfg(not(any(
            test,
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        )))]
        let _ = resolved;
    }

    /// Set the paths inside a denied directory that stay visible under
    /// bubblewrap (`bwrap_exception_args`). The extension host uses it to
    /// deny a Codewhale home whole while keeping its own files and plugin
    /// code readable.
    pub fn set_denied_read_exceptions(&mut self, paths: Vec<PathBuf>) {
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            self.denied_read_exceptions = paths;
        }
        #[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
        let _ = paths;
    }

    pub fn set_readable_paths(&mut self, paths: Vec<PathBuf>) {
        #[cfg(any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        ))]
        {
            self.readable_paths = Some(paths);
        }
        #[cfg(not(any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        )))]
        let _ = paths;
    }

    /// Test-only view of the resolved deny-list (post home-expansion and
    /// canonicalization).
    #[cfg(test)]
    pub fn denied_read_subpaths_for_test(&self) -> &[PathBuf] {
        &self.denied_read_subpaths
    }

    /// Check if sandboxing is available.
    pub fn is_available(&mut self) -> bool {
        if let Some(available) = self.sandbox_available {
            return available;
        }

        let available = self.configured_sandbox().is_some();
        self.sandbox_available = Some(available);
        available
    }

    /// Return the wrapper this manager is configured and able to apply.
    pub fn configured_sandbox(&self) -> Option<SandboxType> {
        get_platform_sandbox_with_bwrap_preference(self.prefer_bwrap)
    }

    /// Select the appropriate sandbox type for the given policy.
    pub fn select_sandbox(&self, policy: &SandboxPolicy) -> SandboxType {
        // If the policy doesn't want sandboxing, return None
        if !policy.should_sandbox() {
            return SandboxType::None;
        }

        // Check for forced sandbox (testing)
        if let Some(forced) = self.forced_sandbox {
            return forced;
        }

        self.configured_sandbox().unwrap_or(SandboxType::None)
    }

    /// Transform a `CommandSpec` into a sandboxed `ExecEnv`.
    ///
    /// This is the main entry point for sandboxing. It takes a command
    /// specification and returns the actual command to run, which may
    /// include sandbox wrapper commands.
    pub fn prepare(&self, spec: &CommandSpec) -> ExecEnv {
        let sandbox_type = self.select_sandbox(&spec.sandbox_policy);

        match sandbox_type {
            SandboxType::None => Self::prepare_unsandboxed(spec),

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => self.prepare_seatbelt(spec),

            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            SandboxType::LinuxBubblewrap => self.prepare_bwrap(spec),

            #[cfg(target_os = "windows")]
            SandboxType::Windows => Self::prepare_windows(spec),
        }
    }

    /// Prepare an unsandboxed execution environment.
    fn prepare_unsandboxed(spec: &CommandSpec) -> ExecEnv {
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.clone());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            timeout: spec.timeout,
            sandbox_type: SandboxType::None,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a Seatbelt-sandboxed execution environment (macOS).
    #[cfg(target_os = "macos")]
    fn prepare_seatbelt(&self, spec: &CommandSpec) -> ExecEnv {
        // Build the original command
        let mut original_command = vec![spec.program.clone()];
        original_command.extend(spec.args.clone());

        // Generate sandbox-exec arguments
        let seatbelt_args = if let Some(paths) = &self.readable_paths {
            seatbelt::create_restricted_seatbelt_args(
                original_command,
                &spec.cwd,
                paths,
                &self.denied_read_subpaths,
            )
        } else {
            seatbelt::create_seatbelt_args(
                original_command,
                &spec.sandbox_policy,
                &spec.cwd,
                &self.denied_read_subpaths,
            )
        };

        // Prepend sandbox-exec to the command
        let mut command = vec![seatbelt::SANDBOX_EXEC_PATH.to_string()];
        command.extend(seatbelt_args);

        // Add sandbox indicator to environment
        let mut env = spec.env.clone();
        env.insert("CODEWHALE_SANDBOX".to_string(), "seatbelt".to_string());
        env.insert("DEEPSEEK_SANDBOX".to_string(), "seatbelt".to_string());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::MacosSeatbelt,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a bubblewrap-sandboxed execution environment (Linux).
    ///
    /// Carries the standard container trio `--dev /dev`, `--proc /proc`,
    /// `--tmpfs /tmp` (#5410): without a private `/dev`, host device nodes
    /// inherited through the read-only root bind reject `open(O_WRONLY)`
    /// with `EROFS` — `foo >/dev/null` was the original report — and
    /// without `/proc`, toolchains that read process tables misbehave.
    /// `/tmp` is writable-but-isolated (tmpfs) so linkers and test
    /// harnesses have scratch space without widening the filesystem
    /// policy. User-configured extensions (extra read-only roots,
    /// writable device nodes) apply after the defaults.
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn prepare_bwrap(&self, spec: &CommandSpec) -> ExecEnv {
        let writable_roots = if self.readable_paths.is_some() {
            vec![policy::WritableRoot::new(spec.cwd.clone())]
        } else {
            if matches!(spec.sandbox_policy, SandboxPolicy::WorkspaceWrite { .. }) {
                let _ = std::fs::create_dir(spec.cwd.join(".codewhale"));
            }
            spec.sandbox_policy.get_writable_roots(&spec.cwd)
        };
        let command = bwrap::build_bwrap_command_with_read_paths(
            &spec.cwd,
            &spec.program,
            &spec.args,
            &writable_roots,
            spec.sandbox_policy.has_network_access(),
            &self.bwrap_extensions,
            &self.denied_read_subpaths,
            &self.denied_read_exceptions,
            self.readable_paths.as_deref(),
        );

        let mut env = spec.env.clone();
        env.insert("CODEWHALE_SANDBOX".to_string(), "bwrap".to_string());
        env.insert("DEEPSEEK_SANDBOX".to_string(), "bwrap".to_string());

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::LinuxBubblewrap,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Prepare a Windows helper execution environment.
    ///
    /// Windows support is currently not advertised by `get_platform_sandbox`.
    /// This branch only exists for forced tests and future helper wiring.
    /// The first supported helper contract is process-tree containment only;
    /// it must not be presented as filesystem or network isolation.
    #[cfg(target_os = "windows")]
    fn prepare_windows(spec: &CommandSpec) -> ExecEnv {
        let mut command = vec![spec.program.clone()];
        command.extend(spec.args.clone());

        let mut env = spec.env.clone();
        let kind = windows::select_best_kind(&spec.sandbox_policy, &spec.cwd);
        env.insert("CODEWHALE_SANDBOX".to_string(), format!("windows:{kind}"));
        env.insert("DEEPSEEK_SANDBOX".to_string(), format!("windows:{kind}"));
        if !spec.sandbox_policy.has_network_access() {
            env.insert(
                "CODEWHALE_SANDBOX_BLOCK_NETWORK".to_string(),
                "1".to_string(),
            );
            env.insert(
                "DEEPSEEK_SANDBOX_BLOCK_NETWORK".to_string(),
                "1".to_string(),
            );
        }

        ExecEnv {
            command,
            cwd: spec.cwd.clone(),
            env,
            timeout: spec.timeout,
            sandbox_type: SandboxType::Windows,
            policy: spec.sandbox_policy.clone(),
        }
    }

    /// Check if a command failure was due to sandbox denial.
    ///
    /// This helps distinguish between legitimate command failures and
    /// sandbox-blocked operations.
    pub fn was_denied(sandbox_type: SandboxType, exit_code: i32, stderr: &str) -> bool {
        #[cfg(not(any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        )))]
        let _ = (exit_code, stderr);

        match sandbox_type {
            SandboxType::None => false,

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => seatbelt::detect_denial(exit_code, stderr),

            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            SandboxType::LinuxBubblewrap => bwrap::detect_denial(exit_code, stderr),

            #[cfg(target_os = "windows")]
            SandboxType::Windows => windows::detect_denial(exit_code, stderr),
        }
    }

    /// Get a human-readable description of why a command was blocked.
    pub fn denial_message(sandbox_type: SandboxType, stderr: &str) -> String {
        #[cfg(not(any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "ohos"))
        )))]
        let _ = stderr;

        match sandbox_type {
            SandboxType::None => "Command failed (no sandbox)".to_string(),

            #[cfg(target_os = "macos")]
            SandboxType::MacosSeatbelt => {
                if stderr.contains("file-write") {
                    "Sandbox blocked write access. The command tried to write to a protected location.".to_string()
                } else if stderr.contains("network") {
                    "Sandbox blocked network access. Enable network_access in sandbox policy if needed.".to_string()
                } else {
                    format!(
                        "Sandbox blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }

            #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
            SandboxType::LinuxBubblewrap => {
                if let Some(error) = bwrap_setup_failure(stderr) {
                    format!("Bubblewrap could not create the sandbox: {}", error)
                } else if stderr.contains("Read-only file system") {
                    "Bubblewrap blocked access outside the writable workspace view.".to_string()
                } else {
                    format!(
                        "Bubblewrap blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }

            #[cfg(target_os = "windows")]
            SandboxType::Windows => {
                if stderr.contains("Access is denied") {
                    "Windows sandbox blocked access. The command lacked required privileges."
                        .to_string()
                } else if stderr.contains("network") {
                    "Windows sandbox blocked network access. Enable network_access in policy if needed."
                        .to_string()
                } else {
                    format!(
                        "Windows sandbox blocked operation: {}",
                        stderr.lines().next().unwrap_or("unknown")
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension host's deny-list form, platform-neutral so it runs on
    /// every host: an exception and a writable root inside a masked
    /// directory are bound again, a missing exception and the masked
    /// directory itself are not, and a denied path inside an exception is
    /// masked again.
    #[test]
    fn bwrap_exceptions_rebind_inside_masks_and_never_reexpose_a_denied_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join("extension-host/data")).unwrap();
        std::fs::create_dir_all(home.join("extension-host/secret")).unwrap();
        std::fs::write(home.join("token"), "s3cret").unwrap();
        std::fs::create_dir_all(root.join("other/writable")).unwrap();
        let bundle = home.join("extension-host");
        let data = bundle.join("data");
        let nested = bundle.join("secret");
        let token = home.join("token");
        let spell = |path: &std::path::Path| path.to_string_lossy().into_owned();
        let (bundle_s, data_s, nested_s, home_s, token_s) = (
            spell(&bundle),
            spell(&data),
            spell(&nested),
            spell(&home),
            spell(&token),
        );

        // `other` is denied without an exception: the writable root inside
        // it stays masked, as it is for every caller that sets none.
        let args = bwrap_exception_args(
            &[
                home.clone(),
                token.clone(),
                nested.clone(),
                root.join("other"),
            ],
            &[bundle.clone(), home.join("plugins"), home.clone()],
            &[data.clone(), root.clone(), root.join("other/writable")],
        );
        assert_eq!(
            args,
            [
                "--ro-bind",
                bundle_s.as_str(),
                bundle_s.as_str(),
                "--bind",
                data_s.as_str(),
                data_s.as_str(),
                "--tmpfs",
                nested_s.as_str(),
            ]
        );
        // No exceptions, or none inside a masked directory: nothing to add.
        assert!(
            bwrap_exception_args(
                std::slice::from_ref(&home),
                &[],
                std::slice::from_ref(&data)
            )
            .is_empty()
        );
        assert!(bwrap_exception_args(std::slice::from_ref(&token), &[bundle], &[data]).is_empty());

        assert_eq!(bwrap_mask_args(&home), ["--tmpfs", home_s.as_str()]);
        assert_eq!(
            bwrap_mask_args(&token),
            ["--ro-bind", "/dev/null", token_s.as_str()]
        );
        assert!(bwrap_mask_args(&home.join("missing")).is_empty());
    }

    #[test]
    fn test_command_spec_shell() {
        let spec = CommandSpec::shell("echo hello", PathBuf::from("/tmp"), Duration::from_secs(30));

        // Program and args depend on the detected shell.
        assert!(!spec.program.is_empty(), "program must not be empty");
        assert!(!spec.args.is_empty(), "args must not be empty");
        assert_eq!(spec.display_command(), "echo hello");
    }

    #[test]
    fn test_command_spec_shell_custom_posix_path_display() {
        let spec = CommandSpec {
            program: "/bin/zsh".to_string(),
            args: vec!["-c".to_string(), "echo hello".to_string()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            sandbox_policy: SandboxPolicy::default(),
            justification: None,
            requested_command: None,
        };

        assert_eq!(spec.display_command(), "echo hello");
    }

    #[test]
    fn test_command_spec_shell_quoted_arg_not_split() {
        // Regression for #1691: a `-m` message containing spaces must remain a
        // single, unsplit argv entry. The shell command string is passed
        // verbatim as ONE argument (`sh -c <cmd>` / `cmd /C <payload>`); we
        // must never tokenize it ourselves into `feat:` / `complete` /
        // `sub-pages"`.
        let cmd = r#"git commit -m "feat: complete sub-pages""#;
        let spec = CommandSpec::shell(cmd, PathBuf::from("/tmp"), Duration::from_secs(30));

        let dispatcher = crate::shell_dispatcher::global_dispatcher();
        assert_eq!(spec.program, dispatcher.kind().binary());
        // The quoted message survives in exactly ONE argv slot, regardless of
        // which shell-specific wrapping (encoding prefix, exit-code capture)
        // the dispatcher added around it. This single-line ASCII command never
        // takes the temp `-File` path, so the payload stays on the argv.
        let carriers: Vec<&String> = spec
            .args
            .iter()
            .filter(|arg| arg.contains(r#""feat: complete sub-pages""#))
            .collect();
        assert_eq!(carriers.len(), 1, "args: {:?}", spec.args);
        // And no argv entry is a tokenized fragment of the message.
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| arg == "feat:" || arg == "complete" || arg == "sub-pages\""),
            "args: {:?}",
            spec.args
        );
        assert_eq!(spec.display_command(), cmd);
    }

    #[test]
    fn test_command_spec_program() {
        let spec = CommandSpec::program(
            "cargo",
            vec!["build".to_string(), "--release".to_string()],
            PathBuf::from("/project"),
            Duration::from_secs(300),
        );

        assert_eq!(spec.program, "cargo");
        assert_eq!(spec.display_command(), "cargo build --release");
    }

    #[test]
    fn test_command_spec_builder() {
        let spec = CommandSpec::shell("test", PathBuf::from("."), Duration::from_secs(10))
            .with_policy(SandboxPolicy::read_only())
            .with_env_var("FOO", "bar")
            .with_justification("Testing");

        assert!(matches!(
            spec.sandbox_policy,
            SandboxPolicy::ReadOnly { .. }
        ));
        assert_eq!(spec.env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(spec.justification, Some("Testing".to_string()));

        let spec = spec
            .with_env(HashMap::new())
            .with_env(HashMap::from([("SECOND".to_string(), "added".to_string())]));
        assert_eq!(spec.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(spec.env.get("SECOND").map(String::as_str), Some("added"));
        let spec = spec.with_env(HashMap::from([("FOO".to_string(), "override".to_string())]));
        assert_eq!(spec.env.get("FOO").map(String::as_str), Some("override"));
        #[cfg(windows)]
        {
            let spec = spec.with_env(HashMap::from([(
                "foo".to_string(),
                "lowercase".to_string(),
            )]));
            assert!(!spec.env.contains_key("FOO"));
            assert_eq!(spec.env.get("foo").map(String::as_str), Some("lowercase"));
            let spec = spec.with_env_var("FOO", "uppercase");
            assert!(!spec.env.contains_key("foo"));
            assert_eq!(spec.env.get("FOO").map(String::as_str), Some("uppercase"));
        }
    }

    #[test]
    fn windows_shell_default_env_forces_python_pipe_stdio_utf8() {
        let env = windows_shell_default_env();

        assert_eq!(
            env.get("PYTHONIOENCODING").map(String::as_str),
            Some("utf-8")
        );
    }

    #[test]
    fn test_sandbox_manager_new() {
        let manager = SandboxManager::new();
        assert!(manager.sandbox_available.is_none());
    }

    #[test]
    fn test_sandbox_manager_select_sandbox() {
        let manager = SandboxManager::new();

        // DangerFullAccess should never sandbox
        let no_sandbox = manager.select_sandbox(&SandboxPolicy::DangerFullAccess);
        assert_eq!(no_sandbox, SandboxType::None);

        // ExternalSandbox should never sandbox
        let external = manager.select_sandbox(&SandboxPolicy::ExternalSandbox {
            network_access: true,
        });
        assert_eq!(external, SandboxType::None);
    }

    #[test]
    fn test_prepare_unsandboxed() {
        let manager = SandboxManager::new();
        let spec = CommandSpec::shell("echo test", PathBuf::from("/tmp"), Duration::from_secs(30))
            .with_policy(SandboxPolicy::DangerFullAccess);

        let env = manager.prepare(&spec);

        assert_eq!(env.sandbox_type, SandboxType::None);
        // Unsandboxed preparation passes the spec through untouched: the
        // command is exactly the spec's program followed by the dispatcher-
        // built args, whatever wrapping the current shell required.
        let mut expected = vec![spec.program.clone()];
        expected.extend(spec.args.iter().cloned());
        assert_eq!(env.command, expected);
        assert!(!env.is_sandboxed());
    }

    #[test]
    fn test_exec_env_helpers() {
        let env = ExecEnv {
            command: vec![
                "sandbox-exec".to_string(),
                "-p".to_string(),
                "policy".to_string(),
                "--".to_string(),
                "echo".to_string(),
                "hello".to_string(),
            ],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            sandbox_type: SandboxType::None,
            policy: SandboxPolicy::default(),
        };

        assert_eq!(env.program(), "sandbox-exec");
        assert_eq!(env.args().len(), 5);
    }

    #[test]
    fn test_sandbox_type_display() {
        assert_eq!(format!("{}", SandboxType::None), "none");

        #[cfg(target_os = "macos")]
        assert_eq!(format!("{}", SandboxType::MacosSeatbelt), "macos-seatbelt");

        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        assert_eq!(format!("{}", SandboxType::LinuxBubblewrap), "linux-bwrap");
    }

    // ── Parity tests (#2187) ──────────────────────────────────────────────

    #[test]
    fn test_parity_platform_sandbox_detection() {
        let sandbox_type = get_platform_sandbox();
        let available = is_sandbox_available();
        if available {
            assert!(sandbox_type.is_some());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parity_macos_seatbelt_available() {
        // Match real runtime availability (`seatbelt::is_available` via
        // `get_platform_sandbox`), not merely the presence of sandbox-exec or a
        // diagnostics layer that may report seatbelt at another boundary.
        // On hosts where sandbox-exec exists but is denied (e.g. some CI /
        // restricted macOS environments), skip rather than asserting a false
        // positive.
        match get_platform_sandbox() {
            Some(SandboxType::MacosSeatbelt) => {}
            None => {
                eprintln!("skipping: MacosSeatbelt unavailable via get_platform_sandbox()");
            }
            Some(other) => panic!("unexpected macOS sandbox type: {other:?}"),
        }
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn linux_default_never_claims_an_unwired_sandbox() {
        assert_eq!(get_platform_sandbox(), None);
        assert_eq!(get_platform_sandbox_with_bwrap_preference(false), None);
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn linux_bwrap_selection_requires_opt_in_and_executable() {
        let expected = bwrap::is_available().then_some(SandboxType::LinuxBubblewrap);
        assert_eq!(get_platform_sandbox_with_bwrap_preference(true), expected);

        let manager = SandboxManager::with_bwrap_preference(true);
        let selected = manager.select_sandbox(&SandboxPolicy::default());
        assert_eq!(selected, expected.unwrap_or(SandboxType::None));
    }

    #[test]
    fn test_parity_denial_zero_exit_never_denied() {
        assert!(!SandboxManager::was_denied(
            SandboxType::None,
            0,
            "anything"
        ));
        #[cfg(target_os = "macos")]
        assert!(!SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            0,
            ""
        ));
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        assert!(!SandboxManager::was_denied(
            SandboxType::LinuxBubblewrap,
            0,
            ""
        ));
        #[cfg(target_os = "windows")]
        assert!(!SandboxManager::was_denied(SandboxType::Windows, 0, ""));
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn bwrap_denial_is_not_inferred_from_seccomp_text() {
        assert!(!SandboxManager::was_denied(
            SandboxType::LinuxBubblewrap,
            1,
            "Bad system call"
        ));
        assert!(SandboxManager::was_denied(
            SandboxType::LinuxBubblewrap,
            1,
            "Read-only file system"
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parity_seatbelt_file_write_detected() {
        // Seatbelt patterns use "Sandbox: <cmd> denied <operation>" format.
        assert!(SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            1,
            "Sandbox: ls denied file-write*"
        ));
        assert!(SandboxManager::was_denied(
            SandboxType::MacosSeatbelt,
            1,
            "Operation not permitted"
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sandbox_child_env_exports_codewhale_marker_and_legacy_alias() {
        let manager = SandboxManager {
            forced_sandbox: Some(SandboxType::MacosSeatbelt),
            ..SandboxManager::default()
        };
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5));
        let env = manager.prepare(&spec);

        assert_eq!(
            env.env.get("CODEWHALE_SANDBOX").map(String::as_str),
            Some("seatbelt")
        );
        assert_eq!(
            env.env.get("DEEPSEEK_SANDBOX").map(String::as_str),
            Some("seatbelt")
        );
    }

    #[test]
    fn test_parity_manager_default_no_bwrap() {
        let manager = SandboxManager::default();
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
            .with_policy(SandboxPolicy::default());
        let env = manager.prepare(&spec);
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            let primary_marker = env.env.get("CODEWHALE_SANDBOX");
            let marker = env.env.get("DEEPSEEK_SANDBOX");
            assert!(primary_marker.is_none());
            assert!(marker.is_none());
            assert_eq!(env.sandbox_type, SandboxType::None);
        }
        let _ = env;
    }

    #[test]
    fn test_parity_manager_with_bwrap() {
        let manager = SandboxManager::with_bwrap_preference(true);
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
            .with_policy(SandboxPolicy::default());
        let env = manager.prepare(&spec);
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            if crate::bwrap::is_available() {
                let primary_marker = env.env.get("CODEWHALE_SANDBOX");
                let marker = env.env.get("DEEPSEEK_SANDBOX");
                assert_eq!(primary_marker.map(String::as_str), Some("bwrap"));
                assert_eq!(marker.map(String::as_str), Some("bwrap"));
                assert_eq!(env.sandbox_type, SandboxType::LinuxBubblewrap);
                assert_eq!(env.program(), bwrap::BWRAP_PATH);
            } else {
                assert_eq!(env.sandbox_type, SandboxType::None);
                assert!(!env.env.contains_key("CODEWHALE_SANDBOX"));
                assert!(!env.env.contains_key("DEEPSEEK_SANDBOX"));
            }
        }
        let _ = env;
    }

    /// Real Linux enforcement proof, not a marker check: the same
    /// outside-workspace write succeeds unsandboxed and fails under
    /// bubblewrap, and a normal workspace write succeeds inside the wrapper.
    /// Skips on hosts where the functional probe says bwrap cannot run.
    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn bwrap_workspace_write_blocks_outside_write_allows_inside() {
        if !bwrap::is_available() {
            return;
        }
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let outside = tempfile::Builder::new()
            .prefix("cw_bwrap_outside")
            .tempdir_in(&home)
            .expect("outside-workspace tempdir under HOME");
        let outside_target = outside.path().join("must_not_persist");
        let inside_target = workspace.path().join("allowed_write");

        let run = |manager: &SandboxManager, command: &str| -> (SandboxType, bool) {
            let spec = CommandSpec::shell(
                command,
                workspace.path().to_path_buf(),
                Duration::from_secs(15),
            )
            // CI's hermetic HOME lives under /tmp. Exclude temporary roots so
            // the outside fixture is outside this test's writable policy too.
            .with_policy(SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![],
                network_access: false,
                exclude_tmpdir: true,
                exclude_slash_tmp: true,
            });
            let env = manager.prepare(&spec);
            let (program, args) = env.command.split_first().unwrap();
            let status = std::process::Command::new(program)
                .args(args)
                .current_dir(&env.cwd)
                .envs(&env.env)
                .status()
                .expect("spawn prepared command");
            (env.sandbox_type, status.success())
        };

        // Control: without the wrapper the outside write lands on the host —
        // this proves the command itself is permitted and only the sandbox
        // confines it.
        let unwrapped = SandboxManager::default();
        let (kind, ok) = run(
            &unwrapped,
            &format!("echo x > {}", outside_target.display()),
        );
        assert_eq!(kind, SandboxType::None);
        assert!(ok);
        assert!(outside_target.exists());
        std::fs::remove_file(&outside_target).unwrap();

        let wrapped = SandboxManager::with_bwrap_preference(true);
        let (kind, ok) = run(&wrapped, &format!("echo x > {}", outside_target.display()));
        assert_eq!(kind, SandboxType::LinuxBubblewrap);
        assert!(!ok);
        assert!(!outside_target.exists());

        let (kind, ok) = run(&wrapped, &format!("echo ok > {}", inside_target.display()));
        assert_eq!(kind, SandboxType::LinuxBubblewrap);
        assert!(ok);
        assert_eq!(
            std::fs::read_to_string(&inside_target).unwrap().trim(),
            "ok"
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn bwrap_read_only_policy_keeps_the_working_directory_read_only() {
        let manager = SandboxManager {
            forced_sandbox: Some(SandboxType::LinuxBubblewrap),
            ..SandboxManager::default()
        };
        let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
            .with_policy(SandboxPolicy::read_only());
        let env = manager.prepare(&spec);

        assert_eq!(env.sandbox_type, SandboxType::LinuxBubblewrap);
        assert!(!env.command.iter().any(|arg| arg == "--bind"));
        assert!(!env.command.iter().any(|arg| arg == "--share-net"));
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn bwrap_workspace_policy_maps_additional_roots_and_network_access() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&extra).expect("extra");

        let manager = SandboxManager {
            forced_sandbox: Some(SandboxType::LinuxBubblewrap),
            ..SandboxManager::default()
        };
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![extra.clone()],
            network_access: true,
            exclude_tmpdir: true,
            exclude_slash_tmp: true,
        };
        let spec = CommandSpec::shell("true", workspace.clone(), Duration::from_secs(5))
            .with_policy(policy);
        let env = manager.prepare(&spec);

        for root in [workspace, extra] {
            let root = root
                .canonicalize()
                .expect("writable root")
                .to_string_lossy()
                .into_owned();
            assert!(env.command.windows(3).any(|args| args[0] == "--bind"
                && args[1].as_str() == root.as_str()
                && args[2].as_str() == root.as_str()));
        }
        assert!(env.command.iter().any(|arg| arg == "--share-net"));
    }

    #[test]
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    fn full_access_and_external_policies_bypass_forced_bwrap() {
        let manager = SandboxManager {
            forced_sandbox: Some(SandboxType::LinuxBubblewrap),
            ..SandboxManager::default()
        };

        for policy in [
            SandboxPolicy::DangerFullAccess,
            SandboxPolicy::ExternalSandbox {
                network_access: false,
            },
        ] {
            let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
                .with_policy(policy);
            let env = manager.prepare(&spec);
            assert_eq!(env.sandbox_type, SandboxType::None);
            assert_ne!(env.program(), bwrap::BWRAP_PATH);
        }
    }

    #[test]
    fn test_parity_exec_env_for_all_policies() {
        let manager = SandboxManager::new();
        let policies = [
            SandboxPolicy::DangerFullAccess,
            SandboxPolicy::read_only(),
            SandboxPolicy::workspace_with_network(),
            SandboxPolicy::default(),
        ];
        for policy in &policies {
            let spec = CommandSpec::shell("true", PathBuf::from("/tmp"), Duration::from_secs(5))
                .with_policy(policy.clone());
            let env = manager.prepare(&spec);
            assert_eq!(env.policy, *policy);
        }
    }
}

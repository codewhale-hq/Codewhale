//! External-binary dependency resolution for tools that shell out to
//! locally-installed programs (Python for `code_execution` / RLM REPL,
//! `pdftotext` for PDF reading in `read_file`, future tools as added).
//!
//! Before v0.8.31, tools that called external binaries hardcoded the
//! command name and failed at execution time when the binary wasn't on
//! `PATH`. The most-cited example was `code_execution`, which spawned
//! `python3` directly — Windows users (where the launcher is `py` or
//! `python`, not `python3`) saw `Failed to execute tool: program not
//! found` with no upstream hint of what was wrong.
//!
//! This module centralises the probe-then-decide pattern. The supported
//! callers today are:
//!
//! - Tool catalog construction (`core::engine::tool_catalog`): for
//!   tools that should be advertised to the model only when the
//!   required runtime is present.
//! - Doctor command (`run_doctor` in `main.rs`): for surfacing the
//!   resolved state to the user so missing dependencies aren't an
//!   invisible failure.
//! - Long-lived REPL runtime (`repl::runtime`): for RLM and inline `repl`
//!   blocks that need to spawn Python on every supported platform.
//!
//! Results are cached for the process lifetime via [`std::sync::OnceLock`]
//! — probing a binary involves a `Command::output` per candidate and
//! we'd rather not pay that on every model turn.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::Duration;
use wait_timeout::ChildExt;

/// Candidate executable names for the Python interpreter, in the
/// order we try them. On Windows the launcher convention is `py -3`,
/// so we add it as a third option; the resolver splits on whitespace
/// at execution time so `py -3 /tmp/code.py` runs correctly.
///
/// Order matters: `python3` first because it's the unambiguous v3
/// binary on Unix and rules out Python 2 leftovers. `python` second
/// covers Windows installations that drop the version suffix and
/// modern macOS where Homebrew installs both. `py -3` last as a
/// Windows-launcher fallback.
pub const PYTHON_CANDIDATES: &[&str] = &["python3", "python", "py -3"];

/// Probe a single executable. Returns `true` when the candidate
/// responds to `--version` with a successful exit. Splits on
/// whitespace so `"py -3"` works as a candidate.
///
/// We deliberately use `--version` rather than `which` so the probe
/// is portable across Unix, Windows (no `which` by default), and
/// containers. The downside is that we spawn a subprocess per
/// candidate; the resolver caches the result so this only fires
/// once per process.
#[must_use]
pub fn probe_executable(spec: &str) -> bool {
    probe_executable_with_flag(spec, "--version")
}

/// Probe a single executable using an explicit version/help flag.
///
/// Most tools report their presence via `--version`, but some do not:
/// Poppler's `pdftotext` treats `--version` as an input *filename* and
/// exits non-zero ("I/O Error: Couldn't open file '--version'"), so the
/// default probe reports it missing even when it is installed (#1667).
/// Such tools pass their own flag (e.g. `-v`) here.
#[must_use]
pub fn probe_executable_with_flag(spec: &str, version_flag: &str) -> bool {
    let mut parts = spec.split_whitespace();
    let Some(program) = parts.next() else {
        return false;
    };
    let mut cmd = version_probe_command(program);
    cmd.args(parts).arg(version_flag);
    probe_found(&mut cmd)
}

/// A probe that launched and was still running at its deadline found an
/// installed tool that is slow under load; only a failed launch or a failing
/// exit means the tool is absent.
fn probe_found(command: &mut Command) -> bool {
    match probe_output(command, false, VERSION_PROBE_TIMEOUT) {
        Ok(output) => output.status.success(),
        Err(error) => error.kind() == std::io::ErrorKind::TimedOut,
    }
}

/// Probe a single executable and capture its version banner in one spawn.
///
/// Same contract as [`probe_executable`] (success = exit 0), but returns the
/// trimmed stdout so callers that want the banner don't need a second process
/// launch. Returns `None` when the probe fails or stdout is not valid UTF-8.
pub fn probe_executable_capturing(spec: &str, version_flag: &str) -> Option<String> {
    let mut parts = spec.split_whitespace();
    let program = parts.next()?;
    let mut cmd = version_probe_command(program);
    cmd.args(parts).arg(version_flag);
    let output = probe_output(&mut cmd, true, VERSION_PROBE_TIMEOUT).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const VERSION_PROBE_MAX_OUTPUT: u64 = 16 * 1024;

fn version_probe_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    codewhale_util::utils::suppress_console_window(&mut cmd);
    codewhale_runtime::child_env::apply_to_command(&mut cmd, std::iter::empty::<(&str, &str)>());
    // A presence probe needs no bootstrap code or import paths from the
    // parent. The general child allowlist retains these for normal SDK tools.
    for key in ["NODE_OPTIONS", "NODE_PATH", "PYTHONPATH", "RUSTC_WRAPPER"] {
        cmd.env_remove(key);
    }
    cmd
}

/// Version/help probes never inherit stdin, retain unbounded banners, or leave
/// a normal descendant alive after their deadline. No runtime is constructed.
fn probe_output(
    command: &mut Command,
    capture: bool,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    probe_output_bounded(command, capture, timeout, VERSION_PROBE_MAX_OUTPUT)
}

pub fn probe_output_bounded(
    command: &mut Command,
    capture: bool,
    timeout: std::time::Duration,
    max_output: u64,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let (mut child, tree) = codewhale_runtime::process_tree::spawn_contained_std(command)?;
    let mut tree = Some(tree);
    let result = (|| {
        let reader = if capture {
            let pipe = child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("version probe stdout missing"))?;
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("version-probe-output".into())
                .spawn(move || {
                    let mut bytes = Vec::new();
                    let result = pipe
                        .take(max_output.saturating_add(1))
                        .read_to_end(&mut bytes)
                        .and_then(|_| {
                            if bytes.len() as u64 > max_output {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "version probe output exceeded limit",
                                ))
                            } else {
                                Ok(bytes)
                            }
                        });
                    let _ = tx.send(result);
                })?;
            Some(rx)
        } else {
            None
        };
        let status = child.wait_timeout(timeout)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "version probe did not finish before its deadline",
            )
        })?;
        // A successful parent may leave an inherited pipe open in a child.
        drop(tree.take());
        let stdout = match reader {
            Some(reader) => reader
                .recv_timeout(std::time::Duration::from_millis(250))
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "version probe pipe did not close",
                    )
                })??,
            None => Vec::new(),
        };
        Ok(std::process::Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    })();
    if result.is_err() {
        drop(tree.take());
        let _ = child.kill();
        let _ = child.wait_timeout(std::time::Duration::from_millis(250));
    }
    result
}

fn executable_path_candidates(program: &str) -> Vec<PathBuf> {
    let program_path = Path::new(program);
    if program_path.components().count() > 1 {
        return vec![program_path.to_path_buf()];
    }

    let Some(path) = std::env::var_os("PATH") else {
        return vec![PathBuf::from(program)];
    };

    let mut candidates = Vec::new();
    for dir in std::env::split_paths(&path) {
        let bare = dir.join(program);
        candidates.push(bare.clone());

        #[cfg(windows)]
        if Path::new(program).extension().is_none() {
            let pathext =
                std::env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
            for ext in pathext.to_string_lossy().split(';') {
                if ext.is_empty() {
                    continue;
                }
                candidates.push(bare.with_extension(ext.trim_start_matches('.')));
            }
        }
    }

    candidates
}

fn resolve_executable_path(spec: &str, version_flag: &str) -> Option<String> {
    let mut parts = spec.split_whitespace();
    let program = parts.next()?;
    let args: Vec<&str> = parts.collect();

    for candidate in executable_path_candidates(program) {
        if !candidate.is_file() {
            continue;
        }

        let mut cmd = version_probe_command(&candidate);
        cmd.args(&args).arg(version_flag);

        if probe_found(&mut cmd) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    None
}

/// Resolve the Python interpreter once per process. Returns the
/// candidate spec (e.g. `"python3"` or `"py -3"`) that succeeded,
/// or `None` when every candidate failed.
///
/// Callers that need to spawn the interpreter should split this
/// string on whitespace — see [`split_interpreter_spec`].
pub fn resolve_python_interpreter() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            for candidate in PYTHON_CANDIDATES {
                if probe_executable(candidate) {
                    tracing::info!(
                        target: "tool_dependencies",
                        candidate = candidate,
                        "Resolved Python interpreter",
                    );
                    return Some((*candidate).to_string());
                }
            }
            tracing::warn!(
                target: "tool_dependencies",
                tried = ?PYTHON_CANDIDATES,
                "No Python interpreter found",
            );
            None
        })
        .clone()
}

/// Resolve `pdftotext` (from Poppler) once per process. Used by
/// file and web PDF paths for truthful availability diagnostics. Unlike
/// the Python case, `read_file` itself still works for text files
/// when `pdftotext` is missing — this resolver exists so the doctor
/// command can surface the miss before a PDF read returns its typed
/// `binary_unavailable` result.
pub fn resolve_pdftotext() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            // Poppler's `pdftotext` rejects `--version` (it is parsed as an
            // input filename and exits non-zero), so probe with `-v`, which
            // prints the version banner and exits 0 (#1667).
            if probe_executable_with_flag("pdftotext", "-v") {
                Some("pdftotext".to_string())
            } else {
                None
            }
        })
        .clone()
}

/// Resolve `tesseract` (OCR engine) once per process. Used by the
/// `image_ocr` tool on platforms that do not have a native OCR backend.
/// Tesseract is the de-facto open-source OCR engine and ships as a single
/// binary on every platform we support, so the candidate list is just
/// `tesseract`.
pub fn resolve_tesseract() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            if probe_executable("tesseract") {
                tracing::info!(
                    target: "tool_dependencies",
                    "Resolved tesseract binary for image_ocr",
                );
                Some("tesseract".to_string())
            } else {
                tracing::warn!(
                    target: "tool_dependencies",
                    "tesseract binary not found; image_ocr will rely on native OCR if available",
                );
                None
            }
        })
        .clone()
}

/// Resolve `pandoc` (universal document converter) once per
/// process. Used by the `pandoc_convert` tool to decide whether
/// to register itself with the model. Pandoc is a single-binary
/// install, so the candidate list is just `pandoc` — no platform
/// fallback path.
pub fn resolve_pandoc() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            if let Some(path) = resolve_executable_path("pandoc", "--version") {
                tracing::info!(
                    target: "tool_dependencies",
                    "Resolved pandoc binary for pandoc_convert",
                );
                Some(path)
            } else {
                tracing::warn!(
                    target: "tool_dependencies",
                    "pandoc binary not found; pandoc_convert tool will not be registered",
                );
                None
            }
        })
        .clone()
}

/// Whether an optional tool whose backend lives on this host (an interpreter,
/// a converter, an OCR engine) is available: `probe` decides, except that a
/// test fixture answers with the recorded host's set so results do not
/// depend on what the machine running them has installed (test builds only).
pub fn host_tool_available(tool: &str, probe: impl FnOnce() -> bool) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(available) = RECORDED_HOST_TOOLS.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|tools| tools.iter().any(|name| name == tool))
    }) {
        return available;
    }
    // Only test fixtures read the name.
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = tool;
    probe()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static RECORDED_HOST_TOOLS: std::cell::RefCell<Option<Vec<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Pin [`host_tool_available`] on this thread to a recorded host's tools until
/// the guard drops.
#[cfg(any(test, feature = "test-support"))]
pub fn pin_recorded_host_tools(tools: Vec<String>) -> RecordedHostToolsGuard {
    RECORDED_HOST_TOOLS.with(|cell| *cell.borrow_mut() = Some(tools));
    RecordedHostToolsGuard
}

#[cfg(any(test, feature = "test-support"))]
pub struct RecordedHostToolsGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for RecordedHostToolsGuard {
    fn drop(&mut self) {
        RECORDED_HOST_TOOLS.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Resolve the Node.js runtime once per process. Used by the
/// `js_execution` tool to decide whether to advertise itself in
/// the catalog. Unlike Python, the executable name `node` is the
/// same across every platform we ship to — there's no `node3` or
/// `node.exe` variant to fall through to — so this is a single
/// probe rather than a candidate ladder.
pub fn resolve_node() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            if probe_executable("node") {
                tracing::info!(
                    target: "tool_dependencies",
                    "Resolved Node.js runtime for js_execution",
                );
                Some("node".to_string())
            } else {
                tracing::warn!(
                    target: "tool_dependencies",
                    "Node.js runtime not found; js_execution tool will not be advertised",
                );
                None
            }
        })
        .clone()
}

/// A Node.js runtime chosen by *running* each candidate, plus every candidate
/// rejected on the way and why (for `/plugin` and doctor diagnostics).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeResolution {
    pub selected: Option<(PathBuf, (u32, u32, u32))>,
    pub rejected: Vec<(PathBuf, String)>,
}

impl NodeResolution {
    /// One-line human summary of why no `kind` candidate was selected.
    #[must_use]
    pub fn describe_rejections(&self, kind: HostRuntimeKind) -> String {
        if self.rejected.is_empty() {
            return format!("no `{}` found {}", kind.name(), kind.search_scope());
        }
        self.rejected
            .iter()
            .map(|(path, reason)| format!("{}: {reason}", path.display()))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `major.minor.patch` at the start of `text`, ignoring any pre-release or
/// build suffix (`1.4.2-canary.3+abc`).
fn parse_version_triple(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Parse `node --version` output (`v22.20.0`).
#[must_use]
pub fn parse_node_version(banner: &str) -> Option<(u32, u32, u32)> {
    parse_version_triple(banner.trim().strip_prefix('v')?)
}

/// Whether `version` satisfies the extension host floor `^22.19 || >=24`
/// (the DSH `engines` range: odd-numbered 23 is not an LTS line).
#[must_use]
pub fn node_version_supported_for_extension_host(version: (u32, u32, u32)) -> bool {
    let (major, minor, _) = version;
    (major == 22 && minor >= 19) || major >= 24
}

/// Parse `bun --version` output (`1.4.0`, or `1.4.0-canary.1+abc`).
#[must_use]
pub fn parse_bun_version(banner: &str) -> Option<(u32, u32, u32)> {
    parse_version_triple(banner.trim())
}

/// The oldest Bun the extension host accepts. The `Bun.plugin` module shim,
/// `--no-install`, `--no-env-file`, the macOS jetsam memory limit and the
/// native-code lockdown were measured against Bun 1.4.0 on macOS 26.1 arm64
/// only. CI's JS host-suite leg installs Bun 1.4.0 on `ubuntu-latest`; the
/// Rust host integration tests do not run on Bun in CI, and no Windows Bun
/// run is recorded. A newer Bun on which a lock no longer holds fails the
/// host's start (`extension-host/src/runtime.ts`).
pub const BUN_MIN_VERSION_FOR_EXTENSION_HOST: (u32, u32, u32) = (1, 4, 0);

/// Node flags that switch off builtins able to load native code in-process:
/// `node:sqlite` (SQLite extensions are `dlopen`ed even under `--no-addons`)
/// and `node:ffi` (on by default where it exists: Node 26.10 has it, 22.20 and
/// 24.19 reject the flag). Each is passed only when the chosen Node accepts
/// it; the host refuses to start if either builtin is still available.
pub const NODE_NATIVE_CODE_FLAGS: &[&str] = &["--no-experimental-sqlite", "--no-experimental-ffi"];

/// A JavaScript runtime that can run the extension host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRuntimeKind {
    Bun,
    Node,
}

impl HostRuntimeKind {
    /// The name the host reports in `host/hello` (`bun` / `node`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Node => "node",
        }
    }

    fn program(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Self::Bun, false) => "bun",
            (Self::Bun, true) => "bun.exe",
            (Self::Node, false) => "node",
            (Self::Node, true) => "node.exe",
        }
    }

    fn floor(self) -> String {
        match self {
            Self::Bun => {
                let (major, minor, patch) = BUN_MIN_VERSION_FOR_EXTENSION_HOST;
                format!(">={major}.{minor}.{patch}")
            }
            // `node_version_supported_for_extension_host`.
            Self::Node => "^22.19 || >=24".to_string(),
        }
    }

    /// Where a search for this runtime looks ([`runtime_candidates`]).
    fn search_scope(self) -> &'static str {
        match self {
            Self::Bun => "on PATH or in $BUN_INSTALL/bin (default ~/.bun/bin)",
            Self::Node => "on PATH",
        }
    }

    fn parse(self, banner: &str) -> Option<(u32, u32, u32)> {
        match self {
            Self::Bun => parse_bun_version(banner),
            Self::Node => parse_node_version(banner),
        }
    }

    fn supported(self, version: (u32, u32, u32)) -> bool {
        match self {
            Self::Bun => version >= BUN_MIN_VERSION_FOR_EXTENSION_HOST,
            Self::Node => node_version_supported_for_extension_host(version),
        }
    }
}

/// The runtime chosen for the extension host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRuntime {
    pub kind: HostRuntimeKind,
    pub path: PathBuf,
    pub version: (u32, u32, u32),
    /// Node: the [`NODE_NATIVE_CODE_FLAGS`] this Node accepts. Empty for Bun.
    pub native_code_flags: Vec<&'static str>,
    /// The canonical host entry is embedded in this Bun executable.
    pub compiled: bool,
}

impl HostRuntime {
    #[must_use]
    pub fn version_string(&self) -> String {
        let (major, minor, patch) = self.version;
        format!("{major}.{minor}.{patch}")
    }

    /// Whether a version the running host reported (`process.versions.bun`
    /// or `process.versions.node`: no `v`, maybe a pre-release suffix) is the
    /// version this runtime's probe saw.
    #[must_use]
    pub fn reports_version(&self, reported: &str) -> bool {
        parse_version_triple(reported.trim().trim_start_matches('v')) == Some(self.version)
    }
}

/// The outcome of `[extension_host] runtime` selection, with every rejected
/// candidate (for `/plugin` and doctor). `bun` / `node` are `None` when that
/// runtime was not probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRuntimeResolution {
    pub choice: codewhale_config::extension_host::ExtensionHostRuntime,
    pub selected: Option<HostRuntime>,
    pub bun: Option<NodeResolution>,
    pub node: Option<NodeResolution>,
}

impl HostRuntimeResolution {
    /// One line: what runs the host and why, including why Bun was passed
    /// over when `auto` fell back to Node, and every candidate of the chosen
    /// runtime that was skipped or rejected on the way.
    #[must_use]
    pub fn summary(&self) -> String {
        let choice = self.choice.as_str();
        let Some(runtime) = &self.selected else {
            return self.failure();
        };
        let mut line = format!(
            "{} {} at {} (runtime = \"{choice}\")",
            runtime.kind.name(),
            runtime.version_string(),
            runtime.path.display()
        );
        if runtime.compiled {
            line.push_str("; compiled canonical host (runtime embedded)");
        }
        if runtime.kind == HostRuntimeKind::Node
            && let Some(bun) = &self.bun
        {
            line.push_str(&format!(
                "; Bun {} not used: {}",
                HostRuntimeKind::Bun.floor(),
                bun.describe_rejections(HostRuntimeKind::Bun)
            ));
        }
        let probed = match runtime.kind {
            HostRuntimeKind::Bun => self.bun.as_ref(),
            HostRuntimeKind::Node => self.node.as_ref(),
        };
        if let Some(probed) = probed
            && !probed.rejected.is_empty()
        {
            line.push_str(&format!(
                "; passed over: {}",
                probed.describe_rejections(runtime.kind)
            ));
        }
        line
    }

    /// Why no runtime was selected.
    #[must_use]
    pub fn failure(&self) -> String {
        let mut reasons = Vec::new();
        if let Some(bun) = &self.bun {
            reasons.push(format!(
                "bun: {}",
                bun.describe_rejections(HostRuntimeKind::Bun)
            ));
        }
        if let Some(node) = &self.node {
            reasons.push(format!(
                "node: {}",
                node.describe_rejections(HostRuntimeKind::Node)
            ));
        }
        let (bun, node) = (HostRuntimeKind::Bun.floor(), HostRuntimeKind::Node.floor());
        let wanted = match self.choice {
            codewhale_config::extension_host::ExtensionHostRuntime::Auto => {
                format!("Bun {bun} or Node.js {node} (set `[extension_host] bun` or `node`)")
            }
            codewhale_config::extension_host::ExtensionHostRuntime::Bun => {
                format!("Bun {bun} (`runtime = \"bun\"`; set `[extension_host] bun`)")
            }
            codewhale_config::extension_host::ExtensionHostRuntime::Node => {
                format!("Node.js {node} (set `[extension_host] node`)")
            }
        };
        format!("the extension host needs {wanted}; {}", reasons.join("; "))
    }
}

/// A probe of a runtime binary: no inherited environment (no credentials, no
/// `NODE_OPTIONS` preloads), since it runs unsandboxed; Windows needs
/// `SystemRoot` to load system DLLs.
fn probe_command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    codewhale_util::utils::suppress_console_window(&mut cmd);
    cmd.env_clear();
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        cmd.env("SystemRoot", root);
    }
    cmd.stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

fn probe_runtime_version(
    kind: HostRuntimeKind,
    path: &Path,
    bundle_sha256: &str,
) -> Result<(u32, u32, u32), String> {
    // Only absolute candidates are run: a relative `PATH` entry resolves
    // against the current (workspace) directory, where a repository could
    // plant a `node`.
    if !path.is_absolute() {
        return Err("not an absolute path; skipped".to_string());
    }
    let mut cmd = probe_command(path);
    let compiled = kind == HostRuntimeKind::Bun && is_compiled_host_path(path);
    cmd.arg(if compiled {
        "--codewhale-host-info"
    } else {
        "--version"
    });
    let output = probe_output(&mut cmd, true, VERSION_PROBE_TIMEOUT)
        .map_err(|error| format!("does not start ({error})"))?;
    if !output.status.success() {
        return Err(format!("does not run (exit {})", output.status));
    }
    if compiled {
        return parse_compiled_host_info(&output.stdout, bundle_sha256);
    }
    let banner = String::from_utf8_lossy(&output.stdout);
    kind.parse(&banner)
        .ok_or_else(|| format!("unrecognized version banner `{}`", banner.trim()))
}

/// A packaged host is adjacent to the running Engine, not searched in a
/// repository or downloaded at launch. Explicit runtime overrides remain sole
/// candidates; `runtime = "node"` never probes this optional Bun image.
fn compiled_host_candidate() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(if cfg!(windows) {
        "codewhale-extension-host.exe"
    } else {
        "codewhale-extension-host"
    });
    candidate.is_file().then_some(candidate)
}

fn is_compiled_host_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == "codewhale-extension-host" || name == "codewhale-extension-host.exe"
    })
}

fn parse_compiled_host_info(
    bytes: &[u8],
    expected_source: &str,
) -> Result<(u32, u32, u32), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Info {
        kind: String,
        runtime: String,
        platform: String,
        arch: String,
        version: String,
        bundle_sha256: String,
    }
    if bytes.len() > 4096 {
        return Err("compiled host identity exceeds 4096 bytes".to_string());
    }
    let info: Info = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid compiled host identity ({error})"))?;
    if info.kind != "codewhale-extension-host" || info.runtime != "bun" {
        return Err("executable is not a compiled Bun extension host".to_string());
    }
    let expected_platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        os => os,
    };
    let expected_arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        arch => arch,
    };
    if info.platform != expected_platform || info.arch != expected_arch {
        return Err("compiled host executed target differs from this Engine".to_string());
    }
    if info.bundle_sha256 != expected_source {
        return Err("compiled host source digest differs from this Engine; reinstall matching release assets or choose a system runtime".to_string());
    }
    parse_bun_version(&info.version).ok_or_else(|| "invalid compiled Bun version".to_string())
}

/// Every `program` on `PATH`; for Bun also its default install location
/// (`$BUN_INSTALL/bin`, else `~/.bun/bin`), which the Bun installer adds to
/// shell profiles but a GUI-launched process may not see.
fn runtime_candidates(kind: HostRuntimeKind) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = executable_path_candidates(kind.program())
        .into_iter()
        .filter(|candidate| candidate.is_file())
        .collect();
    if kind == HostRuntimeKind::Bun {
        let install = std::env::var_os("BUN_INSTALL")
            .map(PathBuf::from)
            .or_else(|| codewhale_paths::user_home().map(|home| home.join(".bun")));
        if let Some(bin) = install.map(|root| root.join("bin").join(kind.program()))
            && bin.is_file()
        {
            candidates.push(bin);
        }
    }
    candidates
}

/// Why a runtime found by *searching* must not be run: it sits in a
/// `node_modules` tree (a package's bin shim, such as
/// `node_modules/.bin/bun`), or inside the current working directory (usually
/// the workspace), where a repository could plant it. Either would run
/// unsandboxed as a version probe and then host plugin code. A configured
/// override is never checked: naming a path is the opt-in.
///
/// Known limits: a working directory that contains the user's home (a
/// launch from `~`, or from `/` as GUI apps are) is not treated as a
/// workspace, because every user-level install lives under it, so a runtime
/// planted directly there is not caught. The `node_modules` test reads the
/// `PATH` spelling, not the resolved target, so a global npm install reached
/// through a symlink outside `node_modules` still counts as trusted.
fn untrusted_location(
    kind: HostRuntimeKind,
    candidate: &Path,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<String> {
    let opt_in = format!("set `[extension_host] {}` to use it anyway", kind.name());
    if candidate
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("node_modules"))
    {
        return Some(format!(
            "inside a `node_modules` directory; skipped ({opt_in})"
        ));
    }
    let cwd = cwd?;
    let contains_home = |home: &Path| {
        home.starts_with(cwd) || std::fs::canonicalize(home).is_ok_and(|home| home.starts_with(cwd))
    };
    if cwd.parent().is_none() || home.is_some_and(contains_home) {
        return None;
    }
    // `current_dir` is the resolved path; a `PATH` entry may be spelled
    // through a symlink (`/tmp` on macOS), so compare its resolved directory.
    let inside = candidate.starts_with(cwd)
        || candidate
            .parent()
            .and_then(|dir| std::fs::canonicalize(dir).ok())
            .is_some_and(|dir| dir.starts_with(cwd));
    inside.then(|| {
        format!(
            "inside the working directory {}; skipped ({opt_in})",
            cwd.display()
        )
    })
}

/// Resolve one runtime. A configured override is the only candidate: one
/// that does not run or is below the floor fails resolution for this
/// runtime with its reason, rather than falling through to a search.
/// Otherwise the search candidates, minus those in an [`untrusted_location`],
/// which are recorded as rejected without being run. Blocking.
fn resolve_runtime(
    kind: HostRuntimeKind,
    override_path: Option<&Path>,
    bundle_sha256: &str,
) -> NodeResolution {
    if let Some(path) = override_path {
        return select_runtime(kind, vec![path.to_path_buf()], bundle_sha256);
    }
    let cwd = std::env::current_dir().ok();
    let home = codewhale_paths::user_home();
    let mut skipped = Vec::new();
    let compiled = (kind == HostRuntimeKind::Bun)
        .then(compiled_host_candidate)
        .flatten();
    let candidates = compiled
        .iter()
        .cloned()
        .chain(runtime_candidates(kind))
        .filter(|candidate| {
            // An image shipped beside the Engine has its install authority.
            // It is still probed for the exact embedded source identity.
            if compiled.as_ref() == Some(candidate) {
                return true;
            }
            match untrusted_location(kind, candidate, cwd.as_deref(), home.as_deref()) {
                Some(reason) => {
                    skipped.push((candidate.clone(), reason));
                    false
                }
                None => true,
            }
        })
        .collect();
    let mut resolution = select_runtime(kind, candidates, bundle_sha256);
    skipped.append(&mut resolution.rejected);
    resolution.rejected = skipped;
    resolution
}

/// Choose the extension host's runtime for `[extension_host] runtime`:
/// `bun` and `node` try only that runtime (an explicit choice never falls
/// back); `auto` prefers a supported Bun and otherwise uses Node, recording
/// why Bun was passed over. Blocking: call from `spawn_blocking`.
#[must_use]
pub fn resolve_extension_host_runtime(
    choice: codewhale_config::extension_host::ExtensionHostRuntime,
    node_override: Option<&Path>,
    bun_override: Option<&Path>,
    bundle_sha256: &str,
) -> HostRuntimeResolution {
    select_host_runtime(
        choice,
        || resolve_runtime(HostRuntimeKind::Bun, bun_override, bundle_sha256),
        || resolve_runtime(HostRuntimeKind::Node, node_override, bundle_sha256),
    )
}

fn select_host_runtime(
    choice: codewhale_config::extension_host::ExtensionHostRuntime,
    probe_bun: impl FnOnce() -> NodeResolution,
    probe_node: impl FnOnce() -> NodeResolution,
) -> HostRuntimeResolution {
    use codewhale_config::extension_host::ExtensionHostRuntime as Choice;
    let mut resolution = HostRuntimeResolution {
        choice,
        selected: None,
        bun: None,
        node: None,
    };
    let pick = |kind: HostRuntimeKind, probe: &NodeResolution| {
        probe.selected.clone().map(|(path, version)| HostRuntime {
            native_code_flags: match kind {
                HostRuntimeKind::Bun => Vec::new(),
                HostRuntimeKind::Node => accepted_flags(&path, NODE_NATIVE_CODE_FLAGS),
            },
            compiled: kind == HostRuntimeKind::Bun && is_compiled_host_path(&path),
            kind,
            path,
            version,
        })
    };
    if matches!(choice, Choice::Auto | Choice::Bun) {
        let bun = probe_bun();
        resolution.selected = pick(HostRuntimeKind::Bun, &bun);
        resolution.bun = Some(bun);
    }
    if resolution.selected.is_none() && matches!(choice, Choice::Auto | Choice::Node) {
        let node = probe_node();
        resolution.selected = pick(HostRuntimeKind::Node, &node);
        resolution.node = Some(node);
    }
    resolution
}

/// The `flags` a runtime starts with (`<runtime> <flag> --version` exits 0).
/// Blocking: one short probe per flag.
fn accepted_flags(path: &Path, flags: &[&'static str]) -> Vec<&'static str> {
    flags
        .iter()
        .copied()
        .filter(|flag| {
            let mut cmd = probe_command(path);
            cmd.args([*flag, "--version"])
                .stdout(std::process::Stdio::null());
            cmd.status().is_ok_and(|status| status.success())
        })
        .collect()
}

fn select_runtime(
    kind: HostRuntimeKind,
    candidates: Vec<PathBuf>,
    bundle_sha256: &str,
) -> NodeResolution {
    let mut seen = std::collections::HashSet::new();
    let mut resolution = NodeResolution::default();
    for candidate in candidates {
        // Deduplicate by spelling only: resolving symlinks here would be a
        // blocking call per candidate for a cosmetic gain.
        if !seen.insert(candidate.clone()) {
            continue;
        }
        match probe_runtime_version(kind, &candidate, bundle_sha256) {
            Ok(version) if kind.supported(version) => {
                resolution.selected = Some((candidate, version));
                break;
            }
            Ok((major, minor, patch)) => resolution.rejected.push((
                candidate,
                format!(
                    "{major}.{minor}.{patch} is below the {} floor",
                    kind.floor()
                ),
            )),
            Err(reason) => resolution.rejected.push((candidate, reason)),
        }
    }
    resolution
}

// ---------------------------------------------------------------------------
// ExternalTool trait — unified subprocess interface
// ---------------------------------------------------------------------------

/// A tool that DeepSeek-TUI shells out to. Instead of scattering
/// `Command::new("git")` / `Command::new("gh")` across the codebase,
/// each external dependency implements this trait once in this module.
/// Callers ask the tool for a pre-populated [`Command`] and chain their
/// own args, working directory, and spawn method.
///
/// # Example
///
/// ```ignore
/// let output = Git::command()
///     .expect("git not found")
///     .args(["diff", "--stat"])
///     .current_dir(&workspace)
///     .output()?;
/// ```
pub trait ExternalTool {
    /// Candidate binary names, tried in order until one responds to
    /// `--version`.  For single-binary tools (git, gh, node) this is a
    /// one-element slice.
    fn candidates() -> &'static [&'static str];

    /// Resolve the best candidate once per process (cached). Returns
    /// the spec string (e.g. `"python3"` or `"py -3"`).
    fn resolve() -> Option<String>;

    /// Quick availability check — true when the tool was found on PATH.
    fn available() -> bool {
        Self::resolve().is_some()
    }

    /// Build a `std::process::Command` pre-populated with the resolved
    /// binary (and any fixed arguments from a multi-word candidate like
    /// `"py -3"`). Returns `None` when the tool isn't installed.
    ///
    /// Callers should chain `.args(...)`, `.current_dir(...)`, and then
    /// call `.output()`, `.status()`, or `.spawn()`.
    fn command() -> Option<Command> {
        Some(command_for_spec(&Self::resolve()?))
    }

    /// The error a caller sees when the tool is not installed. It names the
    /// binary the user would install (`git`, `python3`), never the Rust type
    /// path (`codewhale_tui::dependencies::Git`).
    fn not_found_error() -> std::io::Error {
        let name = Self::candidates().first().copied().unwrap_or("tool");
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} not found on PATH"),
        )
    }

    /// Convenience: run the tool with arguments in a working directory
    /// and return the captured output.
    fn output(args: &[&str], cwd: &std::path::Path) -> std::io::Result<std::process::Output> {
        let mut cmd = Self::command().ok_or_else(Self::not_found_error)?;
        cmd.args(args).current_dir(cwd).output()
    }

    /// Convenience: run the tool with arguments and return only the
    /// exit status (discards stdout/stderr).
    fn status(args: &[&str], cwd: &std::path::Path) -> std::io::Result<std::process::ExitStatus> {
        let mut cmd = Self::command().ok_or_else(Self::not_found_error)?;
        cmd.args(args).current_dir(cwd).status()
    }

    /// Build a `tokio::process::Command` pre-populated with the resolved
    /// binary (and any fixed arguments from a multi-word candidate like
    /// `"py -3"`). Returns `None` when the tool isn't installed.
    ///
    /// Async callers (`code_execution`, `js_execution`) use this instead
    /// of [`ExternalTool::command`] so they can `.await` the child.
    fn tokio_command() -> Option<tokio::process::Command> {
        let spec = Self::resolve()?;
        let (program, fixed_args) = split_interpreter_spec(&spec);
        let mut cmd = tokio::process::Command::new(&program);
        codewhale_util::utils::suppress_tokio_console_window(&mut cmd);
        for arg in &fixed_args {
            cmd.arg(arg);
        }
        Some(cmd)
    }
}

/// Build a `std::process::Command` for an interpreter spec such as `"py -3"`.
fn command_for_spec(spec: &str) -> Command {
    let (program, fixed_args) = split_interpreter_spec(spec);
    let mut cmd = Command::new(&program);
    codewhale_util::utils::suppress_console_window(&mut cmd);
    for arg in &fixed_args {
        cmd.arg(arg);
    }
    cmd
}

/// [`command_for_spec`] started from the sanitized child environment. Used by
/// the runtimes whose every caller runs model-authored code (Python, Node),
/// so no constructor for them hands out the parent's credentials.
fn scrubbed_command_for_spec(spec: &str) -> Command {
    let mut cmd = command_for_spec(spec);
    codewhale_runtime::child_env::apply_to_command(&mut cmd, std::iter::empty::<(&str, &str)>());
    cmd
}

// ---------------------------------------------------------------------------
// Concrete tool implementations
// ---------------------------------------------------------------------------

/// Git version control.
pub struct Git;

/// Keep a git child from ever waiting on a human.
///
/// Git and ssh read credentials, passphrases and host-key confirmations from
/// `/dev/tty` directly — `stdin(null)` does not stop them — so inside the
/// raw-mode TUI or an HTTP request a prompt is an invisible, indefinite hang.
/// `GIT_TERMINAL_PROMPT=0` makes git fail instead of asking for a username or
/// password; BatchMode ssh fails instead of asking for a passphrase or an
/// unknown host key; an empty `GIT_PAGER` keeps output from ever being paged.
/// A user who pinned their own ssh transport (`GIT_SSH_COMMAND` or `GIT_SSH`)
/// keeps it untouched.
///
/// This is the single definition site; [`Git::command`] and
/// [`Git::tokio_command`] apply it to every product git spawn. Call it
/// directly only for a non-git program that may shell out to git (`gh`).
pub fn apply_git_noninteractive_env(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_PAGER", "");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
}

impl Git {
    /// Flags every `diff`, `show` or patch `log` that collects repository
    /// content passes. `--no-ext-diff`/`--no-textconv` skip diff drivers; a
    /// dirty check or `diff.submodule=diff` spawns a child git inside each
    /// submodule that inherits neither flag, so submodules compare by commit
    /// only. A read that touches the working tree also runs the superproject's
    /// clean filters, which no flag disables: build it from
    /// [`Self::review_command`] too.
    pub const REVIEW_DIFF_ARGS: [&'static str; 4] = [
        "--no-ext-diff",
        "--no-textconv",
        "--submodule=short",
        "--ignore-submodules=dirty",
    ];

    const REVIEW_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Construct a read-only review command with content conversion disabled.
    /// Review callers also pass [`Self::REVIEW_DIFF_ARGS`] for diffs.
    /// Configured filters otherwise execute even when those flags are present.
    pub fn review_command(workspace: &Path) -> anyhow::Result<Command> {
        let overrides = Self::review_filter_overrides(workspace)?;
        let mut command = Self::review_base(workspace)?;
        let mut count = 2;
        for (key, value) in overrides {
            // A subsection may contain '='; `-c key=value` would then
            // override a different key. Separate env fields preserve it.
            command.env(format!("GIT_CONFIG_KEY_{count}"), key);
            command.env(format!("GIT_CONFIG_VALUE_{count}"), value);
            count += 1;
        }
        command.env("GIT_CONFIG_COUNT", count.to_string());
        Ok(command)
    }

    /// Git with fsmonitor, hooks, lazy fetch and replace objects disabled,
    /// running in `workspace`. [`Self::review_command`] adds filter overrides.
    pub fn review_base(workspace: &Path) -> anyhow::Result<Command> {
        use anyhow::Context;

        let mut command = Self::command().context("git not found on PATH")?;

        // Runtime config pairs are required to disable filter names containing
        // '=' without changing which key Git sees. Older Git ignores them.
        // Probe once for this process's cached executable, before any read.
        // A random value prevents repository config from spoofing support.
        static REVIEW_CONFIG_SUPPORTED: OnceLock<bool> = OnceLock::new();
        let supported = match REVIEW_CONFIG_SUPPORTED.get() {
            Some(supported) => *supported,
            None => {
                let supported = match Self::command() {
                    None => false,
                    Some(mut probe) => {
                        let value = uuid::Uuid::new_v4().to_string();
                        probe
                            .current_dir(workspace)
                            .env_remove("GIT_CONFIG")
                            .env_remove("GIT_CONFIG_PARAMETERS")
                            .env("GIT_CONFIG_COUNT", "1")
                            .env("GIT_CONFIG_KEY_0", "codewhale.reviewConfigCapability")
                            .env("GIT_CONFIG_VALUE_0", &value)
                            .args(["config", "--get", "codewhale.reviewConfigCapability"]);
                        match run_bounded_git_with_timeout(
                            &mut probe,
                            "config",
                            Self::REVIEW_CONFIG_TIMEOUT,
                        ) {
                            Ok(output) => {
                                output.status.success()
                                    && output.stdout == format!("{value}\n").as_bytes()
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                                let context = format!(
                                    "Cannot safely inspect Git review configuration: {error}"
                                );
                                return Err(anyhow::Error::new(error).context(context));
                            }
                            Err(_) => false,
                        }
                    }
                };
                *REVIEW_CONFIG_SUPPORTED.get_or_init(|| supported)
            }
        };
        if !supported {
            anyhow::bail!(
                "Cannot safely inspect Git review configuration: Git runtime configuration overrides are unavailable; upgrade Git (2.31 or newer)"
            );
        }

        command
            .current_dir(workspace)
            .stdin(std::process::Stdio::null())
            // GIT_CONFIG redirects only `git config`, not `git diff`.
            // Both phases must observe the same effective repository config.
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_CONFIG_KEY_1", "core.hooksPath")
            .env(
                "GIT_CONFIG_VALUE_1",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        Ok(command)
    }

    /// Config overrides (`key`, `value`) that neutralize every clean/process
    /// filter driver configured for the repository at `workspace`. Callers
    /// apply them through `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`.
    pub fn review_filter_overrides(
        workspace: &Path,
    ) -> anyhow::Result<Vec<(String, &'static str)>> {
        use anyhow::{Context, bail};

        let mut command = Self::review_base(workspace)?;
        command.args([
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|process|required)$",
        ]);
        let output =
            run_bounded_git_with_timeout(&mut command, "config", Self::REVIEW_CONFIG_TIMEOUT)
                .context("Failed to inspect Git review filters")?;
        let no_filters =
            output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty();
        if (!output.status.success() && !no_filters)
            || (!output.stdout.is_empty() && !output.stdout.ends_with(&[0]))
        {
            bail!("Cannot safely inspect Git review configuration");
        }
        let mut filters = std::collections::BTreeSet::new();
        for key in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|key| !key.is_empty())
        {
            let key =
                std::str::from_utf8(key).context("Git review filter name is not valid UTF-8")?;
            let (driver, _) = key
                .rsplit_once('.')
                .context("Invalid Git review filter key")?;
            filters.insert(driver.to_string());
        }
        Ok(filters
            .into_iter()
            .flat_map(|driver| {
                [("clean", ""), ("process", ""), ("required", "false")]
                    .map(|(suffix, value)| (format!("{driver}.{suffix}"), value))
            })
            .collect())
    }
}

impl ExternalTool for Git {
    fn candidates() -> &'static [&'static str] {
        &["git"]
    }

    /// Every `Git` invocation in the product is issued against a repository
    /// the user also works in by hand. `git status` and `git diff`
    /// opportunistically refresh the index, and that refresh takes
    /// `.git/index.lock` — which is why a user's own `git commit` could fail
    /// with "Unable to create '.../.git/index.lock': File exists" while
    /// codewhale was merely idling in the same repo (#5617, reported by
    /// @LmeSzinc).
    ///
    /// `GIT_OPTIONAL_LOCKS=0` suppresses only *optional* lock-taking, so
    /// reads stop touching the index while genuine writes (`add`, `commit`,
    /// `stash`, `update-ref`) are unaffected — including the snapshot
    /// side-repo runner, which writes to its own git dir. `git diff --quiet`
    /// exit-code semantics are preserved, which `snapshot::repo` relies on
    /// for `/undo` cursoring.
    ///
    /// Set here rather than on the `ExternalTool::command` default so it does
    /// not leak onto `Gh`, `Cargo`, `Node`, `Python`, or `RustC`. Prefer the
    /// environment variable over the `--no-optional-locks` flag: the flag is
    /// top-level (it must precede the subcommand, awkward for the several
    /// call sites that build argument vectors), it would change the
    /// agent-visible command string rendered by `tools::git::format_command`,
    /// and an unknown flag hard-fails on old git while an unknown environment
    /// variable is silently ignored.
    ///
    /// The child also starts from the sanitized environment (see
    /// [`codewhale_runtime::child_env::apply_to_git_command`]): workspace config such as
    /// `core.fsmonitor` or a clean filter makes even a read like `git status`
    /// run a program the workspace chose, so no git child gets the parent's
    /// credentials. The guards below are applied after the scrub.
    fn command() -> Option<Command> {
        let mut cmd = Command::new(Self::resolve()?);
        codewhale_util::utils::suppress_console_window(&mut cmd);
        codewhale_runtime::child_env::apply_to_git_command(&mut cmd);
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        apply_git_noninteractive_env(&mut cmd);
        Some(cmd)
    }

    /// Same environment as [`Git::command`]: the trait default would build a
    /// bare command and silently drop the lock and prompt guards.
    fn tokio_command() -> Option<tokio::process::Command> {
        Self::command().map(tokio::process::Command::from)
    }

    fn resolve() -> Option<String> {
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                // The review capability cache must follow the executable
                // checked here even if a later child changes PATH or cwd.
                let path = resolve_executable_path(Self::candidates().first()?, "--version")?;
                let path = std::path::absolute(path).ok()?;
                tracing::info!(target: "tool_dependencies", "Resolved git binary");
                Some(path.to_string_lossy().into_owned())
            })
            .clone()
    }
}

/// GitHub CLI.
pub struct Gh;

impl ExternalTool for Gh {
    fn candidates() -> &'static [&'static str] {
        &["gh"]
    }

    fn resolve() -> Option<String> {
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                for candidate in Self::candidates() {
                    if probe_executable(candidate) {
                        tracing::info!(target: "tool_dependencies", "Resolved gh binary");
                        return Some((*candidate).to_string());
                    }
                }
                None
            })
            .clone()
    }
}

/// Rust compiler — used for version reporting in diagnostics.
pub struct RustC;

impl ExternalTool for RustC {
    fn candidates() -> &'static [&'static str] {
        &["rustc"]
    }

    fn resolve() -> Option<String> {
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                // Probe with capture so the `--version` banner observed during
                // resolution is reused by [`rustc_version_banner`] instead of
                // paying a second rustc process launch (each launch loads
                // libLLVM, which dominated diagnostic-command init profiles).
                for candidate in Self::candidates() {
                    if let Some(banner) = probe_executable_capturing(candidate, "--version") {
                        tracing::info!(target: "tool_dependencies", "Resolved rustc binary");
                        let _ = RUSTC_VERSION_BANNER.set(Some(banner));
                        return Some((*candidate).to_string());
                    }
                }
                None
            })
            .clone()
    }
}

/// Captured `--version` banner from the [`RustC`] resolution probe.
///
/// `None` until `RustC::resolve()`/`available()`/`command()` first runs, or
/// when rustc is absent/failing. Reading this after an `available()` check
/// yields the same string the tool would print, without a second process.
static RUSTC_VERSION_BANNER: OnceLock<Option<String>> = OnceLock::new();

/// The rustc `--version` banner, if rustc resolved successfully.
///
/// Populated as a side effect of resolving [`RustC`]; this reads no fresh
/// process state. Callers wanting the value should touch `RustC::available()`
/// first (as the diagnostics path does).
#[must_use]
pub fn rustc_version_banner() -> Option<String> {
    RUSTC_VERSION_BANNER.get().cloned().flatten()
}

/// Rust build tool — used by the `run_tests` tool.
pub struct Cargo;

impl ExternalTool for Cargo {
    fn candidates() -> &'static [&'static str] {
        &["cargo"]
    }

    fn resolve() -> Option<String> {
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                for candidate in Self::candidates() {
                    if probe_executable(candidate) {
                        tracing::info!(target: "tool_dependencies", "Resolved cargo binary");
                        return Some((*candidate).to_string());
                    }
                }
                None
            })
            .clone()
    }
}

/// Python interpreter — used by `code_execution` tool and RLM REPL.
/// Delegates to the existing [`resolve_python_interpreter`] so the
/// multi-candidate ladder (`python3` → `python` → `py -3`) is
/// shared with legacy callers until they migrate to the trait.
pub struct Python;

impl ExternalTool for Python {
    fn candidates() -> &'static [&'static str] {
        PYTHON_CANDIDATES
    }

    /// Every Python caller runs model-authored code (`code_execution`, the
    /// RLM REPL), so both constructors (and the `output`/`status` helpers
    /// built on them) start the child from the sanitized environment instead
    /// of inheriting provider credentials and other parent secrets. Callers
    /// that need extra variables re-apply them through
    /// [`codewhale_runtime::child_env::apply_to_tokio_command`] with explicit overrides.
    fn command() -> Option<Command> {
        Some(scrubbed_command_for_spec(&Self::resolve()?))
    }

    fn tokio_command() -> Option<tokio::process::Command> {
        Self::command().map(tokio::process::Command::from)
    }

    fn resolve() -> Option<String> {
        resolve_python_interpreter()
    }
}

/// Node.js runtime — used by the `js_execution` tool.
/// The binary name `node` is the same on every platform we support,
/// so this is a single probe rather than a candidate ladder.
pub struct Node;

impl ExternalTool for Node {
    fn candidates() -> &'static [&'static str] {
        &["node"]
    }

    /// Node runs model-authored code (`js_execution`); like [`Python`], every
    /// constructor starts from the sanitized environment.
    fn command() -> Option<Command> {
        Some(scrubbed_command_for_spec(&Self::resolve()?))
    }

    fn tokio_command() -> Option<tokio::process::Command> {
        Self::command().map(tokio::process::Command::from)
    }

    fn resolve() -> Option<String> {
        resolve_node()
    }
}

// ---------------------------------------------------------------------------
// Legacy interpreter helpers (kept for existing callers until migrated)
// ---------------------------------------------------------------------------

/// Split an interpreter spec like `"py -3"` into the program name
/// and any initial arguments. Returns `("py", vec!["-3"])` for the
/// example; returns `("python3", vec![])` for a bare name.
///
/// Callers spawn `Command::new(program).args(args).arg(script_path)`.
#[must_use]
pub fn split_interpreter_spec(spec: &str) -> (String, Vec<String>) {
    let mut parts = spec.split_whitespace();
    let program = parts.next().unwrap_or("").to_string();
    let args = parts.map(str::to_string).collect();
    (program, args)
}

/// Grace granted to the pipe readers after git has exited. A clean git
/// closes its own write ends, so EOF is already waiting; only a grandchild
/// that inherited the pipes (a post-checkout hook, a `git gc` pack worker)
/// can hold them past exit, and it must not hold the turn pipeline either.
const GIT_PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Run a pre-configured git command under `timeout` with both pipes drained
/// while the child runs (the same concurrent drain `Command::output`
/// performs). A wedged git degrades to an error instead of hanging the caller.
///
/// The timeout path kills the child and reaps it on a detached thread: on a
/// hard-wedged mount git can sit in uninterruptible kernel I/O where even
/// SIGKILL is deferred.
pub fn run_bounded_git_with_timeout(
    cmd: &mut std::process::Command,
    subcommand: &str,
    timeout: Duration,
) -> io::Result<Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    // Drain both pipes while waiting: the restore path's `ls-tree -r` and
    // the diff commands emit output that grows with workspace size, and a
    // child blocked on a full pipe buffer never exits — it would turn every
    // such call into a guaranteed timeout. Readers stream into shared
    // buffers, and the completion channel lets the collect below bound how
    // long a grandchild-held pipe may hold the call after git has exited.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let buf = std::sync::Arc::clone(&stdout_buf);
        let done_tx = done_tx.clone();
        std::thread::spawn(move || {
            if let Some(mut reader) = stdout_pipe {
                read_pipe_to_buffer(&mut reader, &buf);
            }
            let _ = done_tx.send(());
        });
    }
    {
        let buf = std::sync::Arc::clone(&stderr_buf);
        let done_tx = done_tx.clone();
        std::thread::spawn(move || {
            if let Some(mut reader) = stderr_pipe {
                read_pipe_to_buffer(&mut reader, &buf);
            }
            let _ = done_tx.send(());
        });
    }
    drop(done_tx);

    let Some(status) = child.wait_timeout(timeout)? else {
        let _ = child.kill();
        // Reap off the pipeline thread (see the doc comment): the kernel may
        // not deliver the kill until an uninterruptible syscall returns.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("git {subcommand} timed out after {}s", timeout.as_secs()),
        ));
    };
    // Wait for the readers up to the grace, then return what was captured. A
    // clean git already closed its write ends, so both readers deliver
    // immediately; the grace only covers a pipe still held open by an
    // inherited copy (a daemonizing hook). On expiry the captured output is
    // returned with a note on stderr instead of silently truncating; the
    // reader itself ends whenever whatever holds the pipe exits, holding
    // only a pipe read — never the turn pipeline.
    let mut partial = false;
    let mut drained = 0;
    while drained < 2 {
        match done_rx.recv_timeout(GIT_PIPE_DRAIN_GRACE) {
            Ok(()) => drained += 1,
            Err(_) => {
                partial = true;
                break;
            }
        }
    }
    let stdout = stdout_buf.lock().map(|buf| buf.clone()).unwrap_or_default();
    let mut stderr = stderr_buf.lock().map(|buf| buf.clone()).unwrap_or_default();
    if partial {
        if !stderr.is_empty() && stderr.last() != Some(&b'\n') {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(
            b"[codewhale] git output pipes did not close after git exited \
              (kept open by a hook or subprocess?); captured output may be partial\n",
        );
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Read a child's pipe to end into a shared buffer, tolerating interruption.
fn read_pipe_to_buffer(reader: &mut impl io::Read, buf: &std::sync::Mutex<Vec<u8>>) {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                if let Ok(mut buf) = buf.lock() {
                    buf.extend_from_slice(&chunk[..read]);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_host_identity_requires_exact_engine_source_and_real_bun_version() {
        let digest = "a".repeat(64);
        let info = serde_json::json!({
            "kind": "codewhale-extension-host", "runtime": "bun", "version": "1.4.0", "bundle_sha256": digest,
            "platform": match std::env::consts::OS { "macos" => "darwin", "windows" => "win32", os => os },
            "arch": match std::env::consts::ARCH { "aarch64" => "arm64", "x86_64" => "x64", arch => arch },
        });
        assert_eq!(
            parse_compiled_host_info(&serde_json::to_vec(&info).unwrap(), &digest),
            Ok((1, 4, 0))
        );
        let mut changed = info.clone();
        changed["bundle_sha256"] = "b".repeat(64).into();
        assert!(
            parse_compiled_host_info(&serde_json::to_vec(&changed).unwrap(), &digest)
                .unwrap_err()
                .contains("differs from this Engine")
        );
        for (key, value) in [
            ("runtime", "node"),
            ("kind", "bun"),
            ("version", "not-a-version"),
            ("platform", "incorrect-platform"),
            ("arch", "incorrect-arch"),
        ] {
            let mut invalid = info.clone();
            invalid[key] = value.into();
            assert!(
                parse_compiled_host_info(&serde_json::to_vec(&invalid).unwrap(), &digest).is_err()
            );
        }
        assert!(
            parse_compiled_host_info(&vec![b' '; 4097], &digest)
                .unwrap_err()
                .contains("4096")
        );
        assert!(parse_compiled_host_info(b"{}", &digest).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn version_probes_scrub_credentials_preloads_and_close_stdin() {
        let _home = codewhale_runtime::test_support::SealedHome::new();
        let _secret = codewhale_runtime::test_support::EnvVarGuard::set(
            "DEEPSEEK_API_KEY",
            "synthetic-probe-secret",
        );
        let _preload =
            codewhale_runtime::test_support::EnvVarGuard::set("NODE_OPTIONS", "synthetic-preload");
        let mut command = version_probe_command("/bin/sh");
        command.args(["-c", "[ -z \"${DEEPSEEK_API_KEY+x}\" ] && [ -z \"${NODE_OPTIONS+x}\" ] && ! read ignored && printf clean"]);
        let output = probe_output(&mut command, true, VERSION_PROBE_TIMEOUT).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"clean");
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_timeout_kills_ordinary_descendants() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("descendant.pid");
        let mut command = version_probe_command("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo $! > \"$1\"; wait", "probe"])
            .arg(&pid_file);
        let started = std::time::Instant::now();
        let error =
            probe_output(&mut command, false, std::time::Duration::from_millis(150)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let pid = codewhale_runtime::process_tree::read_pid_file(
            &pid_file,
            std::time::Duration::from_secs(1),
        );
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            pid,
            std::time::Duration::from_secs(2)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_reaps_successful_parents_inherited_pipe_child() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pipe-child.pid");
        let mut command = version_probe_command("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo $! > \"$1\"; printf version", "probe"])
            .arg(&pid_file);
        let output = probe_output(&mut command, true, VERSION_PROBE_TIMEOUT).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"version");
        let pid = codewhale_runtime::process_tree::read_pid_file(
            &pid_file,
            std::time::Duration::from_secs(1),
        );
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            pid,
            std::time::Duration::from_secs(2)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_refuses_oversized_banner() {
        let mut command = version_probe_command("/bin/sh");
        command.args(["-c", "head -c 20000 /dev/zero"]);
        assert_eq!(
            probe_output(&mut command, true, VERSION_PROBE_TIMEOUT)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn node_version_banner_parses_and_floor_matches_dsh_engines() {
        assert_eq!(parse_node_version("v22.20.0\n"), Some((22, 20, 0)));
        assert_eq!(parse_node_version("v24.1.0-nightly"), Some((24, 1, 0)));
        assert_eq!(parse_node_version("22.20.0"), None);
        assert!(node_version_supported_for_extension_host((22, 19, 0)));
        assert!(!node_version_supported_for_extension_host((22, 18, 9)));
        assert!(!node_version_supported_for_extension_host((23, 11, 0)));
        assert!(!node_version_supported_for_extension_host((20, 19, 0)));
        assert!(node_version_supported_for_extension_host((24, 0, 0)));
    }

    #[cfg(unix)]
    #[test]
    fn node_ladder_skips_broken_and_old_candidates() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let broken = script(
            "broken-node",
            "echo 'dyld: Library not loaded' >&2; exit 134",
        );
        let old = script("old-node", "echo v20.11.1");
        let good = script("good-node", "echo v22.20.0");
        let later = script("later-node", "echo v24.0.0");
        let relative = PathBuf::from("node_modules/.bin/node");
        let resolution = select_runtime(
            HostRuntimeKind::Node,
            vec![
                relative.clone(),
                broken.clone(),
                old.clone(),
                good.clone(),
                later,
            ],
            "",
        );
        assert_eq!(resolution.selected, Some((good, (22, 20, 0))));
        assert_eq!(resolution.rejected.len(), 3);
        assert_eq!(resolution.rejected[0].0, relative);
        assert!(resolution.rejected[0].1.contains("not an absolute path"));
        assert_eq!(resolution.rejected[1].0, broken);
        assert!(resolution.rejected[1].1.contains("does not run"));
        assert_eq!(resolution.rejected[2].0, old);
        assert!(resolution.rejected[2].1.contains("below"));
    }

    #[test]
    fn bun_version_banner_parses_and_floor_is_the_validated_release() {
        assert_eq!(parse_bun_version("1.4.0\n"), Some((1, 4, 0)));
        assert_eq!(parse_bun_version("1.4.2-canary.3+abc"), Some((1, 4, 2)));
        assert_eq!(parse_bun_version("v1.4.0"), None);
        assert!(HostRuntimeKind::Bun.supported((1, 4, 0)));
        assert!(HostRuntimeKind::Bun.supported((2, 0, 0)));
        assert!(!HostRuntimeKind::Bun.supported((1, 3, 14)));
    }

    #[cfg(unix)]
    #[test]
    fn host_runtime_auto_prefers_bun_and_explicit_choices_never_fall_back() {
        use codewhale_config::extension_host::ExtensionHostRuntime as Choice;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let bun = script("bun", "echo 1.4.0");
        let old_bun = script("old-bun", "echo 1.3.14");
        // A Node without `node:ffi`: it rejects `--no-experimental-ffi` the
        // way Node 22 and 24 do.
        let node = script(
            "node",
            "case \"$1\" in --no-experimental-ffi) echo 'bad option' >&2; exit 9;; esac\necho v24.1.0",
        );

        let probe = |kind, candidates: &[&PathBuf]| {
            select_runtime(
                kind,
                candidates.iter().map(|path| (*path).clone()).collect(),
                "",
            )
        };

        // auto: a supported Bun wins, and Node is never probed. The Bun
        // passed over on the way is in the summary.
        let auto = select_host_runtime(
            Choice::Auto,
            || probe(HostRuntimeKind::Bun, &[&old_bun, &bun]),
            || panic!("node must not be probed when Bun is usable"),
        );
        let selected = auto.selected.clone().unwrap();
        assert_eq!(selected.kind, HostRuntimeKind::Bun);
        assert_eq!(selected.path, bun);
        assert_eq!(selected.version_string(), "1.4.0");
        assert!(selected.native_code_flags.is_empty());
        let summary = auto.summary();
        assert!(summary.starts_with("bun 1.4.0 at "), "{summary}");
        assert!(summary.contains("(runtime = \"auto\")"), "{summary}");
        assert!(
            summary.contains("passed over: ") && summary.contains("1.3.14 is below"),
            "{summary}"
        );

        // auto without a supported Bun: Node, and the summary says why.
        let fallback = select_host_runtime(
            Choice::Auto,
            || probe(HostRuntimeKind::Bun, &[&old_bun]),
            || probe(HostRuntimeKind::Node, &[&node]),
        );
        assert_eq!(
            fallback.selected.as_ref().unwrap().kind,
            HostRuntimeKind::Node
        );
        let summary = fallback.summary();
        assert!(summary.starts_with("node 24.1.0 at "), "{summary}");
        assert!(summary.contains("Bun >=1.4.0 not used"), "{summary}");
        assert!(
            summary.contains("1.3.14 is below the >=1.4.0 floor"),
            "{summary}"
        );
        // Nothing found: the message names every place that was searched.
        let none_found = select_host_runtime(Choice::Auto, NodeResolution::default, || {
            probe(HostRuntimeKind::Node, &[&node])
        });
        assert!(
            none_found
                .summary()
                .contains("no `bun` found on PATH or in $BUN_INSTALL/bin (default ~/.bun/bin)"),
            "{}",
            none_found.summary()
        );

        // runtime = "bun": no Node fallback, even when Node works.
        let bun_only = select_host_runtime(
            Choice::Bun,
            || probe(HostRuntimeKind::Bun, &[&old_bun]),
            || panic!("runtime = \"bun\" must not probe node"),
        );
        assert!(bun_only.selected.is_none());
        assert!(
            bun_only.failure().contains("needs Bun >=1.4.0"),
            "{}",
            bun_only.failure()
        );

        // runtime = "node": Bun is never probed.
        let node_only = select_host_runtime(
            Choice::Node,
            || panic!("runtime = \"node\" must not probe bun"),
            || probe(HostRuntimeKind::Node, &[&node]),
        );
        let node_runtime = node_only.selected.unwrap();
        assert_eq!(node_runtime.kind, HostRuntimeKind::Node);
        // Only the flags this Node starts with are passed.
        assert_eq!(
            node_runtime.native_code_flags,
            vec!["--no-experimental-sqlite"]
        );
        assert!(node_only.bun.is_none());
    }

    /// A runtime found by searching that a repository could have planted is
    /// never run; a configured override is exempt from that check but is
    /// final: when it fails, resolution fails with its reason.
    #[cfg(unix)]
    #[test]
    fn untrusted_search_candidates_are_skipped_and_a_failing_override_is_final() {
        use codewhale_config::extension_host::ExtensionHostRuntime as Choice;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let kind = HostRuntimeKind::Bun;
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(workspace.join("bin")).unwrap();
        // `current_dir` is resolved; the temp dir may be spelled through a
        // symlink (`/var` → `/private/var` on macOS).
        let cwd = std::fs::canonicalize(&workspace).unwrap();

        let shim = Path::new("/opt/tools/node_modules/.bin/bun");
        let reason = untrusted_location(kind, shim, None, None).unwrap();
        assert!(reason.contains("`node_modules`"), "{reason}");
        let reason = untrusted_location(
            kind,
            &workspace.join("bin/bun"),
            Some(cwd.as_path()),
            Some(home.as_path()),
        )
        .unwrap();
        assert!(reason.contains("inside the working directory"), "{reason}");
        assert!(reason.contains("`[extension_host] bun`"), "{reason}");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(workspace.join("bin"), &link).unwrap();
        assert!(
            untrusted_location(
                kind,
                &link.join("bun"),
                Some(cwd.as_path()),
                Some(home.as_path())
            )
            .is_some()
        );
        assert_eq!(
            untrusted_location(
                kind,
                &dir.path().join("elsewhere/bun"),
                Some(cwd.as_path()),
                Some(home.as_path())
            ),
            None
        );
        // A working directory that contains the home is not a workspace.
        let user_bun = home.join(".bun/bin/bun");
        assert_eq!(
            untrusted_location(kind, &user_bun, Some(home.as_path()), Some(home.as_path())),
            None
        );
        assert_eq!(
            untrusted_location(kind, &user_bun, Some(Path::new("/")), Some(home.as_path())),
            None
        );

        // A failing override never falls through to a search.
        let broken = script("broken-bun", "exit 3");
        let bun_only =
            resolve_extension_host_runtime(Choice::Bun, None, Some(broken.as_path()), "");
        assert!(bun_only.selected.is_none());
        let failure = bun_only.failure();
        assert!(
            failure.contains(&format!("{}: does not run", broken.display())),
            "{failure}"
        );
        assert_eq!(bun_only.bun.as_ref().unwrap().rejected.len(), 1);
        // Under `auto` the Node that runs instead says why the Bun did not.
        let node = script("node", "echo v24.1.0");
        let auto = resolve_extension_host_runtime(
            Choice::Auto,
            Some(node.as_path()),
            Some(broken.as_path()),
            "",
        );
        let summary = auto.summary();
        assert!(summary.starts_with("node 24.1.0 at "), "{summary}");
        assert!(
            summary.contains(&format!(
                "Bun >=1.4.0 not used: {}: does not run",
                broken.display()
            )),
            "{summary}"
        );
    }

    #[test]
    fn probe_executable_returns_false_for_unknown_binary() {
        // Pick a name we're confident isn't on any developer's PATH.
        // If this ever starts failing locally, rename it.
        assert!(!probe_executable("codewhale-tui-imaginary-binary-xyz123"));
    }

    #[test]
    fn probe_executable_handles_multi_word_specs() {
        // `py -3` should split correctly. The probe will fail on
        // most non-Windows machines (no `py` launcher), which is
        // fine — we're checking that the *split* doesn't crash.
        let _ = probe_executable("py -3");
    }

    #[test]
    fn probe_executable_with_flag_returns_false_for_unknown_binary() {
        assert!(!probe_executable_with_flag(
            "codewhale-tui-imaginary-binary-xyz123",
            "-v"
        ));
    }

    #[test]
    fn probe_executable_delegates_to_double_dash_version() {
        // `probe_executable` must remain exactly
        // `probe_executable_with_flag(.., "--version")`.
        let spec = "codewhale-tui-imaginary-binary-xyz123";
        assert_eq!(
            probe_executable(spec),
            probe_executable_with_flag(spec, "--version")
        );
    }

    #[test]
    fn pdftotext_resolver_detects_installed_poppler_via_dash_v() {
        // Regression for #1667: Poppler's `pdftotext` rejects `--version`
        // (it is parsed as an input filename and exits non-zero), so the
        // generic `--version` probe reports it missing even when installed.
        // The resolver must probe with `-v`. Gated on pdftotext actually
        // being installed so CI without Poppler stays green.
        if probe_executable_with_flag("pdftotext", "-v") {
            assert!(
                resolve_pdftotext().is_some(),
                "an installed pdftotext must be detected via -v (#1667)"
            );
        }
    }

    #[test]
    fn split_interpreter_spec_strips_args() {
        assert_eq!(
            split_interpreter_spec("python3"),
            ("python3".to_string(), Vec::<String>::new())
        );
        assert_eq!(
            split_interpreter_spec("py -3"),
            ("py".to_string(), vec!["-3".to_string()])
        );
        assert_eq!(
            split_interpreter_spec("  python3  "),
            ("python3".to_string(), Vec::<String>::new()),
            "leading/trailing whitespace must be tolerated"
        );
    }

    #[test]
    fn split_interpreter_spec_handles_empty_string() {
        assert_eq!(
            split_interpreter_spec(""),
            (String::new(), Vec::<String>::new())
        );
    }

    #[test]
    fn python_resolver_is_cached_across_calls() {
        // Whatever the first call returns, subsequent calls return
        // the same value (cached). If this test ever flakes, the
        // OnceLock semantics changed and we need to rethink the
        // resolver.
        let first = resolve_python_interpreter();
        let second = resolve_python_interpreter();
        assert_eq!(first, second);
    }

    #[test]
    fn python_resolver_returns_some_on_developer_machines() {
        // CI hosts have Python; developer machines have Python.
        // The one environment where this returns None is bare-bones
        // Windows / minimal CI containers — fine, those just don't
        // get code_execution registered, which is the whole point.
        // We don't assert Some() because we don't want this test
        // to fail in those environments. Instead we just confirm
        // the resolver doesn't panic and returns a stable value.
        let resolved = resolve_python_interpreter();
        if let Some(name) = resolved {
            assert!(
                !name.is_empty(),
                "resolved interpreter name must be non-empty"
            );
            // The resolved name must be one of our candidates.
            assert!(
                PYTHON_CANDIDATES.contains(&name.as_str()),
                "resolved {name:?} is not in PYTHON_CANDIDATES {PYTHON_CANDIDATES:?}"
            );
        }
    }

    // ===================================================================
    // ExternalTool trait tests
    // ===================================================================

    #[test]
    fn python_candidates_matches_const() {
        assert_eq!(Python::candidates(), PYTHON_CANDIDATES);
    }

    #[test]
    fn node_candidates_is_node_only() {
        assert_eq!(Node::candidates(), &["node"]);
    }

    #[test]
    fn git_candidates_is_git_only() {
        assert_eq!(Git::candidates(), &["git"]);
    }

    #[test]
    fn gh_candidates_is_gh_only() {
        assert_eq!(Gh::candidates(), &["gh"]);
    }

    #[test]
    fn rustc_candidates_is_rustc_only() {
        assert_eq!(RustC::candidates(), &["rustc"]);
    }

    #[test]
    fn missing_tool_error_names_the_binary_not_the_rust_type() {
        struct Missing;
        impl ExternalTool for Missing {
            fn candidates() -> &'static [&'static str] {
                &["codewhale-imaginary-tool", "fallback-name"]
            }
            fn resolve() -> Option<String> {
                None
            }
        }

        let error = Missing::output(&["--version"], std::path::Path::new("."))
            .expect_err("an unresolvable tool must not spawn");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(
            error.to_string(),
            "codewhale-imaginary-tool not found on PATH"
        );
        assert!(!error.to_string().contains("::"), "{error}");
        assert_eq!(Git::not_found_error().to_string(), "git not found on PATH");
    }

    #[test]
    fn cargo_candidates_is_cargo_only() {
        assert_eq!(Cargo::candidates(), &["cargo"]);
    }

    #[test]
    fn concrete_resolvers_do_not_cross_contaminate_when_available() {
        let values = [
            Git::resolve().map(|v| ("git", v)),
            Gh::resolve().map(|v| ("gh", v)),
            RustC::resolve().map(|v| ("rustc", v)),
            Cargo::resolve().map(|v| ("cargo", v)),
            Node::resolve().map(|v| ("node", v)),
        ];
        let resolved: Vec<(&str, String)> = values.into_iter().flatten().collect();

        for i in 0..resolved.len() {
            for j in (i + 1)..resolved.len() {
                assert_ne!(
                    resolved[i].1, resolved[j].1,
                    "{} and {} unexpectedly resolved to the same binary",
                    resolved[i].0, resolved[j].0
                );
            }
        }
    }

    #[test]
    fn git_resolve_is_cached() {
        let first = Git::resolve();
        let second = Git::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn gh_resolve_is_cached() {
        let first = Gh::resolve();
        let second = Gh::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn python_trait_resolve_is_cached() {
        let first = Python::resolve();
        let second = Python::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn node_resolve_is_cached() {
        let first = Node::resolve();
        let second = Node::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn rustc_resolve_is_cached() {
        let first = RustC::resolve();
        let second = RustC::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn cargo_resolve_is_cached() {
        let first = Cargo::resolve();
        let second = Cargo::resolve();
        assert_eq!(first, second);
    }

    #[test]
    fn git_available_matches_resolve() {
        assert_eq!(Git::available(), Git::resolve().is_some());
    }

    #[test]
    fn python_available_matches_resolve() {
        assert_eq!(Python::available(), Python::resolve().is_some());
    }

    #[test]
    fn node_available_matches_resolve() {
        assert_eq!(Node::available(), Node::resolve().is_some());
    }

    #[test]
    fn rustc_available_matches_resolve() {
        assert_eq!(RustC::available(), RustC::resolve().is_some());
    }

    #[test]
    fn cargo_available_matches_resolve() {
        assert_eq!(Cargo::available(), Cargo::resolve().is_some());
    }

    #[test]
    fn git_command_returns_some_when_available() {
        if Git::available() {
            assert!(Git::command().is_some());
        }
    }

    /// Every git command we build must be lock-free (#5617). Without this,
    /// a read-only probe can take `.git/index.lock` in the user's own
    /// repository and break a `git commit` they run by hand.
    #[test]
    fn git_command_never_takes_optional_locks() {
        if !Git::available() {
            return;
        }
        let cmd = Git::command().expect("git resolves when available");
        let value = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("GIT_OPTIONAL_LOCKS"))
            .and_then(|(_, value)| value)
            .expect("GIT_OPTIONAL_LOCKS must be set on every git command");
        assert_eq!(value, std::ffi::OsStr::new("0"));
    }

    /// No git spawn may prompt on `/dev/tty` (0.10.1 item 3): a credential,
    /// passphrase or host-key prompt inside the raw-mode TUI is a silent hang.
    #[test]
    fn git_commands_are_non_interactive() {
        if !Git::available() {
            return;
        }
        let std_cmd = Git::command().expect("git resolves when available");
        let tokio_cmd = Git::tokio_command().expect("git resolves when available");
        for envs in [
            std_cmd.get_envs().collect::<Vec<_>>(),
            tokio_cmd.as_std().get_envs().collect::<Vec<_>>(),
        ] {
            let get = |name: &str| {
                envs.iter()
                    .find(|(key, _)| *key == std::ffi::OsStr::new(name))
                    .and_then(|(_, value)| *value)
            };
            assert_eq!(get("GIT_TERMINAL_PROMPT"), Some(std::ffi::OsStr::new("0")));
            assert_eq!(get("GIT_PAGER"), Some(std::ffi::OsStr::new("")));
            assert_eq!(get("GIT_OPTIONAL_LOCKS"), Some(std::ffi::OsStr::new("0")));
            if std::env::var_os("GIT_SSH_COMMAND").is_none()
                && std::env::var_os("GIT_SSH").is_none()
            {
                assert_eq!(
                    get("GIT_SSH_COMMAND"),
                    Some(std::ffi::OsStr::new("ssh -o BatchMode=yes"))
                );
            }
        }
    }

    /// The suppression is deliberately scoped to git. Other external tools
    /// have no index to protect and must not inherit a git-specific variable.
    #[test]
    fn optional_lock_suppression_does_not_leak_to_other_tools() {
        for cmd in [Gh::command(), Cargo::command(), Node::command()]
            .into_iter()
            .flatten()
        {
            assert!(
                !cmd.get_envs()
                    .any(|(key, _)| key == std::ffi::OsStr::new("GIT_OPTIONAL_LOCKS")),
                "only Git may set GIT_OPTIONAL_LOCKS"
            );
        }
    }

    /// The Python and Node constructors run model-authored code, so the
    /// command they build must not carry the parent's environment. This
    /// runs on every unix runner, with or without Python or Node installed.
    #[cfg(unix)]
    #[test]
    fn runtime_commands_do_not_inherit_parent_secret_env() {
        let _env_lock = codewhale_runtime::test_support::lock_test_env();
        let _secret = codewhale_runtime::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_RUNTIME_SECRET",
            "runtime-secret-value",
        );
        let output = scrubbed_command_for_spec("env").output().expect("env runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(!stdout.contains("runtime-secret-value"), "{stdout}");
        assert!(stdout.contains("PATH="), "{stdout}");

        // Both constructors of each runtime are built on the scrubbed spec,
        // which sets the sanitized environment explicitly.
        let has_explicit_path = |cmd: &Command| {
            cmd.get_envs()
                .any(|(key, value)| key == std::ffi::OsStr::new("PATH") && value.is_some())
        };
        for cmd in [Python::command(), Node::command()].into_iter().flatten() {
            assert!(has_explicit_path(&cmd), "{cmd:?}");
        }
        for cmd in [Python::tokio_command(), Node::tokio_command()]
            .into_iter()
            .flatten()
        {
            assert!(has_explicit_path(cmd.as_std()), "{cmd:?}");
        }
    }

    /// Workspace git config can make even `git status` run a program
    /// (`core.fsmonitor`); that program must not see the parent's secrets.
    #[cfg(unix)]
    #[test]
    fn git_command_does_not_inherit_parent_secret_env() {
        use std::os::unix::fs::PermissionsExt;
        if !Git::available() {
            return;
        }
        let _env_lock = codewhale_runtime::test_support::lock_test_env();
        let _secret = codewhale_runtime::test_support::EnvVarGuard::set(
            "CODEWHALE_TEST_GIT_SECRET",
            "git-secret-value",
        );
        let repo = tempfile::tempdir().expect("repo");
        let hooks = tempfile::tempdir().expect("hooks");
        let marker = hooks.path().join("seen");
        let hook = hooks.path().join("fsmonitor.sh");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf 'leak=%s\\n' \"${{CODEWHALE_TEST_GIT_SECRET-unset}}\" >> '{}'\nexit 1\n",
                marker.display()
            ),
        )
        .expect("write hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let run = |args: &[&str]| {
            let status = Git::status(args, repo.path()).expect("git spawns");
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        std::fs::write(repo.path().join("file.txt"), "hello\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        run(&["config", "core.fsmonitor", &hook.to_string_lossy()]);

        let output = Git::output(&["status", "--porcelain"], repo.path()).expect("status");
        assert!(output.status.success());
        let seen = std::fs::read_to_string(&marker).expect("fsmonitor hook ran");
        assert!(seen.contains("leak=unset"), "{seen}");
        assert!(!seen.contains("git-secret-value"), "{seen}");
    }

    #[test]
    fn python_command_returns_some_when_available() {
        if Python::available() {
            assert!(Python::command().is_some());
        }
    }

    #[test]
    fn python_tokio_command_returns_some_when_available() {
        if Python::available() {
            assert!(Python::tokio_command().is_some());
        }
    }

    #[test]
    fn node_tokio_command_returns_some_when_available() {
        if Node::available() {
            assert!(Node::tokio_command().is_some());
        }
    }

    #[test]
    fn git_review_accepts_supported_runtime_config() {
        let dir = tempfile::tempdir().unwrap();
        let output = Git::review_command(dir.path())
            .expect("native Git supports runtime overrides")
            .args(["config", "--get", "core.fsmonitor"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"false\n");
    }

    /// The child has a fresh executable/capability cache. Its wrapper ignores
    /// runtime config as older Git would; it is not an old-Git installation.
    #[cfg(unix)]
    #[test]
    fn git_review_refuses_unsupported_runtime_config() {
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "CODEWHALE_TEST_UNSUPPORTED_GIT_REVIEW";
        if let Some(repo) = std::env::var_os(CHILD) {
            let repo = PathBuf::from(repo);
            if std::env::var_os("CODEWHALE_TEST_MISSING_GIT_REVIEW").is_some() {
                let error = Git::review_command(&repo).expect_err("Git is absent");
                assert_eq!(error.to_string(), "git not found on PATH");
                return;
            }
            match Git::review_command(&repo) {
                Err(error) => assert!(
                    error
                        .to_string()
                        .contains("runtime configuration overrides are unavailable"),
                    "{error:#}"
                ),
                Ok(mut command) => {
                    let output = command.args(["status", "--porcelain"]).output().unwrap();
                    let witness = std::fs::read_to_string(repo.join("helper-seen"))
                        .unwrap_or_else(|_| "no helper witness".into());
                    panic!(
                        "unsupported Git reached repository read: status={:?}; {witness}",
                        output.status
                    );
                }
            }
            assert!(!repo.join("helper-seen").exists());
            return;
        }
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let real_git = resolve_executable_path("git", "--version").expect("absolute native Git");
        let repo = tempfile::tempdir().unwrap();
        let wrapper_dir = tempfile::tempdir().unwrap();
        let helper = repo.path().join("fsmonitor.sh");
        let marker = repo.path().join("helper-seen");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf 'unsupported-runtime-config-helper\\n' >> '{}'\nexit 1\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "core.fsmonitor", helper.to_str().unwrap()],
            // A static capability marker would falsely accept this repository.
            vec!["config", "codewhale.reviewConfigCapability", "supported"],
        ] {
            let output = Git::output(&args, repo.path()).unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        let wrapper = wrapper_dir.path().join("git");
        let quoted_git = real_git.replace('\'', "'\\''");
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\nunset GIT_CONFIG_COUNT\nexec '{quoted_git}' \"$@\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dependencies::tests::git_review_refuses_unsupported_runtime_config",
                "--nocapture",
            ])
            .env(CHILD, repo.path())
            .env("PATH", wrapper_dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "emulated unsupported Git refusal failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!marker.exists());
        let missing_path = tempfile::tempdir().unwrap();
        let missing = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dependencies::tests::git_review_refuses_unsupported_runtime_config",
                "--nocapture",
            ])
            .env(CHILD, repo.path())
            .env("CODEWHALE_TEST_MISSING_GIT_REVIEW", "1")
            .env("PATH", missing_path.path())
            .output()
            .unwrap();
        assert!(
            missing.status.success(),
            "missing Git diagnostic failed: {}\n{}",
            String::from_utf8_lossy(&missing.stdout),
            String::from_utf8_lossy(&missing.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_review_keeps_checked_executable_after_path_changes() {
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "CODEWHALE_TEST_GIT_REVIEW_PATH_CHANGE";
        if let Some(repo) = std::env::var_os(CHILD) {
            let _lock = codewhale_runtime::test_support::lock_test_env();
            let repo = PathBuf::from(repo);
            let initial = Git::review_command(&repo)
                .unwrap()
                .args(["config", "--get", "core.fsmonitor"])
                .output()
                .unwrap();
            assert!(initial.status.success());
            assert_eq!(initial.stdout, b"false\n");
            let changed = std::env::var_os("CODEWHALE_TEST_CHANGED_GIT_PATH").unwrap();
            let _path = codewhale_runtime::test_support::EnvVarGuard::set("PATH", changed);
            let output = Git::review_command(&repo)
                .unwrap()
                .args(["status", "--porcelain"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let marker = repo.join("path-change-helper-seen");
            assert!(
                !marker.exists(),
                "cached review switched to unchecked Git: {}",
                std::fs::read_to_string(marker).unwrap_or_default()
            );
            return;
        }
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let real_git = resolve_executable_path("git", "--version").expect("absolute native Git");
        let quoted_git = real_git.replace('\'', "'\\''");
        let repo = tempfile::tempdir().unwrap();
        let original_path = tempfile::tempdir().unwrap();
        let supported_git_dir = original_path.path().join("Git path with spaces");
        std::fs::create_dir(&supported_git_dir).unwrap();
        let changed_path = tempfile::tempdir().unwrap();
        for (dir, prefix) in [
            (supported_git_dir.as_path(), ""),
            (changed_path.path(), "unset GIT_CONFIG_COUNT\n"),
        ] {
            let script = dir.join("git");
            std::fs::write(
                &script,
                format!("#!/bin/sh\n{prefix}exec '{quoted_git}' \"$@\"\n"),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let helper = repo.path().join("fsmonitor.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf 'path-change-fixture-helper\\n' >> '{}'\nexit 1\n",
                repo.path().join("path-change-helper-seen").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "core.fsmonitor", helper.to_str().unwrap()],
        ] {
            assert!(Git::output(&args, repo.path()).unwrap().status.success());
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dependencies::tests::git_review_keeps_checked_executable_after_path_changes",
                "--nocapture",
            ])
            .env(CHILD, repo.path())
            .env("CODEWHALE_TEST_CHANGED_GIT_PATH", changed_path.path())
            .env("PATH", &supported_git_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native PATH-switch review fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!repo.path().join("path-change-helper-seen").exists());
    }

    #[test]
    fn git_output_version_succeeds() {
        // Only run when git is actually installed.
        if !Git::available() {
            return;
        }
        let tmp = std::env::temp_dir();
        let out = Git::output(&["--version"], &tmp);
        assert!(
            out.is_ok(),
            "git --version must succeed when git is available"
        );
        let out = out.unwrap();
        assert!(out.status.success(), "git --version must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("git version"),
            "git --version stdout must contain 'git version', got: {}",
            stdout.trim()
        );
    }

    #[test]
    fn python_output_version_succeeds() {
        if !Python::available() {
            return;
        }
        let tmp = std::env::temp_dir();
        let out = Python::output(&["--version"], &tmp);
        assert!(out.is_ok(), "python --version must spawn");
        let out = out.unwrap();
        // Python --version writes to stdout on 3.x, so just check
        // that it succeeded (exit 0).
        assert!(out.status.success(), "python --version must exit 0");
    }

    #[test]
    fn node_output_version_succeeds() {
        if !Node::available() {
            return;
        }
        let tmp = std::env::temp_dir();
        let out = Node::output(&["--version"], &tmp);
        assert!(out.is_ok(), "node --version must spawn");
        let out = out.unwrap();
        assert!(out.status.success(), "node --version must exit 0");
    }

    #[test]
    fn cargo_output_version_succeeds() {
        if !Cargo::available() {
            return;
        }
        let tmp = std::env::temp_dir();
        let out = Cargo::output(&["--version"], &tmp);
        assert!(out.is_ok(), "cargo --version must spawn");
        let out = out.unwrap();
        assert!(out.status.success(), "cargo --version must exit 0");
    }

    #[test]
    fn external_tool_output_respects_cwd() {
        // Verify that `output()` runs in the requested directory.
        if !Git::available() {
            return;
        }
        let tmp = std::env::temp_dir();
        let out = Git::output(&["rev-parse", "--show-toplevel"], &tmp);
        assert!(out.is_ok(), "git rev-parse must spawn");
        let out = out.unwrap();
        // rev-parse --show-toplevel in a non-git dir should fail
        // because temp_dir is not a git repo. That's expected.
        // The key assertion: the command executed without IO errors.
        // We don't assert success because temp_dir might or might not
        // be inside a git worktree.
        let _ = out; // just checking it didn't panic/IO-error
    }
}

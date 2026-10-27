//! One extension-host process: launch plan, spawn, handshake, channel, exit.
//!
//! When the process exits, every in-flight call fails with a typed error.
//! The existing Rust manager owns heartbeat, crash budget, generation changes,
//! and receipt-checked replay; this channel never replays a tool call.
//!
//! **Host-originated requests** run as tasks, not inline in the reader. Each
//! request the host sends is admitted into [`InboundRequests`] (at most
//! [`protocol::MAX_INFLIGHT`] at a time, no id twice, both violations end the
//! host) and handed to [`HostEvents::host_request`] as its own task. It is
//! cancelled by the host's `$/cancel` (whatever the handler later produces is
//! dropped), by its owner's revocation (the host is answered `Cancelled`) and
//! by the host's exit (nobody is answered); a handler that ignores its token is
//! abandoned [`CANCEL_GRACE`] after the cancel. A method reserved for the other
//! tier is neither accepted from the host nor sent to it
//! (`protocol::MethodSpec::tiers`), and `host/hello` must report the tier the
//! core launched and the built-in module digests it pins
//! ([`check_hello_identity`]).
//!
//! **Tiers** ([`HostTier`]). The host is two processes, one per trust tier,
//! started from the same bundle with `--tier=plugin|builtin` as the last
//! argument. Each has its own data directory ([`tier_data_dir`]) and its own
//! sandbox plan; Native cannot read the builtin tier's data directory.
//! Builtin backends are activated separately by the Engine.
//!
//! **OS sandbox** ([`HostSandbox`]). Native hosts use a restricted positive
//! filesystem policy: selected runtime and its libraries, the canonical host
//! bundle, Core-managed `.runtime/v2` package snapshots, and shared Native data.
//! Writes are limited to Native data and private scratch. Project files,
//! including `.env`, and unrelated user files are not granted. This remains
//! one trusted Native cohort, not isolation between plugins or their data.
//! Builtin keeps its existing separately diagnosed command-sandbox policy.
//! * macOS: Seatbelt with explicit reads and no toolchain cache write grants.
//! * Linux: bubblewrap starts an empty filesystem and binds explicit grants.
//!   A finished `<runtime> --version` probe must succeed; missing bubblewrap
//!   or blocked user namespaces refuses Native launch. Its private PID/network
//!   namespaces and `--die-with-parent` contain descendants and lifetime.
//! * Windows: Native keeps its LPAC AppContainer and Job. Its verified
//!   runtime/bundle/staged-root ACL grants and startup allow/deny probe remain
//!   separate from the Unix profile. Builtin retains its diagnosed exception.
//!
//! Snapshot stores are fixed at Unix host launch. A new store requires a
//! restart; every owner admission verifies current authority and containment
//! in the captured store roots. Snapshot contents remain subject to the
//! existing immutable staging and per-component receipt checks.
//!
//! **Runtime.** Node (the default) or Bun (opt-in: `runtime = "bun"`, or
//! `"auto"`, which prefers a supported Bun), chosen once per manager
//! (`[extension_host] runtime`). Each gets its own flags ([`runtime_args`])
//! and environment ([`runtime_env`]); Bun silently ignores Node's heap and
//! `__proto__` flags. The host itself takes in-process native code away from
//! plugins (`extension-host/src/runtime.ts`: `bun:ffi`, `Bun.FFI`, SQLite
//! extension loading, Worker threads, ShadowRealm, `process.dlopen`) and
//! refuses to start if a lock does not hold. That lockdown covers the entry
//! points found so far (Bun 1.4, Node 22 and 26), not every one a runtime
//! may add.
//!
//! **Memory cap** ([`MemoryEnforcement`]). What was measured where: the
//! macOS mechanism on macOS 26.1 arm64 (2026-09-30, Bun 1.4.0 and Node);
//! the Linux thresholds in [`HOST_MEMORY_CAP`] in a Linux container
//! (2026-09-29). Hosted CI runs the Rust memory-cap test on Linux, macOS and
//! Windows with Node only; the Rust host tests have not run a Bun host on
//! Linux or Windows (CI's JS host suites run under Bun 1.4.0 on Linux).
//! * Linux: `RLIMIT_DATA`, set in the child before exec (clamped to an
//!   inherited hard limit that is already lower); an allocation past the cap
//!   fails. Plugin child processes inherit it.
//! * Windows: the Job Object's per-process limit; an allocation past the cap
//!   fails. It applies to each process in the job, plugin children included.
//! * macOS: `setrlimit(RLIMIT_AS/RLIMIT_DATA)` below the current mapping size
//!   fails with `EINVAL`, and `memorystatus_control` needs privilege. A fatal
//!   jetsam limit set as a `posix_spawn` attribute works unprivileged, but a
//!   later `exec` clears it, so it cannot be set on `sandbox-exec`. The Bun
//!   host therefore re-executes itself in place with the limit before any
//!   plugin loads, and reports it in `host/hello`; past the cap the kernel
//!   SIGKILLs it. Plugin child processes are not covered. A Bun host that
//!   cannot apply the requested limit is refused before initialization. Node
//!   has no FFI to do the same, so a Node host is checked at each heartbeat
//!   instead: enforced only as often as
//!   the heartbeat runs. Node also keeps `--max-old-space-size=256`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::tools::codemode::PauseClock;
use codewhale_dependencies::{HostRuntime, HostRuntimeKind};

use super::protocol::{
    self, CoreRequest, Direction, HelloParams, HostLimits, HostMessage, HostNotification,
    HostRequest, InitializeParams, RegisterResult, RpcErrorWire, error_code,
};
use super::tier::{self, BuiltinModule, HostTier};

/// Budget for spawn → `host/hello` → `host/initialize` → `host/ready`.
///
/// A warm start takes well under 100 ms, but the first start of a freshly
/// materialized bundle pays for a cold `node` launch, and on Windows for an
/// on-access antivirus scan of both. Windows CI under full test load missed
/// the former 5 s budget with the host silent on stderr (four runs on
/// 2026-09-29) while the same tests normally finish in about 1 s.
/// A miss is sticky: the host is marked failed until the next session, so
/// a too-tight budget disables every extension for that session. The
/// handshake runs in the background, off the first-prompt path, so a wider
/// budget costs nothing when the host is healthy; 30 s matches the MCP stdio
/// handshake; it is independent of the active TUI MCP client.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);
pub const ACTIVATE_DEADLINE: Duration = Duration::from_secs(5);
pub const DISPOSE_DEADLINE: Duration = Duration::from_secs(2);
/// A `host/ping` answer later than this means a hung host: the heartbeat's
/// default `hang_timeout`, and the bound for any other ping.
pub const PING_DEADLINE: Duration = Duration::from_secs(10);
/// Grace between `$/cancel` and resolving a call as cancelled on this side.
pub const CANCEL_GRACE: Duration = Duration::from_millis(500);
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const OUTBOUND_QUEUE: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostCallError {
    #[error("{message} (extension error {code})")]
    Rpc { code: i64, message: String },
    #[error("extension host exited: {0}")]
    Exited(String),
    #[error("cancelled: {0}")]
    Cancelled(String),
    #[error("`{method}` timed out after {after:?}; the host was told to cancel it")]
    Timeout {
        method: &'static str,
        after: Duration,
    },
    #[error("extension host channel is full")]
    Busy,
}

/// How the host is started: the argv (wrapped by the OS sandbox when one is
/// available), its working directory, and which sandbox applies.
#[derive(Debug, Clone)]
pub(crate) struct HostLaunch {
    /// The trust tier this host process serves (`--tier=` in its argv).
    pub tier: HostTier,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox: HostSandbox,
    /// Environment the sandbox wrapper adds (`CODEWHALE_SANDBOX`, …).
    pub sandbox_env: Vec<(String, String)>,
    /// The runtime inside the wrapper; `host/hello` must report the same one.
    pub runtime: HostRuntime,
    /// Environment the runtime needs ([`runtime_env`]).
    pub runtime_env: Vec<(String, String)>,
    /// Bytes; see the module docs for how each platform enforces it.
    pub memory_cap: u64,
    /// How the cap is meant to be enforced; a macOS Bun host confirms its
    /// jetsam limit in `host/hello` or initialization is refused.
    pub memory: MemoryEnforcement,
    /// The built-in module digests `host/hello` must report: the table this
    /// build pins ([`tier::BUILTIN_MODULES`]), which the host bundle embedded
    /// from the same build must agree with ([`check_hello_identity`]).
    pub builtin_modules: &'static [BuiltinModule],
    pub native_snapshot_roots: Vec<PathBuf>,
    /// Exact verified Native LPAC plan; Builtin retains its labelled exception.
    #[cfg(windows)]
    pub windows: Option<super::windows::NativeSandbox>,
}

/// Whether the host runs under an OS sandbox, and why not when it does not
/// (module docs). Settled by [`plan_launch`]; never inferred afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSandbox {
    /// Under this wrapper, with a Native-specific suffix for the restricted
    /// Unix profile (`macos-seatbelt-native`, `linux-bwrap-native`).
    Wrapped(String),
    /// With the user's permissions, for this reason.
    Unsandboxed(String),
}

impl HostSandbox {
    /// The wrapper's name, or `none: <reason>`, for one-line diagnostics.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Wrapped(name) => name.clone(),
            Self::Unsandboxed(reason) => format!("none: {reason}"),
        }
    }
}

/// What `/plugin` and doctor say about the sandbox.
impl std::fmt::Display for HostSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wrapped(name) if name == "windows-lpac" => write!(
                f,
                "windows-lpac sandbox (no direct network; reads only the selected runtime, canonical bundle and reviewed staged code; writes to Native data and platform-private scratch; Job limits lifetime/memory)"
            ),
            Self::Wrapped(name) if name.ends_with("-native") => write!(
                f,
                "{name} sandbox (experimental shared Native tier; reads only the host runtime, system libraries, staged package snapshots and Native data; writes only Native data and private scratch; no direct network or repository access)"
            ),
            Self::Wrapped(name) => write!(
                f,
                "{name} sandbox (no direct network; the Codewhale home except plugin code, the Codex and DSH credential homes and the default credential stores are unreadable; other files you can read, such as project .env files, are not protected)"
            ),
            Self::Unsandboxed(reason) => write!(
                f,
                "UNSANDBOXED ({reason}): host code runs with your user permissions"
            ),
        }
    }
}

/// Environment variable carrying the jetsam limit a macOS Bun host applies to
/// itself, in MiB (`extension-host/src/runtime.ts`, `applyMemoryLimit`).
pub(crate) const MEMORY_LIMIT_REQUEST_ENV: &str = "CODEWHALE_HOST_MEMORY_LIMIT_MIB";

/// How the host's memory cap is enforced (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEnforcement {
    /// Linux: kernel `RLIMIT_DATA`.
    Rlimit,
    /// Windows: the Job Object's per-process memory limit.
    JobObject,
    /// macOS + Bun: a fatal jetsam limit the host applied to itself.
    Jetsam,
    /// macOS otherwise: resident size checked at each heartbeat.
    Heartbeat,
    /// No cap on this platform.
    Unenforced,
}

impl MemoryEnforcement {
    /// What this platform does for `kind`, before the host has confirmed it.
    #[must_use]
    pub fn planned(kind: HostRuntimeKind) -> Self {
        if cfg!(target_os = "linux") {
            Self::Rlimit
        } else if cfg!(windows) {
            Self::JobObject
        } else if cfg!(target_os = "macos") {
            match kind {
                HostRuntimeKind::Bun => Self::Jetsam,
                HostRuntimeKind::Node => Self::Heartbeat,
            }
        } else {
            Self::Unenforced
        }
    }

    /// One line for `/plugin` and doctor.
    #[must_use]
    pub fn describe(self, cap: u64) -> String {
        let mib = cap / (1024 * 1024);
        match self {
            Self::Rlimit => format!(
                "memory cap {mib} MiB (kernel RLIMIT_DATA; plugin child processes inherit it)"
            ),
            Self::JobObject => format!(
                "memory cap {mib} MiB (Job Object per-process limit; plugin child processes included)"
            ),
            Self::Jetsam => format!(
                "memory cap {mib} MiB (kernel jetsam limit; the host is killed past it; plugin child processes are not covered)"
            ),
            Self::Heartbeat => format!(
                "memory cap {mib} MiB (resident size checked at each heartbeat; on macOS only the Bun host gets a kernel limit)"
            ),
            Self::Unenforced => "no memory cap on this platform".to_string(),
        }
    }
}

/// The default memory cap. Measured once in a Linux container (2026-09-29),
/// not in CI: under `RLIMIT_DATA` Node 24 aborts when it creates the host's
/// watchdog Worker at 512 MiB, and Bun 1.4 aborts at startup at 256 MiB.
/// Both started and ran at 1 GiB and failed an allocation past it. An idle
/// host used 34–67 MB resident. On hosted x64 Linux (Node 24.21) each V8
/// isolate's executable code range is charged to `RLIMIT_DATA` in full, and
/// a default-sized second isolate aborted the host at this cap; the
/// watchdog Worker therefore asks for a 16 MiB code range (`src/main.ts`).
/// Plugins cannot start Workers (`denyNativeCode`), so it is the only one.
pub const HOST_MEMORY_CAP: u64 = 1 << 30;

/// Runtime flags, before the bundle path. Keep in sync with
/// `extension-host/test/harness.mjs` (`HOST_ARGS`).
///
/// - Node: a 256 MB old-space cap, `__proto__` throws, no native addons,
///   and the [`codewhale_dependencies::NODE_NATIVE_CODE_FLAGS`] this Node
///   accepts (`node:sqlite`, and `node:ffi` where it exists). On Windows it
///   also keeps symlinked paths as given, so module resolution never lstats
///   the drive root the LPAC cannot read.
/// - Bun ignores all of those except `--no-addons`. Instead it gets
///   `--no-install`, because Bun otherwise fetches a missing package from npm
///   while plugin code is running. It also gets `--no-env-file` and
///   `--config=<null device>`, because Bun otherwise loads `.env` and
///   `bunfig.toml` (which can preload code) from the working directory, and
///   that directory is the host's writable data dir. `bun:ffi` and the rest
///   are locked in the host itself (`src/runtime.ts`).
#[must_use]
pub(crate) fn runtime_args(runtime: &HostRuntime) -> Vec<String> {
    if runtime.compiled {
        return Vec::new();
    }
    base_runtime_args(runtime.kind)
        .iter()
        .chain(&runtime.native_code_flags)
        .map(|arg| (*arg).to_string())
        .collect()
}

/// Environment for the runtime. Keep in sync with `HOST_ENV` in
/// `extension-host/test/harness.mjs`.
///
/// - Both: `NODE_OPTIONS` is blanked. The child-environment allowlist passes
///   the user's value through (shell tools need it), and a `--require` or
///   `--import` preload there would run before the host's native-code
///   lockdown. Node honours it; Bun 1.4.0 ignored a `--require` in it when
///   checked (2026-09-30), and it is blanked for Bun too in case a later
///   Bun does not. `BUN_OPTIONS`, which Bun does honour, is not in that
///   allowlist.
/// - Bun: no ShadowRealm, engine-wide (a realm imports a fresh `bun:ffi`;
///   `node:vm` contexts would otherwise hand the constructor out). The host
///   refuses to start without it.
#[must_use]
pub(crate) fn runtime_env(kind: HostRuntimeKind) -> Vec<(String, String)> {
    let mut env = vec![("NODE_OPTIONS".to_string(), String::new())];
    if kind == HostRuntimeKind::Bun {
        env.push(("BUN_JSC_useShadowRealm".to_string(), "0".to_string()));
        env.push(("BUN_OPTIONS".to_string(), String::new()));
        // This variable otherwise makes a standalone image act as the Bun CLI.
        env.push(("BUN_BE_BUN".to_string(), "0".to_string()));
    }
    env
}

fn base_runtime_args(kind: HostRuntimeKind) -> &'static [&'static str] {
    match kind {
        #[cfg(not(windows))]
        HostRuntimeKind::Node => &[
            "--max-old-space-size=256",
            "--disable-proto=throw",
            "--no-addons",
        ],
        // Node resolves the entry bundle with realpathSync, which lstats every
        // ancestor from `C:\`. A Windows LPAC cannot read the drive root, so
        // the host died before its handshake (EPERM, lstat 'C:\'). Keep the
        // granted path as given; the LPAC still decides every access.
        #[cfg(windows)]
        HostRuntimeKind::Node => &[
            "--max-old-space-size=256",
            "--disable-proto=throw",
            "--no-addons",
            "--preserve-symlinks",
            "--preserve-symlinks-main",
        ],
        #[cfg(windows)]
        HostRuntimeKind::Bun => &[
            "--no-install",
            "--no-env-file",
            "--config=NUL",
            "--no-addons",
        ],
        #[cfg(not(windows))]
        HostRuntimeKind::Bun => &[
            "--no-install",
            "--no-env-file",
            "--config=/dev/null",
            "--no-addons",
        ],
    }
}

/// Top-level entries of a Codewhale home the host may read: its own bundle
/// and data (`extension-host`), and plugin code (the staged snapshots live
/// under `plugins/.runtime`). Everything else in a Codewhale home — secrets,
/// tokens, config and its backups, sessions, state, tool outputs, history —
/// is denied.
const HOST_READABLE_HOME_ENTRIES: &[&str] = &["extension-host", "plugins", "builtin-plugins"];

/// Codewhale-home entries denied by name even before they exist, so a store
/// created after the host started is still covered. Existing entries are
/// denied by enumeration (`host_denied_read_paths`).
const HOST_DENIED_HOME_ENTRIES: &[&str] = &[
    "secrets",
    "credentials",
    "tokens",
    "state",
    "state.db",
    "sessions",
    "session-archives",
    "session_index.jsonl",
    "tool_outputs",
    "composer_history.txt",
    "composer_history.jsonl",
    "remote-control",
    "integrations",
    "audit.log",
    "logs",
    "memory",
    "mcp.json",
    "mcp.json.bak",
    "config.toml.bak",
    "settings.toml",
];

/// Paths the host process must never read, even though the sandbox otherwise
/// grants full-disk read, and the exceptions inside them: the curated
/// credential-store defaults; Codewhale's homes (the runtime home, the ambient
/// `~/.codewhale`, and the legacy `~/.deepseek`) except
/// [`HOST_READABLE_HOME_ENTRIES`]; and the Codex and DSH homes whose
/// credential files Codewhale itself reads. Blocking.
///
/// With `whole_homes` (bubblewrap, which can mask only what exists) each home
/// is denied whole and its readable entries come back as exceptions to bind
/// again. Without it (Seatbelt, which matches paths that do not exist yet)
/// every other entry is denied by name and there are no exceptions.
///
/// The plugin tier also denies the builtin tier's data directory, which lies
/// inside the readable `extension-host/` entry: plugin code never reads
/// tier-0 state. Planning materializes that sibling before the wrapper masks
/// it, because bubblewrap cannot mask a directory that does not exist yet.
pub(crate) fn host_denied_read_paths(
    tier: HostTier,
    home: &Path,
    whole_homes: bool,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut paths =
        codewhale_sandbox::read_guard::ReadDenylist::build(true, &[], &[]).subtree_paths();
    let mut exceptions: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    let user_home = codewhale_paths::user_home();
    let mut roots = vec![home.to_path_buf()];
    roots.extend(codewhale_config::codewhale_home().ok());
    if let Some(user) = &user_home {
        roots.push(user.join(codewhale_config::CODEWHALE_APP_DIR));
        roots.push(user.join(".deepseek"));
    }
    for root in roots {
        // The builtin tier's data directory, inside the readable
        // `extension-host/` entry of every Codewhale home: not for plugin code.
        let tier_zero_data =
            (tier == HostTier::Plugin).then(|| tier_data_dir(&root, HostTier::Builtin));
        if whole_homes {
            for entry in HOST_READABLE_HOME_ENTRIES {
                let readable = root.join(entry);
                if !exceptions.contains(&readable) {
                    exceptions.push(readable);
                }
            }
            if let Some(dir) = tier_zero_data {
                push(dir);
            }
            push(root);
            continue;
        }
        let mut names: Vec<std::ffi::OsString> = HOST_DENIED_HOME_ENTRIES
            .iter()
            .chain(std::iter::once(&codewhale_config::CONFIG_FILE_NAME))
            .map(std::ffi::OsString::from)
            .collect();
        if let Ok(entries) = std::fs::read_dir(&root) {
            names.extend(
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name()),
            );
        }
        // Seatbelt matches the kernel-resolved path, and a name that does not
        // exist yet cannot be canonicalized later, so deny it under both the
        // given and the resolved spelling of its (existing) root.
        let resolved = std::fs::canonicalize(&root).ok();
        if let Some(dir) = tier_zero_data {
            if let Some(resolved) = &resolved {
                push(tier_data_dir(resolved, HostTier::Builtin));
            }
            push(dir);
        }
        for name in names {
            let readable = name
                .to_str()
                .is_some_and(|name| HOST_READABLE_HOME_ENTRIES.contains(&name));
            if !readable {
                if let Some(resolved) = &resolved {
                    push(resolved.join(&name));
                }
                push(root.join(name));
            }
        }
    }
    // Codex's home (ChatGPT OAuth tokens in `auth.json`) and the DSH home
    // (`.credentials.yaml`), wherever the environment points them.
    if let Some(codex_home) = codewhale_runtime::oauth::auth_file_path().parent() {
        push(codex_home.to_path_buf());
    }
    if let Some(user) = &user_home {
        push(user.join(".codex"));
        push(user.join(".dsh"));
    }
    if let Some(dsh_home) = codewhale_config::default_dsh_credentials_path().parent() {
        push(dsh_home.to_path_buf());
    }
    (paths, exceptions)
}

/// Plan the host launch for `tier`: the runtime's flags, the bundle, then the
/// tier (`--tier=plugin|builtin`, which the host reads from its own argv).
/// Blocking (creates the tier's data dir, canonicalizes the deny-list, and on
/// Linux runs the bwrap probe); call from `spawn_blocking`.
pub(crate) fn plan_launch(
    tier: HostTier,
    runtime: &HostRuntime,
    bundle: &Path,
    home: &Path,
    memory_cap: u64,
    snapshot_state_paths: &[PathBuf],
) -> Result<HostLaunch, String> {
    let data = host_data_dir(home, tier)?;
    // Bubblewrap cannot mask a root that does not exist yet. Materialize the
    // sibling before a Native host gets its immutable read-deny projection.
    if tier == HostTier::Plugin {
        host_data_dir(home, HostTier::Builtin)?;
    }
    let mut args = runtime_args(runtime);
    if !runtime.compiled {
        args.push(bundle.to_string_lossy().into_owned());
    }
    args.push(tier.argv_flag());
    #[cfg(windows)]
    if tier == HostTier::Plugin {
        let sandbox =
            super::windows::NativeSandbox::prepare(runtime, bundle, home, &data, memory_cap)?;
        return Ok(HostLaunch {
            tier,
            program: sandbox.program.clone(),
            args,
            cwd: data,
            sandbox: HostSandbox::Wrapped("windows-lpac".into()),
            sandbox_env: vec![("CODEWHALE_SANDBOX".into(), "windows-lpac".into())],
            runtime: runtime.clone(),
            runtime_env: runtime_env(runtime.kind),
            memory_cap,
            memory: MemoryEnforcement::JobObject,
            builtin_modules: tier::BUILTIN_MODULES,
            native_snapshot_roots: Vec::new(),
            windows: Some(sandbox),
        });
    }
    let snapshot_roots = if tier == HostTier::Plugin {
        native_snapshot_roots(snapshot_state_paths)?
    } else {
        Vec::new()
    };
    let wrapped = wrap_host(
        tier,
        &runtime.path,
        &args,
        &data,
        home,
        bundle,
        &snapshot_roots,
    );
    let mut launch = host_launch(tier, runtime, args, data, memory_cap, wrapped)?;
    launch.native_snapshot_roots = snapshot_roots;
    Ok(launch)
}

/// The runtime sandbox a `tier` host would get, planned (and on Linux probed)
/// as [`plan_launch`] does, without package-store grants, for doctor.
/// Blocking; creates the data dir, as a launch would.
pub(crate) fn planned_sandbox(
    tier: HostTier,
    runtime: &HostRuntime,
    home: &Path,
) -> Result<HostSandbox, String> {
    let data = host_data_dir(home, tier)?;
    if tier == HostTier::Plugin {
        host_data_dir(home, HostTier::Builtin)?;
    }
    #[cfg(windows)]
    if tier == HostTier::Plugin {
        let bundle = super::materialize_bundle(home)?;
        return Ok(
            match super::windows::NativeSandbox::prepare(
                runtime,
                &bundle,
                home,
                &data,
                HOST_MEMORY_CAP,
            ) {
                Ok(_) => HostSandbox::Wrapped("windows-lpac".into()),
                Err(error) => HostSandbox::Unsandboxed(error),
            },
        );
    }
    let bundle = if tier == HostTier::Plugin {
        super::materialize_bundle(home)?
    } else {
        PathBuf::new()
    };
    Ok(
        match wrap_host(
            tier,
            &runtime.path,
            &runtime_args(runtime),
            &data,
            home,
            &bundle,
            &[],
        ) {
            Ok(wrapped) => HostSandbox::Wrapped(wrapped.name),
            Err(reason) => HostSandbox::Unsandboxed(reason),
        },
    )
}

/// A tier's data directory: its host's working directory and, under the OS
/// sandbox, its only writable root. Pure.
///
/// The plugin tier keeps the directory it has always had,
/// `extension-host/data`, because the per-plugin directories under it
/// ([`plugin_data_dir`]) hold installed plugins' data and moving it would lose
/// that. The builtin tier's is a sibling, `extension-host/data-builtin`, not a
/// child: a child would lie inside the plugin tier's writable root, and the
/// host sandbox has no per-subpath write deny, so plugin code could then write
/// tier-0 state.
pub(crate) fn tier_data_dir(home: &Path, tier: HostTier) -> PathBuf {
    let base = home.join("extension-host");
    match tier {
        HostTier::Plugin => base.join("data"),
        HostTier::Builtin => base.join("data-builtin"),
    }
}

/// The directory an owner's code is given as its own: a plugin's
/// ([`plugin_data_dir`], unchanged), or `modules/<module>` under the builtin
/// tier's data directory for a built-in module (`plugin_name` is the module
/// name, which [`HostTier::check_owner_id`] has restricted to a plain name).
/// Pure.
pub(crate) fn owner_data_dir(
    home: &Path,
    tier: HostTier,
    plugin_id: &str,
    plugin_name: &str,
) -> PathBuf {
    match tier {
        HostTier::Plugin => plugin_data_dir(home, plugin_id, plugin_name),
        HostTier::Builtin => tier_data_dir(home, tier).join("modules").join(plugin_name),
    }
}

/// One plugin's own directory inside the host's data dir, which is the host
/// sandbox's writable root: stable for one plugin id (so it survives updates
/// and restarts), distinct per plugin, and a single path component under
/// `plugins/`. The id contains slashes and the name alone can collide between
/// scopes, so the name is joined to a digest of the id. Pure.
pub(crate) fn plugin_data_dir(home: &Path, plugin_id: &str, plugin_name: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(plugin_id.as_bytes());
    let short: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let name: String = plugin_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    home.join("extension-host")
        .join("data")
        .join("plugins")
        .join(format!("{name}-{short}"))
}

fn host_data_dir(home: &Path, tier: HostTier) -> Result<PathBuf, String> {
    let data = tier_data_dir(home, tier);
    std::fs::create_dir_all(&data)
        .map_err(|error| format!("cannot create {}: {error}", data.display()))?;
    Ok(data)
}

/// A host command wrapped in an OS sandbox ([`wrap_host`]).
#[derive(Debug)]
struct Wrapped {
    /// `sandbox::SandboxType`'s name for the wrapper.
    name: String,
    /// The wrapper's argv, ending with the runtime's.
    command: Vec<String>,
    /// Environment the wrapper adds (`CODEWHALE_SANDBOX`, …).
    env: Vec<(String, String)>,
}

/// Why the host runs on Windows without an OS sandbox.
const WINDOWS_UNSANDBOXED: &str = "Windows has no host sandbox yet; its Job Object contains the process tree, which is not isolation";

fn native_snapshot_roots(state_paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for state in state_paths {
        let parent = state.parent().ok_or("plugin state path has no parent")?;
        let root = parent.join(".runtime/v2");
        if !root.exists() {
            continue;
        }
        crate::plugins::registry::ensure_private_plugin_state_directory(parent)?;
        let mut path = parent.canonicalize().map_err(|error| error.to_string())?;
        for component in [".runtime", "v2"] {
            path.push(component);
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(
                    "Native snapshot store must be a directory without symbolic links".into(),
                );
            }
        }
        let canonical = path.canonicalize().map_err(|error| error.to_string())?;
        if canonical != path {
            return Err("Native snapshot store changed its canonical path".into());
        }
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    Ok(roots)
}

fn native_readable_paths(
    program: &Path,
    bundle: &Path,
    data: &Path,
    snapshot_roots: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    for path in [data, data.parent().ok_or("Native data path has no parent")?] {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Native data directory must not traverse symbolic links".into());
        }
    }
    let mut paths = vec![
        program.to_path_buf(),
        bundle.to_path_buf(),
        data.to_path_buf(),
    ];
    paths.extend_from_slice(snapshot_roots);
    let canonical_data = data.canonicalize().map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    {
        paths.extend(native_runtime_libraries(program)?);
        for path in [
            "/System/Library",
            "/usr/lib",
            "/Library/Apple/System/Library",
            "/private/var/db/dyld",
            "/usr/share/icu",
            "/usr/share/zoneinfo",
            "/private/etc/localtime",
            "/dev/null",
            "/dev/random",
            "/dev/urandom",
        ] {
            if Path::new(path).exists() {
                paths.push(PathBuf::from(path));
            }
        }
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    for path in [
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/usr/share/zoneinfo",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/localtime",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
    ] {
        if Path::new(path).exists() {
            paths.push(PathBuf::from(path));
        }
    }
    let mut canonical = Vec::new();
    for path in &paths {
        let resolved = path.canonicalize().map_err(|error| {
            format!(
                "Native read grant {} is unavailable: {error}",
                path.display()
            )
        })?;
        if path != data
            && (resolved.starts_with(&canonical_data) || canonical_data.starts_with(&resolved))
        {
            return Err("Native writable data overlaps a runtime or package read grant".into());
        }
        canonical.push(resolved);
        if path.is_file() {
            if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                canonical.push(
                    parent
                        .canonicalize()
                        .map_err(|error| error.to_string())?
                        .join(name),
                );
            }
        }
    }
    paths.extend(canonical);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(target_os = "macos")]
fn native_runtime_libraries(program: &Path) -> Result<Vec<PathBuf>, String> {
    use std::io::{Read as _, Seek as _};
    use wait_timeout::ChildExt as _;
    let executable = program.canonicalize().map_err(|error| error.to_string())?;
    let mut pending = vec![(executable.clone(), Vec::<PathBuf>::new())];
    let mut inspected = std::collections::BTreeSet::new();
    let mut libraries = Vec::new();
    while let Some((binary, inherited_paths)) = pending.pop() {
        if !inspected.insert(binary.clone()) {
            continue;
        }
        if inspected.len() > 256 {
            return Err("Native runtime library closure exceeds 256 files".into());
        }
        let mut output = tempfile::tempfile().map_err(|error| error.to_string())?;
        let mut child = std::process::Command::new("/usr/bin/otool")
            .args([
                "-l",
                binary.to_str().ok_or("Native runtime path is not UTF-8")?,
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(output.try_clone().map_err(|error| error.to_string())?)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Native runtime library inspection unavailable: {error}"))?;
        let status = match child.wait_timeout(Duration::from_secs(3)) {
            Ok(Some(status)) => status,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Native runtime library inspection timed out".into());
            }
        };
        if !status.success()
            || output.metadata().map_err(|error| error.to_string())?.len() > 1024 * 1024
        {
            return Err("Native runtime library inspection failed or exceeded its bound".into());
        }
        output.rewind().map_err(|error| error.to_string())?;
        let mut text = String::new();
        output
            .take(1024 * 1024)
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())?;
        let expand = |name: &str| -> Result<PathBuf, String> {
            if name == "@loader_path" {
                Ok(binary
                    .parent()
                    .ok_or("Native library has no parent")?
                    .to_path_buf())
            } else if name == "@executable_path" {
                Ok(executable
                    .parent()
                    .ok_or("Native runtime has no parent")?
                    .to_path_buf())
            } else if let Some(relative) = name.strip_prefix("@loader_path/") {
                Ok(binary
                    .parent()
                    .ok_or("Native library has no parent")?
                    .join(relative))
            } else if let Some(relative) = name.strip_prefix("@executable_path/") {
                Ok(executable
                    .parent()
                    .ok_or("Native runtime has no parent")?
                    .join(relative))
            } else if Path::new(name).is_absolute() {
                Ok(PathBuf::from(name))
            } else {
                Err(format!(
                    "Native runtime uses an unsupported library reference: {name}"
                ))
            }
        };
        let mut command = "";
        let mut run_paths = Vec::new();
        let mut dependencies = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("cmd ") {
                command = value;
            } else if command == "LC_RPATH" {
                if let Some(value) = line.strip_prefix("path ") {
                    run_paths.push(expand(value.split(" (offset ").next().unwrap_or_default())?);
                }
            } else if matches!(
                command,
                "LC_LOAD_DYLIB"
                    | "LC_LOAD_WEAK_DYLIB"
                    | "LC_REEXPORT_DYLIB"
                    | "LC_LOAD_UPWARD_DYLIB"
            ) {
                if let Some(value) = line.strip_prefix("name ") {
                    dependencies.push(value.split(" (offset ").next().unwrap_or_default());
                }
            }
        }
        run_paths.extend(inherited_paths);
        for name in dependencies {
            if name.starts_with("/usr/lib/") || name.starts_with("/System/Library/") {
                continue;
            }
            let path = if let Some(relative) = name.strip_prefix("@rpath/") {
                run_paths
                    .iter()
                    .map(|root| root.join(relative))
                    .find(|path| path.is_file())
                    .ok_or_else(|| format!("Native runtime library cannot be resolved: {name}"))?
            } else {
                expand(name)?
            };
            let canonical = path.canonicalize().map_err(|error| {
                format!(
                    "Native runtime library {} is unavailable: {error}",
                    path.display()
                )
            })?;
            if !canonical.is_file() {
                return Err("Native runtime library is not a regular file".into());
            }
            libraries.push(path);
            libraries.push(canonical.clone());
            pending.push((canonical, run_paths.clone()));
        }
    }
    Ok(libraries)
}

/// Wrap `program args` in the host's OS sandbox (module docs), or say why it
/// has none here. Blocking.
fn wrap_host(
    tier: HostTier,
    program: &Path,
    args: &[String],
    data: &Path,
    home: &Path,
    bundle: &Path,
    snapshot_roots: &[PathBuf],
) -> Result<Wrapped, String> {
    use codewhale_sandbox::{CommandSpec, SandboxManager, SandboxPolicy, SandboxType};
    if cfg!(windows) {
        return Err(WINDOWS_UNSANDBOXED.to_string());
    }
    let spec = |args: Vec<String>| {
        CommandSpec::program(
            &program.to_string_lossy(),
            args,
            data.to_path_buf(),
            Duration::ZERO,
        )
        .with_policy(SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: false,
            exclude_tmpdir: tier == HostTier::Plugin,
            exclude_slash_tmp: tier == HostTier::Plugin,
        })
    };
    let bwrap = cfg!(all(target_os = "linux", not(target_env = "ohos")));
    let mut manager = SandboxManager::with_bwrap_preference(bwrap);
    let (mut denied, exceptions) = if tier == HostTier::Plugin {
        let mut denied =
            codewhale_sandbox::read_guard::ReadDenylist::build(true, &[], &[]).subtree_paths();
        denied.push(tier_data_dir(home, HostTier::Builtin));
        (denied, Vec::new())
    } else {
        host_denied_read_paths(tier, home, bwrap)
    };
    if tier == HostTier::Plugin {
        let readable = native_readable_paths(program, bundle, data, snapshot_roots)?;
        denied.retain(|path| {
            readable
                .iter()
                .any(|grant| path.starts_with(grant) || grant.starts_with(path))
        });
        manager.set_readable_paths(readable);
    } else {
        manager.set_denied_read_exceptions(exceptions);
    }
    manager.set_denied_read_subpaths(denied);
    let env = manager.prepare(&spec(args.to_vec()));
    if matches!(env.sandbox_type, SandboxType::None) {
        return Err(no_wrapper_reason());
    }
    let mut environment: Vec<_> = env.env.into_iter().collect();
    if tier == HostTier::Plugin {
        let scratch = data.join("tmp");
        crate::plugins::registry::ensure_private_plugin_state_directory(&scratch)?;
        for name in ["TMPDIR", "TMP", "TEMP"] {
            environment.push((name.into(), scratch.to_string_lossy().into_owned()));
        }
        environment.push(("HOME".into(), data.to_string_lossy().into_owned()));
        environment.push(("OPENSSL_CONF".into(), "/dev/null".into()));
    }
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    if matches!(env.sandbox_type, SandboxType::LinuxBubblewrap) {
        probe_bwrap(
            &manager
                .prepare(&spec(vec!["--version".to_string()]))
                .command,
            data,
            if tier == HostTier::Plugin {
                &environment
            } else {
                &[]
            },
        )?;
    }
    Ok(Wrapped {
        name: if tier == HostTier::Plugin {
            format!("{}-native", env.sandbox_type)
        } else {
            env.sandbox_type.to_string()
        },
        command: env.command,
        env: environment,
    })
}

/// The launch for a [`wrap_host`] outcome: the wrapper's argv, or the
/// pinned Builtin runtime itself, carrying the reason `/plugin` shows. Native
/// code is refused without the verified wrapper. Pure.
fn host_launch(
    tier: HostTier,
    runtime: &HostRuntime,
    args: Vec<String>,
    data: PathBuf,
    memory_cap: u64,
    wrapped: Result<Wrapped, String>,
) -> Result<HostLaunch, String> {
    let (program, args, sandbox, sandbox_env) = match wrapped {
        Ok(wrapped) => {
            let mut command = wrapped.command.into_iter();
            let program = command
                .next()
                .ok_or("sandbox wrapper produced an empty command")?;
            (
                PathBuf::from(program),
                command.collect(),
                HostSandbox::Wrapped(wrapped.name),
                wrapped.env,
            )
        }
        Err(reason) if tier == HostTier::Plugin => {
            return Err(format!(
                "Native extensions require a verified OS sandbox: {reason}"
            ));
        }
        // Only the pinned Builtin tier may run without filesystem/network
        // isolation. Every effect is still admitted by Rust operation tickets.
        Err(reason) => (
            runtime.path.clone(),
            args,
            HostSandbox::Unsandboxed(reason),
            Vec::new(),
        ),
    };
    Ok(HostLaunch {
        tier,
        program,
        args,
        cwd: data,
        sandbox,
        sandbox_env,
        runtime: runtime.clone(),
        runtime_env: runtime_env(runtime.kind),
        memory_cap,
        memory: MemoryEnforcement::planned(runtime.kind),
        builtin_modules: tier::BUILTIN_MODULES,
        native_snapshot_roots: Vec::new(),
        #[cfg(windows)]
        windows: None,
    })
}

/// Why [`wrap_host`] found no wrapper on this platform.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn no_wrapper_reason() -> String {
    format!(
        "bwrap unavailable: {} is not an executable file (install bubblewrap)",
        codewhale_sandbox::bwrap::BWRAP_PATH
    )
}

#[cfg(target_os = "macos")]
fn no_wrapper_reason() -> String {
    "Seatbelt (sandbox-exec) is unavailable".to_string()
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", not(target_env = "ohos"))
)))]
fn no_wrapper_reason() -> String {
    "no OS sandbox for the host on this platform".to_string()
}

/// How long the bwrap probe may take. A working bwrap runs
/// `<runtime> --version` in well under a second, even on a loaded CI runner.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
const BWRAP_PROBE_DEADLINE: Duration = Duration::from_secs(10);

/// Run the finished bwrap wrapper around `<runtime> --version` (`command`),
/// with the selected startup environment, to learn whether bwrap works here: it may be
/// installed yet unable to create its namespaces. Blocking.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn probe_bwrap(command: &[String], cwd: &Path, env: &[(String, String)]) -> Result<(), String> {
    use std::io::Read as _;
    use wait_timeout::ChildExt as _;
    let (program, args) = command
        .split_first()
        .ok_or("the sandbox wrapper produced an empty command")?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("bwrap unavailable: {program} does not start ({error})"))?;
    let status = match child.wait_timeout(BWRAP_PROBE_DEADLINE) {
        Ok(Some(status)) => status,
        outcome => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(match outcome {
                Err(error) => format!("bwrap unavailable: waiting for the probe failed ({error})"),
                _ => format!(
                    "bwrap unavailable: the probe did not finish within {BWRAP_PROBE_DEADLINE:?}"
                ),
            });
        }
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    bwrap_probe_verdict(status.success(), &status.to_string(), &stderr)
}

/// What a wrapped `<runtime> --version` run says about bwrap here: `Ok` when
/// it ran, otherwise the reason `/plugin` and doctor show — bwrap's first
/// line of stderr (or the exit status), and a hint when that line is about
/// the user namespace bwrap could not create. Pure.
#[cfg(any(test, all(target_os = "linux", not(target_env = "ohos"))))]
fn bwrap_probe_verdict(succeeded: bool, status: &str, stderr: &str) -> Result<(), String> {
    if succeeded {
        return Ok(());
    }
    let first = stderr.lines().map(str::trim).find(|line| !line.is_empty());
    let mut detail: String = first.map_or_else(
        || status.to_string(),
        |line| line.chars().take(240).collect(),
    );
    if first.is_some_and(|line| line.contains("namespace") || line.contains("uid map")) {
        detail.push_str(
            "; unprivileged user namespaces look blocked here (on Ubuntu 24.04 and later: the kernel.apparmor_restrict_unprivileged_userns sysctl)",
        );
    }
    Err(format!("bwrap unavailable ({detail})"))
}

/// The kernel-enforced memory cap on Linux: `RLIMIT_DATA`, applied in the
/// child between fork and exec, so only the host (and what it starts) is
/// limited. Soft and hard limit are both set, so plugin code cannot raise
/// it. An unprivileged process cannot raise its hard limit, so when the
/// inherited hard limit is already below `cap` the host gets that lower
/// limit instead of failing to spawn with `EPERM`.
///
/// Known limit: in that case `/plugin` and doctor still name the configured
/// cap, not the lower inherited one.
#[cfg(target_os = "linux")]
pub(super) fn limit_child_memory(command: &mut tokio::process::Command, cap: u64) {
    // SAFETY: the closure runs in the forked child before exec and calls only
    // `getrlimit` and `setrlimit`, which are async-signal-safe; it allocates
    // nothing.
    unsafe {
        command.pre_exec(move || {
            let mut inherited = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_DATA, &raw mut inherited) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let cap = (cap as libc::rlim_t).min(inherited.rlim_max);
            let limit = libc::rlimit {
                rlim_cur: cap,
                rlim_max: cap,
            };
            if libc::setrlimit(libc::RLIMIT_DATA, &raw const limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn limit_child_memory(_command: &mut tokio::process::Command, _cap: u64) {}

/// Resident size of `pid` in bytes, for the macOS memory-cap check.
#[cfg(target_os = "macos")]
pub(crate) fn resident_bytes(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is a correctly sized, writable `proc_taskinfo` buffer.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: a full-size write initialized the struct.
    (written == size).then(|| unsafe { info.assume_init() }.pti_resident_size)
}

/// Platforms without a supervisor-side check (Linux uses `RLIMIT_DATA`,
/// Windows the Job Object).
#[cfg(not(target_os = "macos"))]
pub(crate) fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Report the observed exit and configured cap. SIGKILL alone cannot identify
/// jetsam: an operator or another process can send the same signal.
fn exit_reason(
    status: std::process::ExitStatus,
    memory: Option<MemoryEnforcement>,
    cap: u64,
) -> String {
    let reason = format!("exited with {status}");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if status.signal() == Some(libc::SIGKILL) && memory == Some(MemoryEnforcement::Jetsam) {
            return format!(
                "{reason}; configured kernel memory limit: {} MiB; SIGKILL cause unavailable",
                cap / (1024 * 1024)
            );
        }
    }
    #[cfg(not(unix))]
    let _ = (memory, cap);
    reason
}

/// What one host-originated request is told about its own life: its id on the
/// channel and the token that fires when the request is cancelled (the host's
/// `$/cancel`, the owner's revocation, or the host's exit). A handler that
/// waits for anything must wait on `cancel` too; one that ignores it is
/// abandoned [`CANCEL_GRACE`] after it fires and its answer is dropped.
pub(crate) struct HostRequestContext {
    pub id: u64,
    pub cancel: CancellationToken,
    /// The channel's kill switch, for a handler that finds the host in
    /// violation of the protocol (it ends the host, like a bad frame).
    kill: mpsc::Sender<String>,
}

#[cfg(test)]
impl HostRequestContext {
    /// A context not attached to any channel, with the receiver its violations
    /// arrive on and the token that cancels it.
    pub(crate) fn for_test(id: u64) -> (Self, mpsc::Receiver<String>, CancellationToken) {
        let (kill, violations) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        (
            Self {
                id,
                cancel: cancel.clone(),
                kill,
            },
            violations,
            cancel,
        )
    }
}

impl HostRequestContext {
    /// Report a protocol violation by the host: the host process is ended.
    pub(crate) fn violation(&self, reason: String) {
        let _ = self.kill.try_send(format!("protocol violation: {reason}"));
    }
}

/// Callbacks from the channel into the manager.
#[async_trait]
pub(crate) trait HostEvents: Send + Sync + 'static {
    fn register(&self, params: &protocol::RegisterParams) -> RegisterResult;
    fn unregister(&self, params: &protocol::UnregisterParams);
    fn faulted(&self, params: &protocol::FaultedParams);
    fn log(&self, params: &protocol::LogParams);
    fn exited(&self, host_generation: u64, reason: String, stderr_tail: String);

    /// A current, non-revoked call received a correlated reply. Late replies
    /// and heartbeat answers do not pass here; the monitor validates pongs.
    fn responded(&self) {}

    /// Answer one host-originated request. Every such request runs as its own
    /// task (`start_host_request`), so a handler may take as long as it needs
    /// without holding up the reader. The registry requests are quick and
    /// synchronous; a request that has to wait (a tool call the host asked the
    /// core to make, later) overrides this and observes `cx.cancel`.
    async fn host_request(
        &self,
        request: HostRequest,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        registry_host_request(self, request, &cx)
    }
}

/// The answer to a registry request (`registry/register`,
/// `registry/unregister`), which is quick and synchronous, and the refusal of a
/// `core/call` an events implementation does not serve. The default
/// [`HostEvents::host_request`], and what an overriding one falls back to.
pub(crate) fn registry_host_request<E: HostEvents + ?Sized>(
    events: &E,
    request: HostRequest,
    cx: &HostRequestContext,
) -> Result<Value, RpcErrorWire> {
    tracing::trace!(target: "extension_host", id = cx.id, "host request");
    // A request cancelled before its handler began does nothing.
    if cx.cancel.is_cancelled() {
        return Err(RpcErrorWire {
            code: error_code::CANCELLED,
            message: "cancelled".to_string(),
            data: None,
        });
    }
    match request {
        HostRequest::Register(params) => Ok(serde_json::to_value(events.register(&params))
            .unwrap_or_else(|_| json!({"refused": "internal"}))),
        HostRequest::Unregister(params) => {
            events.unregister(&params);
            Ok(json!({}))
        }
        HostRequest::SessionCheckpoint(_) => Err(RpcErrorWire {
            code: error_code::REFUSED,
            message: "session/checkpoint is not served here".to_string(),
            data: None,
        }),
        HostRequest::ExecutionRedeem(_)
        | HostRequest::CoreCall(_)
        | HostRequest::ProcLaunch(_)
        | HostRequest::ProcRead(_)
        | HostRequest::ProcWrite(_)
        | HostRequest::ProcClose(_)
        | HostRequest::NetStart(_)
        | HostRequest::NetFetch(_)
        | HostRequest::NetRead(_)
        | HostRequest::NetRelease(_)
        | HostRequest::NetClose(_) => Err(RpcErrorWire {
            code: error_code::REFUSED,
            message: "core/call is not served here".to_string(),
            data: None,
        }),
    }
}

/// Why an in-flight host request was cancelled; decides what the host is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelReason {
    /// The host sent `$/cancel`: it has already stopped waiting, so whatever
    /// the handler produces afterwards is dropped.
    Host,
    /// The request's owner was revoked: the host is answered `Cancelled`.
    Revoked,
    /// The host exited: there is nobody to answer.
    Exit,
}

struct InboundRequest {
    /// The plugin whose revocation cancels it.
    owner: String,
    cancel: CancellationToken,
    cancelled: Option<CancelReason>,
}

/// The host-originated requests in flight, by the id the host gave them. At
/// most [`protocol::MAX_INFLIGHT`] at a time (the same bound the host holds
/// itself to), no id twice, and nothing admitted once the host has exited.
#[derive(Default)]
pub(crate) struct InboundRequests {
    table: Mutex<InboundTable>,
}

#[derive(Default)]
struct InboundTable {
    requests: HashMap<u64, InboundRequest>,
    closed: bool,
}

impl InboundRequests {
    /// Start tracking request `id` of `owner`. `Err` is a protocol violation:
    /// the host reused an id still in flight, or has more requests in flight
    /// than its own limit allows.
    fn admit(&self, id: u64, owner: &str) -> Result<CancellationToken, String> {
        let mut table = self.table.lock().expect("inbound lock");
        if table.closed {
            return Err("host request after the host exited".to_string());
        }
        if table.requests.contains_key(&id) {
            return Err(format!("host request id {id} is already in flight"));
        }
        if table.requests.len() >= protocol::MAX_INFLIGHT {
            return Err(format!(
                "more than {} host requests in flight",
                protocol::MAX_INFLIGHT
            ));
        }
        let cancel = CancellationToken::new();
        table.requests.insert(
            id,
            InboundRequest {
                owner: owner.to_string(),
                cancel: cancel.clone(),
                cancelled: None,
            },
        );
        Ok(cancel)
    }

    /// The host's `$/cancel {id}`. An id that is not in flight (already
    /// answered, or never sent) is ignored: the cancel raced the answer.
    fn cancel_by_host(&self, id: u64) {
        if let Some(request) = self
            .table
            .lock()
            .expect("inbound lock")
            .requests
            .get_mut(&id)
        {
            request.fire(CancelReason::Host);
        }
    }

    /// Cancel every in-flight request of `plugin_id`: its owner was revoked.
    fn cancel_owner(&self, plugin_id: &str) {
        for request in self
            .table
            .lock()
            .expect("inbound lock")
            .requests
            .values_mut()
            .filter(|request| request.owner == plugin_id)
        {
            request.fire(CancelReason::Revoked);
        }
    }

    /// The host exited: cancel everything and admit nothing more.
    fn cancel_all(&self) {
        let mut table = self.table.lock().expect("inbound lock");
        table.closed = true;
        for request in table.requests.values_mut() {
            request.fire(CancelReason::Exit);
        }
    }

    /// The handler is done (or abandoned): forget the request and say why it
    /// was cancelled, if it was.
    fn finish(&self, id: u64) -> Option<CancelReason> {
        self.table
            .lock()
            .expect("inbound lock")
            .requests
            .remove(&id)
            .and_then(|request| request.cancelled)
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self.table.lock().expect("inbound lock").requests.len()
    }
}

impl InboundRequest {
    /// Cancel for `reason`; the first reason stands.
    fn fire(&mut self, reason: CancelReason) {
        self.cancelled.get_or_insert(reason);
        self.cancel.cancel();
    }
}

/// Receives one request's outcome.
pub(crate) type CallReceiver = oneshot::Receiver<Result<Value, HostCallError>>;

struct PendingCall {
    tx: oneshot::Sender<Result<Value, HostCallError>>,
    /// Plugin whose revocation cancels this call.
    owner: Option<String>,
    /// Exact never-reused registration handle, when this is a contribution call.
    handle: Option<u64>,
    revoked: bool,
    heartbeat: bool,
}

#[derive(Default)]
struct Handshake {
    hello: Option<oneshot::Sender<protocol::HelloParams>>,
    ready: Option<oneshot::Sender<()>>,
}

pub(crate) struct HostProcess {
    /// The tier this process was launched for.
    pub tier: HostTier,
    /// The generation of its tier's host this process is (what the manager
    /// bumps per launch); a capability ticket is bound to it.
    pub generation: u64,
    pub pid: Option<u32>,
    /// The runtime the Rust side launched; `host/hello` must agree.
    pub runtime: HostRuntime,
    /// Runtime version as reported by the host in `host/hello`.
    pub runtime_version: std::sync::OnceLock<String>,
    pub memory_cap: u64,
    /// How the cap is enforced for this process, settled at the handshake.
    memory: Arc<std::sync::OnceLock<MemoryEnforcement>>,
    pub sandbox: HostSandbox,
    native_snapshot_roots: Vec<PathBuf>,
    tree: Arc<codewhale_runtime::process_tree::ProcessTree>,
    #[cfg(windows)]
    windows: Option<super::windows::NativeSandbox>,
    outbound: mpsc::Sender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<u64, PendingCall>>>,
    /// Requests the host sent, running as tasks ([`start_host_request`]).
    inbound: Arc<InboundRequests>,
    /// Admission checks and sealing hold `pending`; the manager also reads
    /// this flag to avoid activation while the exit callback is still pending.
    admission_closed: AtomicBool,
    next_id: AtomicU64,
    /// Heartbeat pings among those ids; tests count only the core's own work.
    #[cfg(test)]
    heartbeats_sent: AtomicU64,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    exited: tokio::sync::watch::Receiver<bool>,
    kill: mpsc::Sender<String>,
}

fn push_tail(tail: &Mutex<VecDeque<u8>>, bytes: &[u8]) {
    let mut tail = tail.lock().expect("stderr tail lock");
    tail.extend(bytes);
    while tail.len() > STDERR_TAIL_BYTES {
        tail.pop_front();
    }
}

fn tail_string(tail: &Mutex<VecDeque<u8>>) -> String {
    let tail = tail.lock().expect("stderr tail lock");
    let bytes: Vec<u8> = tail.iter().copied().collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The watcher/protocol remains one implementation for both launch mechanisms.
enum HostChild {
    Tokio(tokio::process::Child),
    #[cfg(windows)]
    Native(super::windows::Child),
}
impl HostChild {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Tokio(child) => child.wait().await,
            #[cfg(windows)]
            Self::Native(child) => child.wait().await,
        }
    }
    async fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tokio(child) => child.kill().await,
            #[cfg(windows)]
            Self::Native(child) => child.kill().await,
        }
    }
}
struct SpawnedHost {
    child: HostChild,
    pid: Option<u32>,
    tree: Arc<codewhale_runtime::process_tree::ProcessTree>,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
}
fn spawn_host(
    launch: &HostLaunch,
    mut command: tokio::process::Command,
    _environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<SpawnedHost, String> {
    #[cfg(windows)]
    if let Some(sandbox) = &launch.windows {
        let native = sandbox
            .spawn(&launch.args, _environment, launch.memory_cap)
            .map_err(|error| {
                format!("failed to launch the verified Windows Native host: {error}")
            })?;
        let stdin =
            tokio::process::ChildStdin::from_std(std::process::ChildStdin::from(native.stdin))
                .map_err(|error| format!("host stdin conversion failed: {error}"))?;
        let stdout =
            tokio::process::ChildStdout::from_std(std::process::ChildStdout::from(native.stdout))
                .map_err(|error| format!("host stdout conversion failed: {error}"))?;
        let stderr =
            tokio::process::ChildStderr::from_std(std::process::ChildStderr::from(native.stderr))
                .map_err(|error| format!("host stderr conversion failed: {error}"))?;
        let tree = Arc::clone(&native.child.tree);
        let pid = Some(native.child.pid);
        return Ok(SpawnedHost {
            child: HostChild::Native(native.child),
            pid,
            tree,
            stdin,
            stdout,
            stderr,
        });
    }
    // On Linux a failure to apply the memory cap in the child surfaces
    // here as a spawn error carrying only its errno, indistinguishable
    // from a failed exec, so the message names both.
    let mut child = command.spawn().map_err(|error| {
            if cfg!(target_os = "linux") {
                format!(
                    "failed to start {}, or to apply its {} MiB memory cap (RLIMIT_DATA) before exec: {error}",
                    launch.program.display(),
                    launch.memory_cap / (1024 * 1024)
                )
            } else {
                format!("failed to start {}: {error}", launch.program.display())
            }
        })?;
    let pid = child.id();
    let tree = match codewhale_runtime::process_tree::ProcessTree::attach_tokio(&child) {
        Ok(tree) => Arc::new(tree),
        Err(error) => {
            let _ = child.start_kill();
            return Err(format!("failed to contain the extension host: {error}"));
        }
    };
    #[cfg(windows)]
    if let Err(error) = tree.limit_process_memory(launch.memory_cap) {
        let _ = tree.kill();
        let _ = child.start_kill();
        return Err(format!(
            "failed to cap the extension host's memory: {error}"
        ));
    }
    let stdin = child.stdin.take().ok_or("host stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("host stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("host stderr unavailable")?;

    Ok(SpawnedHost {
        child: HostChild::Tokio(child),
        pid,
        tree,
        stdin,
        stdout,
        stderr,
    })
}

impl HostProcess {
    pub(crate) async fn admit_native_root(
        &self,
        authority: codewhale_config::PluginAuthority,
    ) -> Result<(), String> {
        #[cfg(windows)]
        let sandbox = self
            .windows
            .clone()
            .ok_or("Native host has no verified LPAC plan")?;
        #[cfg(not(windows))]
        let snapshot_roots = self.native_snapshot_roots.clone();
        let policy = crate::plugins::activation::extension_host_policy_enabled();
        if !policy {
            return Err("Native extensions are disabled".into());
        }
        #[cfg(test)]
        let env_scope = crate::test_support::env_scope_ticket();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
            let capability = crate::plugins::activation::PluginActivationCapability::Native;
            crate::plugins::registry::verify_plugin_component_authority(&authority, capability)?;
            let root =
                crate::plugins::agent_plugin::plugin_root_for_manifest(&authority.staged_manifest)
                    .ok_or("reviewed runtime manifest has no bundle root")?;
            #[cfg(windows)]
            sandbox.admit_root(root)?;
            #[cfg(not(windows))]
            {
                let canonical = root.canonicalize().map_err(|error| error.to_string())?;
                if !snapshot_roots.iter().any(|grant| canonical.starts_with(grant) && canonical != *grant) {
                    return Err("Native snapshot store was not granted to this host; restart Codewhale to admit this store".into());
                }
                let current = native_snapshot_roots(&[authority.state_path.clone()])?;
                if !current.iter().any(|grant| snapshot_roots.contains(grant) && canonical.starts_with(grant)) {
                    return Err("Native snapshot store changed after host launch".into());
                }
            }
            crate::plugins::registry::verify_plugin_component_authority(&authority, capability)
        })
        .await
        .map_err(|error| format!("Native bundle admission worker failed: {error}"))?
    }

    /// Spawn the host and complete the handshake. `expected_sha256` is the
    /// digest of the bundle this process materialized; the host's
    /// self-reported digest must match (a consistency check, not
    /// anti-substitution: the control is that Rust chooses what to exec).
    pub(crate) async fn spawn(
        generation: u64,
        launch: &HostLaunch,
        expected_sha256: &str,
        events: Arc<dyn HostEvents>,
    ) -> Result<Arc<Self>, String> {
        let mut command = tokio::process::Command::new(&launch.program);
        codewhale_util::utils::suppress_tokio_console_window(&mut command);
        command
            .args(&launch.args)
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Scrubbed environment: no credentials, no ambient proxy URLs.
        command.env_clear();
        let parent_pid = std::process::id().to_string();
        // On Unix the host leads its own process group (below), so it may kill
        // that group when the core goes away (stdin EOF, or a parent change
        // seen by its watchdog thread).
        let own_group = if cfg!(unix) { "1" } else { "0" };
        let memory_request = (launch.memory == MemoryEnforcement::Jetsam)
            .then(|| (launch.memory_cap / (1024 * 1024)).max(1).to_string());
        let overrides = launch
            .sandbox_env
            .iter()
            .chain(&launch.runtime_env)
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .chain(
                memory_request
                    .as_deref()
                    .map(|mib| (MEMORY_LIMIT_REQUEST_ENV, mib)),
            )
            .chain([
                ("CODEWHALE_HOST_PARENT_PID", parent_pid.as_str()),
                ("CODEWHALE_HOST_PROCESS_GROUP", own_group),
            ]);
        let environment = codewhale_runtime::child_env::sanitized_plugin_mcp_env_from(
            std::env::vars_os(),
            overrides,
        );
        command.envs(environment.iter().map(|(key, value)| (key, value)));
        #[cfg(unix)]
        command.process_group(0);
        limit_child_memory(&mut command, launch.memory_cap);

        let SpawnedHost {
            mut child,
            pid,
            tree,
            stdin,
            stdout,
            stderr,
        } = spawn_host(launch, command, &environment)?;
        let memory: Arc<std::sync::OnceLock<MemoryEnforcement>> = Arc::default();

        let (outbound, mut outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE);
        let pending: Arc<Mutex<HashMap<u64, PendingCall>>> = Arc::default();
        let inbound: Arc<InboundRequests> = Arc::default();
        let stderr_tail: Arc<Mutex<VecDeque<u8>>> = Arc::default();
        let (exited_tx, exited_rx) = tokio::sync::watch::channel(false);
        let (hello_tx, hello_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let handshake = Arc::new(Mutex::new(Handshake {
            hello: Some(hello_tx),
            ready: Some(ready_tx),
        }));

        // Writer: the only task that touches stdin. Dropping every sender
        // closes stdin, which the host treats as "core is gone".
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(frame) = outbound_rx.recv().await {
                if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // stderr: a bounded tail for diagnostics, chunks into tracing. Read
        // in fixed-size chunks, never by line: a plugin writing endless
        // output without a newline must not grow this process's memory.
        {
            let tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut stderr = stderr;
                let mut chunk = vec![0_u8; 4096];
                while let Ok(read) = stderr.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    push_tail(&tail, &chunk[..read]);
                    tracing::debug!(
                        target: "extension_host",
                        "host stderr: {}",
                        String::from_utf8_lossy(&chunk[..read]).trim_end()
                    );
                }
            });
        }

        // Reader: every frame is validated strictly; a framing or protocol
        // violation kills the host (a plugin wrote to the channel, or the
        // host is not ours).
        let (kill_tx, mut kill_rx) = mpsc::channel::<String>(1);
        {
            let reader = Reader {
                kill: kill_tx.clone(),
                pending: Arc::clone(&pending),
                inbound: Arc::clone(&inbound),
                outbound: outbound.clone(),
                events: Arc::clone(&events),
                handshake: Arc::clone(&handshake),
            };
            let tier = launch.tier;
            let kill_tx = kill_tx.clone();
            tokio::spawn(async move {
                let mut stdout = stdout;
                loop {
                    let value = match protocol::read_frame(&mut stdout).await {
                        Ok(Some(value)) => value,
                        Ok(None) => break,
                        Err(error) => {
                            let _ = kill_tx.try_send(format!("channel framing violation: {error}"));
                            break;
                        }
                    };
                    let message = match protocol::parse_host_message(value, tier) {
                        Ok(message) => message,
                        Err(error) => {
                            let _ = kill_tx.try_send(format!("protocol violation: {error}"));
                            break;
                        }
                    };
                    if let Err(violation) = reader.handle(message) {
                        let _ = kill_tx.try_send(format!("protocol violation: {violation}"));
                        break;
                    }
                }
            });
        }

        // Exit watcher: owns the child. On exit, fail everything and report.
        {
            let pending = Arc::clone(&pending);
            let inbound = Arc::clone(&inbound);
            let tail = Arc::clone(&stderr_tail);
            let events = Arc::clone(&events);
            let tree = Arc::clone(&tree);
            let memory = Arc::clone(&memory);
            let memory_cap = launch.memory_cap;
            tokio::spawn(async move {
                let reason = tokio::select! {
                    biased;
                    status = child.wait() => match status {
                        Ok(status) => exit_reason(status, memory.get().copied(), memory_cap),
                        Err(error) => format!("wait failed: {error}"),
                    },
                    Some(reason) = kill_rx.recv() => {
                        let _ = tree.kill();
                        let _ = child.kill().await;
                        reason
                    }
                };
                // The leader is gone; take anything it left behind with it.
                let _ = tree.kill();
                let drained: Vec<PendingCall> = {
                    let mut pending = pending.lock().expect("pending lock");
                    // Publish exit under the admission lock before draining:
                    // waking a failed call must not admit another orphaned call.
                    let _ = exited_tx.send(true);
                    pending.drain().map(|(_, call)| call).collect()
                };
                for call in drained {
                    let _ = call.tx.send(Err(HostCallError::Exited(reason.clone())));
                }
                // Requests the host sent have nobody left to answer.
                inbound.cancel_all();
                // Give the stderr task a moment to capture the last lines.
                tokio::time::sleep(Duration::from_millis(50)).await;
                events.exited(generation, reason, tail_string(&tail));
            });
        }

        let host = Arc::new(Self {
            tier: launch.tier,
            generation,
            pid,
            runtime: launch.runtime.clone(),
            runtime_version: std::sync::OnceLock::new(),
            memory_cap: launch.memory_cap,
            memory,
            sandbox: launch.sandbox.clone(),
            native_snapshot_roots: launch.native_snapshot_roots.clone(),
            tree,
            #[cfg(windows)]
            windows: launch.windows.clone(),
            outbound,
            pending,
            inbound,
            admission_closed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            #[cfg(test)]
            heartbeats_sent: AtomicU64::new(0),
            stderr_tail,
            exited: exited_rx,
            kill: kill_tx.clone(),
        });

        let handshake_result = tokio::time::timeout(HANDSHAKE_DEADLINE, async {
            let hello = hello_rx
                .await
                .map_err(|_| "host exited before host/hello".to_string())?;
            if hello.protocol.min > protocol::PROTOCOL_VERSION
                || hello.protocol.max < protocol::PROTOCOL_VERSION
            {
                return Err(format!(
                    "host speaks protocol {}..={}, core speaks {}",
                    hello.protocol.min,
                    hello.protocol.max,
                    protocol::PROTOCOL_VERSION
                ));
            }
            // Never a silent runtime switch: the host must be running on
            // the runtime this process chose and launched.
            if hello.runtime.name != launch.runtime.kind.name() {
                return Err(format!(
                    "host reports runtime {} but {} was launched",
                    hello.runtime.name,
                    launch.runtime.kind.name()
                ));
            }
            check_hello_identity(&hello, launch.tier, launch.builtin_modules)?;
            // Restarts reuse the pinned runtime without probing it again, so
            // the binary at that path can have been replaced since (an
            // upgrade mid-session). Its flags and lockdown were chosen for
            // the probed version; refuse rather than run an unprobed one.
            if !launch.runtime.reports_version(&hello.runtime.version) {
                return Err(format!(
                    "host reports {} {} but {} {} was probed at {}: the runtime binary changed mid-session; restart Codewhale to use the new version",
                    hello.runtime.name,
                    hello.runtime.version,
                    launch.runtime.kind.name(),
                    launch.runtime.version_string(),
                    launch.runtime.path.display()
                ));
            }
            if hello.bundle_sha256 != expected_sha256 {
                return Err(format!(
                    "host bundle digest {} does not match the materialized bundle {}",
                    hello.bundle_sha256, expected_sha256
                ));
            }
            let requested = memory_request
                .as_deref()
                .and_then(|mib| mib.parse::<u64>().ok());
            let memory = match (hello.memory_limit_mib, requested) {
                (Some(applied), Some(requested)) if applied == requested => {
                    MemoryEnforcement::Jetsam
                }
                (Some(applied), _) => {
                    return Err(format!(
                        "host reports a {applied} MiB memory limit the core did not ask for"
                    ));
                }
                // The mandatory kernel cap cannot degrade to a delayed RSS
                // observation. Refuse before plugin initialization and keep
                // the host's stderr explanation in the existing diagnosis.
                (None, Some(requested)) => {
                    return Err(format!(
                        "host did not apply the requested {requested} MiB kernel memory limit; initialization refused"
                    ));
                }
                (None, None) => launch.memory,
            };
            let _ = host.memory.set(memory);
            let initialize = CoreRequest::Initialize(InitializeParams {
                protocol: protocol::PROTOCOL_VERSION,
                limits: HostLimits {
                    max_frame: protocol::MAX_FRAME as u64,
                    max_inflight: protocol::MAX_INFLIGHT as u64,
                    dispose_deadline_ms: DISPOSE_DEADLINE.as_millis() as u64,
                    activate_deadline_ms: ACTIVATE_DEADLINE.as_millis() as u64,
                },
            });
            host.call(initialize, None)
                .await
                .map_err(|error| format!("host/initialize failed: {error}"))?;
            ready_rx
                .await
                .map_err(|_| "host exited before host/ready".to_string())?;
            Ok(hello.runtime.version)
        })
        .await;
        match handshake_result {
            Ok(Ok(runtime_version)) => {
                let _ = host.runtime_version.set(runtime_version);
                Ok(host)
            }
            Ok(Err(reason)) => {
                let _ = kill_tx.try_send(reason.clone());
                Err(format!(
                    "{reason}; stderr: {}",
                    tail_string(&host.stderr_tail)
                ))
            }
            Err(_) => {
                let reason = format!("handshake exceeded {HANDSHAKE_DEADLINE:?}");
                let _ = kill_tx.try_send(reason.clone());
                Err(format!(
                    "{reason}; stderr: {}",
                    tail_string(&host.stderr_tail)
                ))
            }
        }
    }

    /// How the memory cap is enforced for this process (planned until the
    /// handshake settles it).
    #[must_use]
    pub fn memory(&self) -> MemoryEnforcement {
        self.memory
            .get()
            .copied()
            .unwrap_or_else(|| MemoryEnforcement::planned(self.runtime.kind))
    }

    /// How many requests the core has sent this host (handshake included),
    /// excluding the monitor's heartbeat pings, which run on their own timer
    /// and would otherwise make a "no request yet" assertion timing-dependent.
    #[cfg(test)]
    pub(crate) fn requests_started(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed) - 1 - self.heartbeats_sent.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn has_exited(&self) -> bool {
        *self.exited.borrow()
    }

    pub(crate) fn terminate(&self, reason: String) {
        let _ = self.kill.try_send(reason);
    }

    pub(crate) fn is_retiring(&self) -> bool {
        self.admission_closed.load(Ordering::Acquire)
    }

    /// Seal admission and retire this process only if no non-heartbeat call
    /// is pending. The same lock guards admission in `start_request`.
    pub(crate) fn terminate_if_idle(&self, reason: &str) -> bool {
        let pending = self.pending.lock().expect("pending lock");
        if self.has_exited()
            || self.admission_closed.load(Ordering::Relaxed)
            || pending.values().any(|call| !call.heartbeat)
        {
            return false;
        }
        if self.kill.try_send(reason.to_string()).is_err() {
            return false;
        }
        self.admission_closed.store(true, Ordering::Release);
        true
    }

    pub(crate) fn stderr_tail(&self) -> String {
        tail_string(&self.stderr_tail)
    }

    fn send_frame(&self, value: &Value) -> Result<(), HostCallError> {
        let frame = protocol::encode_frame(value).map_err(|error| HostCallError::Rpc {
            code: error_code::INVALID_PARAMS,
            message: error.to_string(),
        })?;
        self.outbound.try_send(frame).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => HostCallError::Busy,
            mpsc::error::TrySendError::Closed(_) => {
                HostCallError::Exited("channel closed".to_string())
            }
        })
    }

    /// Send a request; the returned id can be cancelled with [`Self::cancel`].
    pub(crate) fn start_request(
        &self,
        request: CoreRequest,
        owner: Option<String>,
    ) -> Result<(u64, CallReceiver), HostCallError> {
        // A method reserved for another tier is never sent to this host.
        if !protocol::allowed_on(Direction::CoreToHost, request.method(), self.tier) {
            return Err(HostCallError::Rpc {
                code: error_code::METHOD_NOT_FOUND,
                message: format!(
                    "`{}` is not allowed on the {} tier",
                    request.method(),
                    self.tier.name()
                ),
            });
        }
        if self.has_exited() {
            return Err(HostCallError::Exited("already exited".to_string()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if matches!(request, CoreRequest::Ping) {
            self.heartbeats_sent.fetch_add(1, Ordering::Relaxed);
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("pending lock");
            // Exit may have won after the fast check above. It publishes under
            // this same lock, so every admitted call is either live or drained.
            if self.has_exited() {
                return Err(HostCallError::Exited("already exited".to_string()));
            }
            if self.admission_closed.load(Ordering::Relaxed) {
                return Err(HostCallError::Exited("host is restarting".to_string()));
            }
            // Reserve one control request for the single heartbeat monitor,
            // so saturated tool calls cannot make a healthy host look hung.
            if pending.len() >= protocol::MAX_INFLIGHT && !matches!(request, CoreRequest::Ping) {
                return Err(HostCallError::Busy);
            }
            pending.insert(
                id,
                PendingCall {
                    tx,
                    owner,
                    handle: match &request {
                        CoreRequest::ToolCall(params) => Some(params.handle),
                        CoreRequest::CommandRun(params) => Some(params.handle),
                        CoreRequest::HookEvaluate(params) => Some(params.handle),
                        _ => None,
                    },
                    revoked: false,
                    heartbeat: matches!(request, CoreRequest::Ping),
                },
            );
        }
        if let Err(error) = self.send_frame(&request.to_value(id)) {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err(error);
        }
        Ok((id, rx))
    }

    /// Send `request` and wait at most its method's deadline
    /// ([`CoreRequest::deadline`]) for the answer. On expiry the host is sent
    /// `$/cancel`, the call is forgotten (a late answer is dropped) and the
    /// caller gets [`HostCallError::Timeout`]. Dropping the returned future
    /// (a turn interrupt) cancels the same way. Every awaited core→host
    /// request goes through here; only the heartbeat drives
    /// [`Self::start_request`] itself, under its own timeouts.
    pub(crate) async fn call(
        &self,
        request: CoreRequest,
        owner: Option<String>,
    ) -> Result<Value, HostCallError> {
        self.call_with_clock(request, owner, None).await
    }

    /// [`Self::call`] with the deadline measured on `clock`, which stops while
    /// the caller waits on something that is not the host's time (a
    /// `core/call` waiting on an approval card). `None` is a clock nothing
    /// pauses: the plain deadline.
    pub(crate) async fn call_with_clock(
        &self,
        request: CoreRequest,
        owner: Option<String>,
        clock: Option<Arc<Mutex<PauseClock>>>,
    ) -> Result<Value, HostCallError> {
        let method = request.method();
        let deadline = request.deadline();
        let clock = clock.unwrap_or_else(|| Arc::new(Mutex::new(PauseClock::new())));
        let (id, mut rx) = self.start_request(request, owner)?;
        let mut guard = CancelOnDrop {
            host: self,
            id,
            armed: true,
        };
        let answer = loop {
            let remaining = clock
                .lock()
                .expect("deadline clock lock")
                .remaining(deadline);
            let Some(remaining) = remaining else {
                // `guard` is still armed: dropping it sends `$/cancel`.
                return Err(HostCallError::Timeout {
                    method,
                    after: deadline,
                });
            };
            tokio::select! {
                answer = &mut rx => break answer,
                () = tokio::time::sleep(remaining) => {}
            }
        };
        // Answered, drained at exit, or resolved by revocation: nothing left
        // to cancel.
        guard.armed = false;
        answer.unwrap_or_else(|_| Err(HostCallError::Exited("channel closed".to_string())))
    }

    /// Fire `$/cancel`. Best effort: a full or closed channel is fine, the
    /// caller resolves its side on its own schedule.
    pub(crate) fn cancel(&self, id: u64) {
        let _ = self.send_frame(&protocol::cancel_value(id));
    }

    /// Forget a request without cancelling it (its answer will be dropped).
    pub(crate) fn forget(&self, id: u64) {
        self.pending.lock().expect("pending lock").remove(&id);
    }

    /// Revocation: cancel every in-flight call owned by `plugin_id`; each
    /// resolves as cancelled when the host answers or after `CANCEL_GRACE`,
    /// whichever is first — revocation never waits on the host.
    pub(crate) fn revoke_calls_of(self: &Arc<Self>, plugin_id: &str) {
        // Requests the host sent for this owner are cancelled too (and
        // answered `Cancelled`), whatever they are waiting for.
        self.inbound.cancel_owner(plugin_id);
        self.revoke_pending_where(|call| call.owner.as_deref() == Some(plugin_id));
    }

    /// Retiring one Native entry cancels exactly its admitted contribution
    /// calls. Sibling entry requests and owner-wide broker lifecycle survive.
    pub(crate) fn revoke_calls_for_handles(self: &Arc<Self>, plugin_id: &str, handles: &[u64]) {
        self.revoke_pending_where(|call| {
            call.owner.as_deref() == Some(plugin_id)
                && call.handle.is_some_and(|handle| handles.contains(&handle))
        });
    }

    fn revoke_pending_where(self: &Arc<Self>, drop_it: impl Fn(&PendingCall) -> bool) {
        let ids: Vec<u64> = {
            let mut pending = self.pending.lock().expect("pending lock");
            pending
                .iter_mut()
                .filter(|(_, call)| drop_it(call))
                .map(|(id, call)| {
                    call.revoked = true;
                    *id
                })
                .collect()
        };
        for id in ids {
            self.cancel(id);
            let pending = Arc::clone(&self.pending);
            tokio::spawn(async move {
                tokio::time::sleep(CANCEL_GRACE).await;
                if let Some(call) = pending.lock().expect("pending lock").remove(&id) {
                    let _ = call.tx.send(Err(HostCallError::Cancelled(
                        "extension was revoked".to_string(),
                    )));
                }
            });
        }
    }

    /// Bounded shutdown: `host/shutdown` (2 s), close stdin, then kill the
    /// process group at 3 s total.
    #[cfg(test)]
    pub(crate) async fn shutdown(&self) {
        let _ = self.call(CoreRequest::Shutdown, None).await;
        let mut exited = self.exited.clone();
        let waited = tokio::time::timeout(Duration::from_secs(1), async {
            while !*exited.borrow() {
                if exited.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        if waited.is_err() {
            let _ = self.tree.kill();
        }
    }
}

/// Cancels a [`HostProcess::call`] that stops waiting before its answer:
/// deadline expiry, or the caller's future being dropped.
struct CancelOnDrop<'a> {
    host: &'a HostProcess,
    id: u64,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.host.cancel(self.id);
            self.host.forget(self.id);
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        // The exit watcher also holds the tree; kill explicitly so dropping
        // the last handle to a live host never leaves it running.
        if !self.has_exited() {
            let _ = self.tree.kill();
        }
    }
}

/// What the channel's reader task holds: everything one decoded host→core
/// message may touch.
struct Reader {
    kill: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, PendingCall>>>,
    inbound: Arc<InboundRequests>,
    outbound: mpsc::Sender<Vec<u8>>,
    events: Arc<dyn HostEvents>,
    handshake: Arc<Mutex<Handshake>>,
}

impl Reader {
    /// Route one validated host→core message. `Err` is a protocol violation
    /// the caller ends the host for.
    fn handle(&self, message: HostMessage) -> Result<(), String> {
        match message {
            HostMessage::Response { id, outcome } => {
                let Some(call) = self.pending.lock().expect("pending lock").remove(&id) else {
                    tracing::debug!(target: "extension_host", id, "dropping late host response");
                    return Ok(());
                };
                if !call.revoked && !call.heartbeat {
                    self.events.responded();
                }
                let result = if call.revoked {
                    Err(HostCallError::Cancelled(
                        "extension was revoked".to_string(),
                    ))
                } else {
                    match outcome {
                        Ok(value) => Ok(value),
                        Err(error) if error.code == error_code::CANCELLED => {
                            Err(HostCallError::Cancelled(error.message))
                        }
                        Err(error) => Err(HostCallError::Rpc {
                            code: error.code,
                            message: error.message,
                        }),
                    }
                };
                let _ = call.tx.send(result);
            }
            HostMessage::Request { id, request } => self.start_host_request(id, request)?,
            HostMessage::Notification(notification) => match notification {
                HostNotification::Hello(hello) => {
                    if let Some(tx) = self.handshake.lock().expect("handshake lock").hello.take() {
                        let _ = tx.send(hello);
                    }
                }
                HostNotification::Ready => {
                    if let Some(tx) = self.handshake.lock().expect("handshake lock").ready.take() {
                        let _ = tx.send(());
                    }
                }
                HostNotification::Faulted(params) => self.events.faulted(&params),
                HostNotification::Log(log) => {
                    self.events.log(&log);
                    let plugin = log.plugin_id.as_deref().unwrap_or("host");
                    match log.level.as_str() {
                        "error" => tracing::warn!(target: "extension_host", plugin, "{}", log.msg),
                        "warn" => tracing::info!(target: "extension_host", plugin, "{}", log.msg),
                        _ => tracing::debug!(target: "extension_host", plugin, "{}", log.msg),
                    }
                }
                // The host withdrawing a request of its own. One that has
                // already been answered is not in flight: the cancel lost the
                // race, and nothing is owed.
                HostNotification::Cancel(params) => self.inbound.cancel_by_host(params.id),
            },
        }
        Ok(())
    }

    /// Run one host-originated request as its own task: tracked by id so the
    /// host's `$/cancel`, the owner's revocation and the host's exit can
    /// cancel it, and answered when the handler finishes unless it was
    /// cancelled for a reason that makes the answer moot ([`CancelReason`]).
    /// A handler that does not stop within [`CANCEL_GRACE`] of its cancel is
    /// abandoned.
    fn start_host_request(&self, id: u64, request: HostRequest) -> Result<(), String> {
        let cancel = self.inbound.admit(id, request.plugin_id())?;
        let events = Arc::clone(&self.events);
        let inbound = Arc::clone(&self.inbound);
        let outbound = self.outbound.clone();
        let kill = self.kill.clone();
        tokio::spawn(async move {
            let cx = HostRequestContext {
                id,
                cancel: cancel.clone(),
                kill,
            };
            let mut handler = std::pin::pin!(events.host_request(request, cx));
            let outcome = tokio::select! {
                outcome = &mut handler => Some(outcome),
                () = async {
                    cancel.cancelled().await;
                    tokio::time::sleep(CANCEL_GRACE).await;
                } => None,
            };
            let outcome = match (inbound.finish(id), outcome) {
                // The host stopped waiting, or is gone: a late answer is dropped.
                (Some(CancelReason::Host | CancelReason::Exit), _) => return,
                (Some(CancelReason::Revoked), _) | (None, None) => Err(RpcErrorWire {
                    code: error_code::CANCELLED,
                    message: "extension was revoked".to_string(),
                    data: None,
                }),
                (None, Some(outcome)) => outcome,
            };
            if let Ok(frame) = protocol::encode_frame(&protocol::response_value(id, &outcome)) {
                let _ = outbound.send(frame).await;
            }
        });
        Ok(())
    }
}

/// What `host/hello` must say about the host's tier and built-in modules,
/// checked the way its runtime name and version are: against what the core
/// launched. The tier must be the one in the launch plan (`--tier=`), and the
/// module digests the bundle embeds must be exactly the ones `modules` pins, so
/// a host build and a Rust table that disagree about a built-in module are
/// refused before any module can run. Pure.
pub(crate) fn check_hello_identity(
    hello: &HelloParams,
    tier: HostTier,
    modules: &[BuiltinModule],
) -> Result<(), String> {
    if hello.tier != tier {
        return Err(format!(
            "host reports the {} tier but the {} tier was launched",
            hello.tier.name(),
            tier.name()
        ));
    }
    let describe = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(id, digest)| format!("{id}={}", digest.chars().take(12).collect::<String>()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut reported: Vec<(String, String)> = hello
        .builtin_modules
        .iter()
        .map(|module| (module.id.clone(), module.sha256.clone()))
        .collect();
    let mut pinned: Vec<(String, String)> = modules
        .iter()
        .map(|module| (module.id.to_string(), module.source_sha256.to_string()))
        .collect();
    reported.sort();
    pinned.sort();
    if reported != pinned {
        return Err(format!(
            "host bundle embeds the built-in module digests [{}] but the core pins [{}]",
            describe(&reported),
            describe(&pinned)
        ));
    }
    Ok(())
}

/// Where the embedded bundle is written: `<root>/extension-host/<sha256>/`.
#[must_use]
pub fn bundle_dir(root: &Path, sha256: &str) -> PathBuf {
    root.join("extension-host").join(sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_host_has_direct_argv_and_cannot_become_a_bun_cli() {
        let runtime = HostRuntime {
            kind: HostRuntimeKind::Bun,
            path: PathBuf::from("/opt/codewhale-extension-host"),
            version: (1, 4, 0),
            native_code_flags: Vec::new(),
            compiled: true,
        };
        assert!(runtime_args(&runtime).is_empty());
        let temp = tempfile::tempdir().unwrap();
        let launch = host_launch(
            HostTier::Builtin,
            &runtime,
            vec![HostTier::Builtin.argv_flag()],
            temp.path().to_path_buf(),
            HOST_MEMORY_CAP,
            Err("fixture unsupported platform".to_string()),
        )
        .unwrap();
        assert_eq!(launch.program, runtime.path);
        assert_eq!(launch.args, vec!["--tier=builtin".to_string()]);
        assert!(
            launch
                .runtime_env
                .contains(&("BUN_OPTIONS".to_string(), String::new()))
        );
        assert!(
            launch
                .runtime_env
                .contains(&("BUN_BE_BUN".to_string(), "0".to_string()))
        );
        assert_eq!(
            launch.memory,
            MemoryEnforcement::planned(HostRuntimeKind::Bun)
        );
    }

    fn node() -> HostRuntime {
        HostRuntime {
            kind: HostRuntimeKind::Node,
            path: PathBuf::from("/opt/node/bin/node"),
            version: (22, 19, 0),
            native_code_flags: Vec::new(),
            compiled: false,
        }
    }

    /// A failed exact sandbox probe refuses Native code on every platform.
    /// The pinned Builtin exception keeps the concrete diagnostic; a verified
    /// wrapper keeps its command and never silently falls through to raw JS.
    #[test]
    fn missing_verified_sandbox_refuses_native_but_reports_pinned_builtin_exception() {
        let runtime = node();
        let data = PathBuf::from("/home/u/.codewhale/extension-host/data");
        let mut args = runtime_args(&runtime);
        args.push("/home/u/.codewhale/extension-host/abc/host.mjs".to_string());

        let refused = bwrap_probe_verdict(
            false,
            "exit status: 1",
            "\nbwrap: setting up uid map: Permission denied\n",
        )
        .unwrap_err();
        assert!(
            refused.starts_with("bwrap unavailable (bwrap: setting up uid map: Permission denied;"),
            "{refused}"
        );
        assert!(
            refused.contains("kernel.apparmor_restrict_unprivileged_userns"),
            "{refused}"
        );
        let error = host_launch(
            HostTier::Plugin,
            &runtime,
            args.clone(),
            data.clone(),
            HOST_MEMORY_CAP,
            Err(refused.clone()),
        )
        .unwrap_err();
        assert!(error.contains("Native extensions require a verified OS sandbox"));
        assert!(error.contains(&refused));
        let launch = host_launch(
            HostTier::Builtin,
            &runtime,
            args.clone(),
            data.clone(),
            HOST_MEMORY_CAP,
            Err(refused.clone()),
        )
        .unwrap();
        assert_eq!(launch.program, runtime.path);
        assert_eq!(launch.args, args);
        assert!(launch.sandbox_env.is_empty());
        assert_eq!(launch.sandbox, HostSandbox::Unsandboxed(refused.clone()));
        assert_eq!(launch.sandbox.label(), format!("none: {refused}"));
        assert!(
            launch
                .sandbox
                .to_string()
                .starts_with(&format!("UNSANDBOXED ({refused}): ")),
            "{}",
            launch.sandbox
        );

        // No stderr: the exit status is the reason, with no namespace hint.
        assert_eq!(
            bwrap_probe_verdict(false, "signal: 9 (SIGKILL)", ""),
            Err("bwrap unavailable (signal: 9 (SIGKILL))".to_string())
        );

        // A probe that ran keeps the wrapper.
        assert_eq!(bwrap_probe_verdict(true, "exit status: 0", ""), Ok(()));
        let command: Vec<String> = ["/usr/bin/bwrap", "--unshare-all", "--die-with-parent", "--"]
            .iter()
            .map(ToString::to_string)
            .chain(std::iter::once(runtime.path.to_string_lossy().into_owned()))
            .chain(args.iter().cloned())
            .collect();
        let launch = host_launch(
            HostTier::Plugin,
            &runtime,
            args.clone(),
            data.clone(),
            HOST_MEMORY_CAP,
            Ok(Wrapped {
                name: "linux-bwrap".to_string(),
                command: command.clone(),
                env: vec![("CODEWHALE_SANDBOX".to_string(), "bwrap".to_string())],
            }),
        )
        .unwrap();
        assert_eq!(launch.program, PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(launch.args, command[1..].to_vec());
        assert_eq!(launch.cwd, data);
        assert_eq!(
            launch.sandbox,
            HostSandbox::Wrapped("linux-bwrap".to_string())
        );
        assert!(
            launch
                .sandbox
                .to_string()
                .starts_with("linux-bwrap sandbox (no direct network;"),
            "{}",
            launch.sandbox
        );
    }

    /// Each tier has its own data directory, and neither is inside the other:
    /// the data directory is the host's only writable root, so a nested
    /// builtin directory would be writable by plugin code. The plugin tier
    /// keeps the path it has always had, and the plugin tier's sandbox denies
    /// reads of the builtin tier's directory.
    #[test]
    fn each_tier_has_its_own_data_directory_and_the_plugin_tier_cannot_read_the_builtin_one() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(tier_data_dir(&home, HostTier::Builtin)).unwrap();
        let plugin = tier_data_dir(&home, HostTier::Plugin);
        let builtin = tier_data_dir(&home, HostTier::Builtin);
        assert_ne!(plugin, builtin);
        assert!(!builtin.starts_with(&plugin) && !plugin.starts_with(&builtin));
        assert_eq!(plugin, home.join("extension-host").join("data"));

        for whole_homes in [false, true] {
            let (denied, _) = host_denied_read_paths(HostTier::Plugin, &home, whole_homes);
            assert!(
                denied.contains(&builtin),
                "plugin tier (whole_homes {whole_homes}) must deny {}",
                builtin.display()
            );
            assert!(!denied.contains(&plugin));
            let (denied, _) = host_denied_read_paths(HostTier::Builtin, &home, whole_homes);
            assert!(
                !denied.contains(&builtin),
                "the builtin tier reads its own directory"
            );
        }
    }

    /// The per-plugin directory plugin config and context introduced keeps its
    /// path, so an installed plugin's data survives the tier split; a built-in
    /// module's is under the builtin tier's directory.
    #[test]
    fn the_per_plugin_data_directory_keeps_its_path() {
        let home = PathBuf::from("/home/u/.codewhale");
        let id = "user/0123456789ab/demo";
        assert_eq!(
            plugin_data_dir(&home, id, "demo"),
            home.join("extension-host/data/plugins/demo-e9f63c6f1a7f")
        );
        assert_eq!(
            owner_data_dir(&home, HostTier::Plugin, id, "demo"),
            plugin_data_dir(&home, id, "demo")
        );
        assert_eq!(
            owner_data_dir(&home, HostTier::Builtin, "host:mcp", "mcp"),
            home.join("extension-host/data-builtin/modules/mcp")
        );
    }

    /// bubblewrap can mask only what exists, so under it each Codewhale home
    /// is denied whole (an entry created later is then denied too) and its
    /// readable entries come back as exceptions; Seatbelt's form is unchanged.
    #[test]
    fn under_bubblewrap_a_codewhale_home_is_denied_whole_but_its_readable_entries() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join("extension-host")).unwrap();
        std::fs::write(home.join("config.toml.bak-1"), "").unwrap();

        let (seatbelt, none) = host_denied_read_paths(HostTier::Plugin, &home, false);
        assert!(none.is_empty());
        assert!(seatbelt.contains(&home.join("config.toml.bak-1")));
        assert!(
            seatbelt.contains(&home.join("secrets")),
            "named before it exists"
        );
        assert!(!seatbelt.contains(&home));
        assert!(!seatbelt.contains(&home.join("extension-host")));

        let (bwrap, exceptions) = host_denied_read_paths(HostTier::Plugin, &home, true);
        assert!(bwrap.contains(&home));
        assert!(!bwrap.contains(&home.join("config.toml.bak-1")));
        for entry in HOST_READABLE_HOME_ENTRIES {
            assert!(exceptions.contains(&home.join(entry)), "{entry}");
            assert!(!bwrap.contains(&home.join(entry)), "{entry}");
        }
    }

    // -----------------------------------------------------------------------
    // The host's tier and built-in module digests in `host/hello`
    // -----------------------------------------------------------------------

    const DEMO_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_DIGEST: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    const PINNED: &[BuiltinModule] = &[
        BuiltinModule {
            id: "demo",
            source_sha256: DEMO_DIGEST,
            tools: &[],
        },
        BuiltinModule {
            id: "other",
            source_sha256: OTHER_DIGEST,
            tools: &[],
        },
    ];

    fn hello(tier: HostTier, modules: &[(&str, &str)]) -> HelloParams {
        HelloParams {
            protocol: protocol::ProtocolRange { min: 1, max: 1 },
            host_version: "0.1.0".to_string(),
            bundle_sha256: "0".repeat(64),
            runtime: protocol::HelloRuntime {
                name: "node".to_string(),
                version: "22.20.0".to_string(),
            },
            tier,
            builtin_modules: modules
                .iter()
                .map(|(id, sha256)| protocol::ModuleDigestWire {
                    id: (*id).to_string(),
                    sha256: (*sha256).to_string(),
                })
                .collect(),
            memory_limit_mib: None,
        }
    }

    #[test]
    fn hello_must_report_the_launched_tier_and_exactly_the_pinned_module_digests() {
        // Production: no module, so none may be reported, on either tier.
        for tier in HostTier::ALL {
            assert_eq!(check_hello_identity(&hello(tier, &[]), tier, &[]), Ok(()));
        }
        // Order is not part of the claim.
        let reported = [("other", OTHER_DIGEST), ("demo", DEMO_DIGEST)];
        assert_eq!(
            check_hello_identity(
                &hello(HostTier::Builtin, &reported),
                HostTier::Builtin,
                PINNED
            ),
            Ok(())
        );

        let refused = |hello: HelloParams, tier: HostTier| {
            check_hello_identity(&hello, tier, PINNED).unwrap_err()
        };
        assert_eq!(
            refused(hello(HostTier::Plugin, &reported), HostTier::Builtin),
            "host reports the plugin tier but the builtin tier was launched"
        );
        assert_eq!(
            refused(hello(HostTier::Builtin, &reported), HostTier::Plugin),
            "host reports the builtin tier but the plugin tier was launched"
        );
        let pinned = "[demo=0123456789ab, other=fedcba987654]";
        for (what, rows, shown) in [
            ("none reported", vec![], "[]"),
            (
                "one missing",
                vec![("demo", DEMO_DIGEST)],
                "[demo=0123456789ab]",
            ),
            (
                "an unpinned module",
                vec![
                    ("demo", DEMO_DIGEST),
                    ("other", OTHER_DIGEST),
                    ("extra", DEMO_DIGEST),
                ],
                "[demo=0123456789ab, extra=0123456789ab, other=fedcba987654]",
            ),
            (
                "a changed digest",
                vec![("demo", OTHER_DIGEST), ("other", OTHER_DIGEST)],
                "[demo=fedcba987654, other=fedcba987654]",
            ),
            (
                "a row twice",
                vec![
                    ("demo", DEMO_DIGEST),
                    ("demo", DEMO_DIGEST),
                    ("other", OTHER_DIGEST),
                ],
                "[demo=0123456789ab, demo=0123456789ab, other=fedcba987654]",
            ),
        ] {
            assert_eq!(
                refused(hello(HostTier::Builtin, &rows), HostTier::Builtin),
                format!(
                    "host bundle embeds the built-in module digests {shown} but the core pins {pinned}"
                ),
                "{what}"
            );
        }
        // A host that reports modules when the core pins none is refused too.
        assert!(
            check_hello_identity(
                &hello(HostTier::Plugin, &[("demo", DEMO_DIGEST)]),
                HostTier::Plugin,
                &[]
            )
            .is_err()
        );
    }

    // -----------------------------------------------------------------------
    // Host-originated requests run as their own tracked tasks
    // -----------------------------------------------------------------------

    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    /// How a stub handler waits.
    #[derive(Clone, Copy)]
    enum Behaviour {
        /// Until `release`, or until its request is cancelled (then it stops).
        WaitsUnlessCancelled,
        /// Until `release`, whatever happens to its request.
        IgnoresCancel,
        /// Forever.
        Hangs,
    }

    struct Stub {
        behaviour: Behaviour,
        release: Arc<Notify>,
        started: AtomicUsize,
        saw_cancel: AtomicBool,
    }

    impl Stub {
        fn new(behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                behaviour,
                release: Arc::new(Notify::new()),
                started: AtomicUsize::new(0),
                saw_cancel: AtomicBool::new(false),
            })
        }
    }

    #[async_trait]
    impl HostEvents for Stub {
        fn register(&self, _: &protocol::RegisterParams) -> RegisterResult {
            unreachable!("the stub answers every request itself")
        }
        fn unregister(&self, _: &protocol::UnregisterParams) {}
        fn faulted(&self, _: &protocol::FaultedParams) {}
        fn log(&self, _: &protocol::LogParams) {}
        fn exited(&self, _: u64, _: String, _: String) {}

        async fn host_request(
            &self,
            _request: HostRequest,
            cx: HostRequestContext,
        ) -> Result<Value, RpcErrorWire> {
            self.started.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::WaitsUnlessCancelled => {
                    tokio::select! {
                        () = cx.cancel.cancelled() => {
                            self.saw_cancel.store(true, Ordering::SeqCst);
                            Err(RpcErrorWire {
                                code: error_code::CANCELLED,
                                message: "stub saw the cancel".to_string(),
                                data: None,
                            })
                        }
                        () = self.release.notified() => Ok(json!({"answered": cx.id})),
                    }
                }
                Behaviour::IgnoresCancel => {
                    self.release.notified().await;
                    Ok(json!({"answered": cx.id}))
                }
                Behaviour::Hangs => std::future::pending().await,
            }
        }
    }

    /// An events implementation that keeps the trait's own answers for the
    /// registry requests.
    struct Plain;

    #[async_trait]
    impl HostEvents for Plain {
        fn register(&self, _: &protocol::RegisterParams) -> RegisterResult {
            RegisterResult::Admitted { handle: 9 }
        }
        fn unregister(&self, _: &protocol::UnregisterParams) {}
        fn faulted(&self, _: &protocol::FaultedParams) {}
        fn log(&self, _: &protocol::LogParams) {}
        fn exited(&self, _: u64, _: String, _: String) {}
    }

    struct Rig {
        reader: Reader,
        frames: mpsc::Receiver<Vec<u8>>,
    }

    fn new_rig(events: Arc<dyn HostEvents>) -> Rig {
        let (outbound, frames) = mpsc::channel(OUTBOUND_QUEUE);
        Rig {
            reader: Reader {
                kill: mpsc::channel(1).0,
                pending: Arc::default(),
                inbound: Arc::default(),
                outbound,
                events,
                handshake: Arc::default(),
            },
            frames,
        }
    }

    fn owner(plugin: &str) -> codewhale_config::OwnerRef {
        codewhale_config::OwnerRef {
            plugin_id: plugin.to_string(),
            generation: 1,
            owner_token: "t".repeat(32),
        }
    }

    fn register_request(plugin: &str) -> HostRequest {
        HostRequest::Register(protocol::RegisterParams {
            scope: None,
            owner: owner(plugin),
            kind: protocol::RegisterKind::Tool,
            spec: protocol::RegisterSpecWire {
                name: "t".to_string(),
                description: "d".to_string(),
                input_schema: Some(serde_json::Map::new()),
                argument_hint: None,
            },
        })
    }

    fn request(rig: &Rig, id: u64, plugin: &str) -> Result<(), String> {
        rig.reader.handle(HostMessage::Request {
            id,
            request: register_request(plugin),
        })
    }

    fn cancel(rig: &Rig, id: u64) {
        rig.reader
            .handle(HostMessage::Notification(HostNotification::Cancel(
                protocol::CancelParams { id },
            )))
            .unwrap();
    }

    /// The next frame the core wrote to the host, decoded.
    async fn next_frame(rig: &mut Rig) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), rig.frames.recv())
            .await
            .expect("a frame within 5 s")
            .expect("the channel is open");
        serde_json::from_slice(&frame[protocol::HEADER_LEN..]).expect("a JSON frame")
    }

    /// Nothing is written to the host for `ms`.
    async fn no_frame(rig: &mut Rig, ms: u64) {
        let frame = tokio::time::timeout(Duration::from_millis(ms), rig.frames.recv()).await;
        assert!(frame.is_err(), "unexpected frame: {frame:?}");
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn registry_requests_are_answered_through_the_generic_handler_as_before() {
        let mut rig = new_rig(Arc::new(Plain));
        request(&rig, 5, "p").unwrap();
        assert_eq!(
            next_frame(&mut rig).await,
            json!({"jsonrpc": "2.0", "id": 5, "result": {"handle": 9}})
        );
        rig.reader
            .handle(HostMessage::Request {
                id: 6,
                request: HostRequest::Unregister(protocol::UnregisterParams {
                    owner: owner("p"),
                    handle: 9,
                }),
            })
            .unwrap();
        assert_eq!(
            next_frame(&mut rig).await,
            json!({"jsonrpc": "2.0", "id": 6, "result": {}})
        );
        until("the table to empty", || rig.reader.inbound.in_flight() == 0).await;
    }

    #[tokio::test]
    async fn a_host_cancel_cancels_the_task_and_the_answer_is_dropped() {
        // A handler that stops when cancelled answers nothing.
        let stub = Stub::new(Behaviour::WaitsUnlessCancelled);
        let mut rig = new_rig(stub.clone());
        request(&rig, 1, "p").unwrap();
        until("the handler to start", || {
            stub.started.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(rig.reader.inbound.in_flight(), 1);
        cancel(&rig, 1);
        until("the handler to see the cancel", || {
            stub.saw_cancel.load(Ordering::SeqCst)
        })
        .await;
        no_frame(&mut rig, 150).await;
        until("the table to empty", || rig.reader.inbound.in_flight() == 0).await;
        // A cancel for an id that is not in flight (answered, or never sent) is ignored.
        cancel(&rig, 1);
        cancel(&rig, 4242);

        // A handler that ignores the cancel and finishes later: its late answer is dropped.
        let stub = Stub::new(Behaviour::IgnoresCancel);
        let mut rig = new_rig(stub.clone());
        request(&rig, 2, "p").unwrap();
        until("the handler to start", || {
            stub.started.load(Ordering::SeqCst) == 1
        })
        .await;
        cancel(&rig, 2);
        stub.release.notify_one();
        no_frame(&mut rig, 150).await;
        until("the table to empty", || rig.reader.inbound.in_flight() == 0).await;

        // One that never finishes is abandoned CANCEL_GRACE after the cancel,
        // and frees its slot.
        let mut rig = new_rig(Stub::new(Behaviour::Hangs));
        request(&rig, 3, "p").unwrap();
        cancel(&rig, 3);
        assert_eq!(
            rig.reader.inbound.in_flight(),
            1,
            "held until the grace ends"
        );
        until("the abandoned request to leave the table", || {
            rig.reader.inbound.in_flight() == 0
        })
        .await;
        no_frame(&mut rig, 100).await;
    }

    #[tokio::test]
    async fn revoking_an_owner_answers_its_requests_cancelled_and_leaves_others_alone() {
        let stub = Stub::new(Behaviour::WaitsUnlessCancelled);
        let mut rig = new_rig(stub.clone());
        request(&rig, 1, "a").unwrap();
        request(&rig, 2, "b").unwrap();
        until("both handlers to start", || {
            stub.started.load(Ordering::SeqCst) == 2
        })
        .await;
        rig.reader.inbound.cancel_owner("a");
        // The revoked owner's host is told (its own `$/cancel` never came).
        let frame = next_frame(&mut rig).await;
        assert_eq!(frame["id"], 1);
        assert_eq!(frame["error"]["code"], error_code::CANCELLED);
        assert_eq!(frame["error"]["message"], "extension was revoked");
        // The other owner's request is untouched and answers when released.
        assert_eq!(rig.reader.inbound.in_flight(), 1);
        stub.release.notify_one();
        assert_eq!(
            next_frame(&mut rig).await,
            json!({"jsonrpc": "2.0", "id": 2, "result": {"answered": 2}})
        );
    }

    #[tokio::test]
    async fn a_host_exit_cancels_every_request_answers_none_and_admits_no_more() {
        let stub = Stub::new(Behaviour::WaitsUnlessCancelled);
        let mut rig = new_rig(stub.clone());
        request(&rig, 1, "a").unwrap();
        request(&rig, 2, "b").unwrap();
        until("both handlers to start", || {
            stub.started.load(Ordering::SeqCst) == 2
        })
        .await;
        rig.reader.inbound.cancel_all();
        no_frame(&mut rig, 150).await;
        until("the table to empty", || rig.reader.inbound.in_flight() == 0).await;
        assert!(
            request(&rig, 3, "a")
                .unwrap_err()
                .contains("after the host exited")
        );
    }

    #[tokio::test]
    async fn host_requests_are_capped_and_an_id_cannot_be_reused_in_flight() {
        let rig = new_rig(Stub::new(Behaviour::Hangs));
        for id in 1..=protocol::MAX_INFLIGHT as u64 {
            request(&rig, id, "p").unwrap_or_else(|reason| panic!("request {id}: {reason}"));
        }
        assert_eq!(rig.reader.inbound.in_flight(), protocol::MAX_INFLIGHT);
        let over = request(&rig, 9999, "p").unwrap_err();
        assert!(
            over.contains("more than 256 host requests in flight"),
            "{over}"
        );
        let reused = request(&rig, 1, "p").unwrap_err();
        assert!(reused.contains("already in flight"), "{reused}");
        // Both are protocol violations (the reader ends the host); neither
        // disturbed what was admitted.
        assert_eq!(rig.reader.inbound.in_flight(), protocol::MAX_INFLIGHT);
        rig.reader.inbound.cancel_all();
    }
}

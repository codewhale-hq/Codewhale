//! Experimental TypeScript extension host — phase 1 (`[features] extension_host`).
//!
//! Codewhale's Rust core stays closed and authoritative: one turn loop, one
//! event authority, one store, one prompt authority, one approval gate. The
//! extension host (`crates/tui/extension-host`, a Bun or Node process running
//! the embedded bundle) is the only extensible surface, and in phase 1 it can do
//! exactly one thing: contribute tools, which become ordinary registry
//! `ToolSpec`s ([`tool::HostToolSpec`]) behind the existing gate.
//!
//! Lifecycle: a reviewed, enabled plugin with a `native` entry (activation
//! policy v4, selected by the flag) makes [`ExtensionHostManager::reconcile_in_background`]
//! spawn the host in the background — never on the first-prompt path — and
//! activate one owner per plugin. Tools join the per-turn registry at the next
//! rebuild, deferred. Disabling, revoking or updating the plugin revokes its
//! registrations synchronously before the host is asked to tear down.
//!
//! `core/call` ([`core_call`], tickets in [`ticket`]): an extension tool that
//! the model called directly may ask the core to run a core tool for it,
//! through the same gate as the model's own calls. The host decides nothing:
//! planning, approval and the card's text are Rust's, MCP and anything that
//! changes the session's authority are refused, and shell and network calls
//! force a prompt.
//!
//! Engines: the manager and its one host are process-wide, but each engine
//! holds its own [`HostAttachment`] carrying the plugin snapshot of its
//! workspace. Reconcile activates the union of what every attached snapshot
//! desires and revokes only owners no attachment desires. Each snapshot is
//! re-verified against persisted plugin state on every reconcile, so a
//! disable or revoke made through any registry revokes the plugin for every
//! engine. An engine installs only the tools of owners its own snapshot
//! desires, so a workspace never sees another workspace's project plugins.
//! Dropping an attachment detaches without revoking; the next reconcile
//! revokes whatever no remaining attachment desires. Engines without a
//! plugin snapshot of their own (isolated chats, the empty fallback) never
//! attach.
//!
//! Commands: a plugin may also contribute slash commands ([`command`]). They
//! are owned registrations like tools, loaded into the user command registry
//! at the lowest precedence, and run by `command/run` only when the user
//! invokes them.
//!
//! Plugin context: a plugin's `apply(ctx, config)` receives the settings from
//! `[plugins."<name>".config]` ([`plugin_config`]), and its tools and commands
//! are told, per call, the workspace the call comes from and the plugin's own
//! data directory. A plugin with several `native` entries activates them in
//! order under one owner.
//!
//! Known limitations (by design — see the design doc §8 and its "As built"
//! sections):
//! * Tools, slash commands, programmable pre-execute admission hooks, additive
//!   prompt sections and owner-local storage. Skills and MCP are not host services yet.
//!   The one thing the host may ask the core to do is a `core/call` from a tool
//!   under the turn's gate; commands, timers and activation code ask for
//!   nothing.
//! * Heartbeat and bounded automatic restart preserve the shared crash budget
//!   across engine creation and replay. Dead-host calls fail with a typed
//!   error and are never replayed. Three crashes in five minutes require an
//!   explicit plugin change/reload to retry. Two dirty teardowns within ten
//!   minutes retire the process once non-heartbeat calls are idle, without
//!   resetting or consuming the unexpected-crash budget.
//! * Every awaited core→host request is bounded by its method's deadline
//!   (`protocol::CoreRequest::deadline`) and then cancelled with `$/cancel`
//!   (`supervisor::HostProcess::call`). Cancellation is a request: a plugin
//!   that ignores its abort signal keeps running in the host until the host
//!   is torn down, though the core has already failed the call.
//! * One host per engine process and trust tier ([`tier`]): the plugin tier
//!   hosts reviewed third-party plugins, the builtin tier (tier 0) is for
//!   Codewhale's own host code and starts only for a row of
//!   [`tier::BUILTIN_MODULES`]. The MCP SDK backend starts its pinned module
//!   only when explicitly selected; plugin attachment does not start it.
//!   Under its OS sandbox
//!   (Seatbelt on macOS; bubblewrap on Linux when a launch-time probe shows it
//!   works) the host has no direct network, and cannot read the Codewhale
//!   home (except the bundle, its data dir and plugin code), the Codex and
//!   DSH credential homes, or the default credential stores
//!   (`supervisor::plan_launch`, whose module docs list what bubblewrap does
//!   not cover). Other files the user can read — including project `.env`
//!   files — stay readable, and Mach services are not restricted. On Windows,
//!   and on Linux where bwrap is missing or cannot start, Native launch is
//!   refused. The pinned Builtin exception is explicitly diagnosed and still
//!   ticket-bound. The flag remains Experimental.
//! * The runtime (`[extension_host] runtime`) defaults to Node. Bun is an
//!   opt-in (`bun`, or `auto`, which prefers a Bun >= 1.4.0 and uses Node
//!   when none is found) and is qualified on macOS only. The runtime is
//!   pinned once a host on it completes the handshake; restarts reuse it
//!   without probing it again, and the handshake refuses a host reporting a
//!   different runtime or version. Under `auto`, a Bun that fails to launch or
//!   handshake before anything is pinned is reported once and Node is used
//!   for the rest of the session; an explicit `bun`/`node` never falls back.
//!   The 1 GiB memory cap is applied through `RLIMIT_DATA` on Linux, the Job
//!   Object on Windows and, for a Bun host on macOS, a jetsam limit the host
//!   applies to itself; a macOS Node host is checked at each heartbeat
//!   instead. The Rust host tests have not run a Bun host on Linux or
//!   Windows (`supervisor` module docs say what was measured where).
//! * In-process native code is taken away from plugins by the host
//!   (`extension-host/src/runtime.ts`), for the entry points found so far; a
//!   native-code entry point a newer runtime adds is not covered until it is
//!   added there. A process a plugin starts is outside that policy and runs
//!   under the same verified OS sandbox required for Native admission.
//! * The owner token is a bug/staleness guard, not a boundary between
//!   plugins that share the process: one plugin can alter another's
//!   behaviour, which the approval card discloses.
//! * Extension tool names that any name-keyed approval table special-cases
//!   are refused (`registry::core_special_case`), so an extension tool never
//!   shares an approval key, summary or category with a built-in.
//! * The host's process tree (Unix process group / Windows Job Object,
//!   shared with hooks via `codewhale_runtime::process_tree`) is killed as a whole. On
//!   Windows the host is assigned to its job just after spawn, not created
//!   suspended as hooks are. On Unix a plugin child that calls `setsid` leaves
//!   the group and is not killed with it. When the core goes away, the host
//!   kills its own group at stdin EOF, and a watchdog thread does the same
//!   when its parent process changes, even if a plugin blocks the event loop.
//! * A running engine's snapshot is replaced only by its own workspace
//!   switch or by [`plugins_changed`] for the same workspace. A plugin newly
//!   enabled through another workspace's registry reaches an engine at its
//!   next snapshot, not at once; disables and revokes always reach it at the
//!   next reconcile, through the persisted-state check.
//! * The host re-hashes each `native` entry file before importing it; other
//!   files in the staged snapshot are covered by Rust's per-call receipt
//!   check, not re-hashed by the host.

pub(crate) mod command;
pub(crate) mod composition_review;
pub mod composition_scope;
pub(crate) mod core_call;
pub(crate) mod execution;
pub(crate) use execution::StockOperation;
pub(crate) mod avatars;
mod hooks;
pub(crate) mod mcp;
pub(crate) mod native_mcp;
pub(crate) mod plugin_config;
pub(crate) mod prompt;
pub(crate) mod protocol;
pub(crate) mod registry;
pub(crate) mod skills;
pub(crate) mod supervisor;
pub(crate) mod ticket;
pub(crate) mod tier;
pub(crate) mod tool;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod core_call_tests;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use self::protocol::{
    ActivateParams, ActivateResult, CoreRequest, DeactivateParams, DeactivateResult, RegisterKind,
    RegisterResult,
};
use self::registry::{CommandRegistration, OwnerRegistry, OwnerState, ToolRegistration};
use self::supervisor::{HostEvents, HostProcess, HostRequestContext};
use self::tier::{BuiltinModule, HostTier};
use crate::plugins::PluginRegistry;
use crate::plugins::activation::{self, PluginActivationCapability};
use codewhale_config::PluginAuthority;
use codewhale_config::{EntryRef, OwnerRef};

/// The host bundle, embedded so every distribution channel carries it.
const BUNDLE: &[u8] = include_bytes!("../../extension-host/dist/codewhale-extension-host.mjs");
const BUNDLE_FILE_NAME: &str = "codewhale-extension-host.mjs";
/// The licence notices of the third-party code inside [`BUNDLE`], generated by
/// the host build from the bundler's metafile (`extension-host/build.mjs`). The
/// bundle's banner promises "LICENSES.txt next to this file", so every
/// materialised copy of the bundle carries one.
const NOTICES: &[u8] = include_bytes!("../../extension-host/dist/LICENSES.txt");
const NOTICES_FILE_NAME: &str = "LICENSES.txt";
const MAX_DIAGNOSTICS: usize = 64;
const MAX_DIAGNOSTIC_BYTES: usize = 2048;
const DIRTY_RESTART_REASON: &str = "planned restart after repeated dirty teardowns";

struct Diagnostic {
    plugin_id: Option<String>,
    message: String,
}

fn bounded_diagnostic(mut message: String) -> String {
    if message.len() > MAX_DIAGNOSTIC_BYTES {
        let mut end = MAX_DIAGNOSTIC_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    message
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// SHA-256 of the embedded bundle.
#[must_use]
pub fn bundle_sha256() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| hex(Sha256::digest(BUNDLE)))
}

/// Write the embedded bundle, and the licence notices that belong with it, to
/// `<root>/extension-host/<sha256>/` unless identical copies are already there,
/// then re-verify the bytes on disk. A file is never overwritten in place: a
/// different build writes a different directory. Blocking.
fn materialize_bundle(root: &Path) -> Result<PathBuf, String> {
    let dir = supervisor::bundle_dir(root, bundle_sha256());
    let bundle = materialize_file(&dir, BUNDLE_FILE_NAME, BUNDLE)?;
    materialize_file(&dir, NOTICES_FILE_NAME, NOTICES)?;
    materialize_file(
        &dir.join("builtin"),
        "mcp.mjs",
        include_bytes!("../../extension-host/dist/builtin/mcp.mjs"),
    )?;
    materialize_file(
        &dir.join("builtin"),
        "harness.mjs",
        include_bytes!("../../extension-host/dist/builtin/harness.mjs"),
    )?;
    Ok(bundle)
}

/// One embedded file in `dir`: written to a private staging name, made
/// read-only, renamed into place (so a reader never sees a partial file and a
/// symlink at the final name is replaced, not followed), and read back.
fn materialize_file(dir: &Path, file_name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let path = dir.join(file_name);
    let matches = |path: &Path| std::fs::read(path).is_ok_and(|on_disk| on_disk == bytes);
    if !matches(&path) {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let staging = dir.join(format!(".{file_name}.{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&staging, bytes)
            .map_err(|error| format!("cannot write {}: {error}", staging.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o400));
        }
        if let Err(error) = std::fs::rename(&staging, &path) {
            let _ = std::fs::remove_file(&staging);
            if !matches(&path) {
                return Err(format!("cannot publish {}: {error}", path.display()));
            }
        }
    }
    // Re-check what will actually be used.
    if !matches(&path) {
        return Err(format!(
            "{} does not match the embedded copy (sha256 {})",
            path.display(),
            hex(Sha256::digest(bytes))
        ));
    }
    Ok(path)
}

/// Options fixed for the life of one manager.
#[derive(Debug, Clone, Default)]
pub struct ExtensionHostOptions {
    /// `[extension_host] runtime`: `node` (the default), `bun`, or `auto`
    /// (Bun first).
    pub runtime: codewhale_config::extension_host::ExtensionHostRuntime,
    /// `[extension_host] node`: tried before every `node` on `PATH`.
    pub node_override: Option<PathBuf>,
    /// `[extension_host] bun`: tried before every `bun` on `PATH`.
    pub bun_override: Option<PathBuf>,
    /// Where the bundle is materialized; defaults to the Codewhale home.
    pub root: Option<PathBuf>,
    /// Per-manager timings; tests can shorten them without global state.
    pub supervision: SupervisionOptions,
}

#[derive(Debug, Clone)]
pub struct SupervisionOptions {
    pub heartbeat_interval: Duration,
    pub ping_timeout: Duration,
    pub hang_timeout: Duration,
    pub restart_backoff: Duration,
    pub crash_window: Duration,
    pub crash_limit: usize,
    pub start_retry_cooldown: Duration,
    pub dirty_window: Duration,
    pub dirty_limit: usize,
    /// Host memory cap in bytes (`supervisor::HOST_MEMORY_CAP`).
    pub memory_cap: u64,
    /// How long one extension tool call may take before the core cancels it
    /// (`tool::TOOL_CALL_DEADLINE`); sent to the host as `deadline_ms`. The
    /// other methods' deadlines are fixed (`CoreRequest::deadline`).
    pub tool_call_deadline: Duration,
    /// How long the user waits for one extension command before the core
    /// cancels it (`command::COMMAND_RUN_DEADLINE`); sent to the host as
    /// `deadline_ms`.
    pub command_run_deadline: Duration,
}

impl ExtensionHostOptions {
    /// Options for the `[extension_host]` table (paths `~`-expanded).
    #[must_use]
    pub fn from_config(
        table: Option<&codewhale_config::extension_host::ExtensionHostConfig>,
    ) -> Self {
        let expand = |path: Option<&String>| {
            path.map(|path| PathBuf::from(shellexpand::tilde(path).as_ref()))
        };
        Self {
            runtime: table.map_or_else(Default::default, |table| table.effective_runtime()),
            node_override: expand(table.and_then(|table| table.node.as_ref())),
            bun_override: expand(table.and_then(|table| table.bun.as_ref())),
            ..Default::default()
        }
    }
}

impl Default for SupervisionOptions {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(3),
            ping_timeout: Duration::from_secs(3),
            hang_timeout: supervisor::PING_DEADLINE,
            restart_backoff: Duration::from_millis(250),
            crash_window: Duration::from_secs(5 * 60),
            crash_limit: 3,
            start_retry_cooldown: Duration::from_secs(60),
            dirty_window: Duration::from_secs(10 * 60),
            dirty_limit: 2,
            memory_cap: supervisor::HOST_MEMORY_CAP,
            tool_call_deadline: tool::TOOL_CALL_DEADLINE,
            command_run_deadline: command::COMMAND_RUN_DEADLINE,
        }
    }
}

#[derive(Default)]
struct SupervisionState {
    crashes: VecDeque<Instant>,
    last_start: Option<Instant>,
    launch_failed: bool,
    retry_ticket: u64,
    policy: bool,
    dirty_teardowns: VecDeque<Instant>,
    dirty_restart_pending: bool,
    planned_restart: Option<u64>,
}

impl SupervisionState {
    fn record_dirty_teardown(&mut self, now: Instant, options: &SupervisionOptions) {
        while self
            .dirty_teardowns
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= options.dirty_window)
        {
            self.dirty_teardowns.pop_front();
        }
        self.dirty_teardowns.push_back(now);
        let limit = options.dirty_limit.max(1);
        self.dirty_restart_pending |= self.dirty_teardowns.len() >= limit;
        while self.dirty_teardowns.len() > limit {
            self.dirty_teardowns.pop_front();
        }
    }

    fn record_crash(&mut self, now: Instant, options: &SupervisionOptions) -> bool {
        while self
            .crashes
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= options.crash_window)
        {
            self.crashes.pop_front();
        }
        self.crashes.push_back(now);
        self.launch_failed = false;
        self.retry_ticket += 1;
        self.crashes.len() < options.crash_limit
    }
}

/// Observable host state: the one source for `/plugin` and for the error a
/// tool call routed to the host gets while it is down ([`fmt::Display`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStatus {
    /// `[features] extension_host` is off for this process.
    Disabled,
    /// Not started: nothing has needed it yet.
    Idle,
    Starting,
    Ready {
        pid: Option<u32>,
        /// `bun` or `node`.
        runtime: &'static str,
        /// As reported by the host (`host/hello`).
        runtime_version: String,
        sandbox: supervisor::HostSandbox,
        /// How the memory cap is enforced for this process.
        memory: supervisor::MemoryEnforcement,
    },
    Unresponsive {
        pid: Option<u32>,
    },
    /// Crashed (or retired for maintenance); the supervisor restarts it.
    Restarting {
        reason: String,
    },
    /// Start refused (`start failed: …`) or crash budget exhausted; only an
    /// explicit plugin change or reload retries.
    Failed {
        reason: String,
        stderr_tail: String,
    },
}

/// Why the host can or cannot take a call, in one phrase.
impl fmt::Display for HostStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("disabled by config ([features] extension_host is off)"),
            Self::Idle => f.write_str(
                "not started (it starts in the background when a reviewed plugin with host code is enabled)",
            ),
            Self::Starting => f.write_str("starting"),
            Self::Ready { .. } => f.write_str("running"),
            Self::Unresponsive { pid } => write!(
                f,
                "unresponsive (pid {} missed its heartbeat; the supervisor restarts it if it stays silent)",
                pid.map_or_else(|| "?".to_string(), |pid| pid.to_string())
            ),
            Self::Restarting { reason } => write!(f, "restarting after: {reason}"),
            Self::Failed { reason, .. } => {
                write!(f, "{reason} (change or reload a plugin to retry)")
            }
        }
    }
}

enum HostSlot {
    Idle,
    Starting,
    Ready(Arc<HostProcess>),
    Unresponsive(Arc<HostProcess>),
    Restarting { reason: String },
    Failed { reason: String, stderr_tail: String },
}

impl HostSlot {
    fn status(&self) -> HostStatus {
        match self {
            Self::Idle => HostStatus::Idle,
            Self::Starting => HostStatus::Starting,
            Self::Ready(host) if host.is_retiring() => HostStatus::Restarting {
                reason: DIRTY_RESTART_REASON.to_string(),
            },
            // The exit watcher reports the exit a moment later.
            Self::Ready(host) | Self::Unresponsive(host) if host.has_exited() => {
                HostStatus::Restarting {
                    reason: "the host process exited".to_string(),
                }
            }
            Self::Ready(host) => HostStatus::Ready {
                pid: host.pid,
                runtime: host.runtime.kind.name(),
                runtime_version: host.runtime_version.get().cloned().unwrap_or_default(),
                sandbox: host.sandbox.clone(),
                memory: host.memory(),
            },
            Self::Unresponsive(host) => HostStatus::Unresponsive { pid: host.pid },
            Self::Restarting { reason } => HostStatus::Restarting {
                reason: reason.clone(),
            },
            Self::Failed {
                reason,
                stderr_tail,
            } => HostStatus::Failed {
                reason: reason.clone(),
                stderr_tail: stderr_tail.clone(),
            },
        }
    }
}

/// One tier's host process, and everything that goes with supervising it: the
/// slot, the generation that makes old callbacks harmless, the spawn count and
/// the crash/restart bookkeeping. The two tiers each own one, so a crash,
/// restart or failed launch of one never touches the other. The owner
/// registry, the runtime pin and the attachments stay shared.
struct TierRuntime {
    host: Mutex<HostSlot>,
    host_generation: AtomicU64,
    spawn_attempts: AtomicU64,
    /// Lock order: see [`ManagerShared`].
    supervision: Mutex<SupervisionState>,
}

impl TierRuntime {
    fn new() -> Self {
        Self {
            host: Mutex::new(HostSlot::Idle),
            host_generation: AtomicU64::new(0),
            spawn_attempts: AtomicU64::new(0),
            supervision: Mutex::new(SupervisionState::default()),
        }
    }
}

struct DesiredOwner {
    plugin_name: String,
    authority: PluginAuthority,
    entries: Vec<(PathBuf, String)>,
}

/// What [`ExtensionHostManager::activate_owner`] activates, on either tier:
/// a plugin's reviewed bytes, or a built-in module's pinned source.
struct Activation {
    tier: HostTier,
    owner_id: String,
    /// The plugin's manifest name, or the module's name.
    name: String,
    /// The reviewed plugin authority; `None` for a built-in module.
    authority: Option<PluginAuthority>,
    /// What the owner is bound to: the plugin's content hash, or the module's
    /// pinned source digest.
    content_hash: String,
    entries: Vec<(PathBuf, String)>,
}

/// One engine's view: its plugin snapshot and the owners (plugin id →
/// reviewed content hash) that snapshot desired at the last reconcile.
struct AttachmentState {
    plugins: Arc<PluginRegistry>,
    desired: BTreeMap<String, String>,
    selection: composition_scope::CompositionSelection,
    revision: u64,
    selection_cancel: CancellationToken,
    settled: Option<composition_scope::SelectionRevision>,
    session_id: Option<String>,
    agent_id: Option<String>,
}

/// Lock order, outermost first, everywhere in this module:
///
/// 1. `sync_lock` (async; the only lock held across an await, and it only
///    serializes reconciliation);
/// 2. one tier's `host` slot, then that same tier's `supervision`;
/// 3. `registry`.
///
/// No code holds both tiers' slots or both tiers' supervision state at once:
/// a path that must visit both tiers visits one, releases it, then the other.
/// `attachments`, `runtime`, `plugin_configs` and `diagnostics` are leaf
/// locks, never held together with another lock (`diagnostics` is only ever
/// taken last, from the `diagnostic` helpers). No lock but `sync_lock` is held
/// across an await.
pub(crate) struct ManagerShared {
    options: ExtensionHostOptions,
    /// The built-in host modules this manager may start the builtin tier for:
    /// [`tier::BUILTIN_MODULES`] in production; a test substitutes its
    /// own through [`ExtensionHostManager::with_builtin_modules`].
    builtin_modules: &'static [BuiltinModule],
    attachments: Mutex<BTreeMap<u64, AttachmentState>>,
    next_attachment: AtomicU64,
    /// One registry for both tiers: an owner's entry records which.
    registry: Mutex<OwnerRegistry>,
    /// Capability tickets and the invocations they belong to (`core/call`).
    /// A leaf lock domain: its mutexes are never held with another lock.
    core_calls: Arc<core_call::CoreCalls>,
    engine_handle: OnceLock<tokio::runtime::Handle>,
    execution_broker: execution::Broker,
    harness_users: AtomicU64,
    stock_users: AtomicU64,
    mcp_broker: mcp::Broker,
    mcp_users: AtomicU64,
    /// Tier 1: reviewed third-party plugins.
    plugin: TierRuntime,
    /// Tier 0: Codewhale's own host code. Starts only for a built-in module.
    builtin: TierRuntime,
    sync_lock: tokio::sync::Mutex<()>,
    /// Bound native root parsing independently of reconciliation (activation
    /// itself calls admission while reconciliation owns sync_lock).
    skill_admission: Arc<tokio::sync::Semaphore>,
    diagnostics: Mutex<VecDeque<Diagnostic>>,
    /// Which runtime every host (of either tier) runs on.
    runtime: Mutex<RuntimePin>,
    /// User settings per plugin name.
    plugin_configs: Mutex<plugin_config::PluginConfigs>,
}

/// Which runtime this manager's hosts run on.
#[derive(Default)]
struct RuntimePin {
    /// Set by the first host that completes the handshake and reused by
    /// every restart, so a session never switches runtime (a `bun` install
    /// or removal mid-session changes nothing until the next process; a
    /// binary replaced at the pinned path is refused at the handshake). With
    /// the one-line summary of why it was chosen, for `/plugin`.
    pinned: Option<(codewhale_dependencies::HostRuntime, String)>,
    /// `runtime = "auto"` only: a Bun host failed to launch or handshake
    /// before anything was pinned, so every later launch this session
    /// resolves Node.
    bun_failed: bool,
}

/// Resolve the runtime unless one is pinned, materialize the bundle and plan
/// the launch. Returns the summary to pin once a host on this runtime
/// completes the handshake, or `None` when the runtime was already pinned.
/// Blocking.
fn prepare_launch(
    options: &ExtensionHostOptions,
    tier: HostTier,
    pinned: Option<(codewhale_dependencies::HostRuntime, String)>,
    bun_failed: bool,
    snapshot_state_paths: &[PathBuf],
) -> Result<(supervisor::HostLaunch, Option<String>), String> {
    let (runtime, summary) = match pinned {
        Some((runtime, _)) => (runtime, None),
        None => {
            let choice = if bun_failed {
                codewhale_config::extension_host::ExtensionHostRuntime::Node
            } else {
                options.runtime
            };
            let mut resolution = codewhale_dependencies::resolve_extension_host_runtime(
                choice,
                options.node_override.as_deref(),
                options.bun_override.as_deref(),
                crate::extension_host::bundle_sha256(),
            );
            // Report the configured choice, not the Node-only fallback.
            resolution.choice = options.runtime;
            let note = if bun_failed {
                "; Bun failed to start this session (see diagnostics)"
            } else {
                ""
            };
            let Some(runtime) = resolution.selected.clone() else {
                return Err(format!("{}{note}", resolution.failure()));
            };
            (runtime, Some(format!("{}{note}", resolution.summary())))
        }
    };
    let root = host_root(options)?;
    let bundle = materialize_bundle(&root)?;
    let launch = supervisor::plan_launch(
        tier,
        &runtime,
        &bundle,
        &root,
        options.supervision.memory_cap,
        snapshot_state_paths,
    )?;
    Ok((launch, summary))
}

/// The Codewhale home the host lives under.
fn host_root(options: &ExtensionHostOptions) -> Result<PathBuf, String> {
    match options.root.clone() {
        Some(root) => Ok(root),
        None => codewhale_config::codewhale_home()
            .map_err(|error| format!("Codewhale home unavailable: {error}")),
    }
}

/// The selected tier's sandbox, planned exactly as a launch does (for doctor).
/// Blocking; creates the host's data dir and probes bubblewrap on Linux.
pub(crate) fn planned_sandbox(
    options: &ExtensionHostOptions,
    runtime: &codewhale_dependencies::HostRuntime,
    tier: HostTier,
) -> Result<supervisor::HostSandbox, String> {
    supervisor::planned_sandbox(tier, runtime, &host_root(options)?)
}

impl ManagerShared {
    pub(crate) fn engine_handle(&self) -> Result<tokio::runtime::Handle, String> {
        self.engine_handle.get().cloned().ok_or_else(|| "enabled execution requires the Engine scheduler; no new runtime or legacy fallback is allowed".to_string())
    }

    /// The supervision state of `tier`'s host.
    fn tier_runtime(&self, tier: HostTier) -> &TierRuntime {
        match tier {
            HostTier::Plugin => &self.plugin,
            HostTier::Builtin => &self.builtin,
        }
    }

    async fn deactivate_owner(&self, host: &Arc<HostProcess>, owner: &OwnerRef) {
        self.deactivate_entry(host, owner, None).await;
    }
    /// A failed or retired Native entry must produce the same bounded teardown
    /// receipt as a whole owner, while its other selected entries stay live.
    async fn deactivate_entry(
        &self,
        host: &Arc<HostProcess>,
        owner: &OwnerRef,
        entry: Option<EntryRef>,
    ) {
        let diagnostic = match host
            .call(
                CoreRequest::Deactivate(DeactivateParams {
                    entry,
                    owner: owner.clone(),
                }),
                None,
            )
            .await
            .map(serde_json::from_value::<DeactivateResult>)
        {
            Ok(Ok(ack)) if ack.disposed && ack.leaked.is_empty() => return,
            Ok(Ok(ack)) => format!(
                "extension `{}` teardown incomplete (disposed: {}, leaked: {:?})",
                owner.plugin_id, ack.disposed, ack.leaked
            ),
            Ok(Err(error)) => format!(
                "extension `{}` teardown answer malformed: {error}",
                owner.plugin_id
            ),
            Err(error) => format!("extension `{}` teardown failed: {error}", owner.plugin_id),
        };
        self.plugin_diagnostic(&owner.plugin_id, diagnostic);
        self.record_dirty_teardown(host);
    }

    fn record_dirty_teardown(&self, host: &Arc<HostProcess>) {
        let runtime = self.tier_runtime(host.tier);
        let slot = runtime.host.lock().expect("host lock");
        if !matches!(&*slot, HostSlot::Ready(current) | HostSlot::Unresponsive(current)
            if Arc::ptr_eq(current, host))
        {
            return;
        }
        runtime
            .supervision
            .lock()
            .expect("supervision lock")
            .record_dirty_teardown(Instant::now(), &self.options.supervision);
    }

    fn diagnostic(&self, message: String) {
        self.record_diagnostic(None, message);
    }

    fn plugin_diagnostic(&self, plugin_id: &str, message: String) {
        // Refused registrations can contain an arbitrary host-supplied id.
        // Keep an oversized id global instead of retaining unbounded metadata
        // or truncating it into another plugin's identity.
        let plugin_id = (plugin_id.len() <= MAX_DIAGNOSTIC_BYTES).then(|| plugin_id.to_string());
        self.record_diagnostic(plugin_id, message);
    }

    fn record_diagnostic(&self, plugin_id: Option<String>, message: String) {
        let message = bounded_diagnostic(message);
        tracing::info!(target: "extension_host", "{message}");
        let mut diagnostics = self.diagnostics.lock().expect("diagnostics lock");
        diagnostics.push_back(Diagnostic { plugin_id, message });
        while diagnostics.len() > MAX_DIAGNOSTICS {
            diagnostics.pop_front();
        }
    }

    /// `tier`'s host, when it can take a call; otherwise why not.
    fn ready_host(&self, tier: HostTier) -> Result<Arc<HostProcess>, HostStatus> {
        match &*self.tier_runtime(tier).host.lock().expect("host lock") {
            HostSlot::Ready(host) if !host.has_exited() && !host.is_retiring() => {
                Ok(Arc::clone(host))
            }
            slot => Err(slot.status()),
        }
    }

    /// The shared fast liveness guard: current policy, running host, exact
    /// owner generation/tier and its reviewed authority. `live_host` follows
    /// with Native receipt validation and another host check; root admission
    /// uses the same authority inside its one bounded blocking disk job.
    ///
    /// `owner_if_live` runs under the registry lock and names the owner
    /// generation the call belongs to, or why it is no longer registered.
    fn live_owner_authority(
        &self,
        tier: HostTier,
        owner_if_live: impl FnOnce(&OwnerRegistry) -> Result<OwnerRef, String>,
    ) -> Result<Option<PluginAuthority>, String> {
        let native_policy = activation::extension_host_policy_enabled();
        if tier == HostTier::Plugin && !native_policy {
            return Err(host_down(tier, &HostStatus::Disabled));
        }
        self.ready_host(tier)
            .map_err(|status| host_down(tier, &status))?;
        {
            let registry = self.registry.lock().expect("registry lock");
            let owner = owner_if_live(&registry)?;
            // Independent Core demand admits only the exact pinned module.
            // Script/hook jobs still require Native policy in Caller::check.
            let core_stock =
                owner.plugin_id == "host:harness" && self.stock_users.load(Ordering::SeqCst) > 0;
            if !native_policy && owner.plugin_id != "host:mcp" && !core_stock {
                return Err(host_down(tier, &HostStatus::Disabled));
            }
            let owner_tier = registry
                .tier_of(&owner)
                .ok_or_else(|| "extension owner has no authority".to_string())?;
            if owner_tier != tier {
                return Err(format!(
                    "extension owner `{}` belongs to the {} tier, not the {} tier",
                    owner.plugin_id,
                    owner_tier.name(),
                    tier.name()
                ));
            }
            match (tier, registry.authority_for(&owner)) {
                (HostTier::Plugin, Some(authority)) => Ok(Some(authority)),
                (HostTier::Builtin, None) => Ok(None),
                _ => Err("extension owner has no authority".to_string()),
            }
        }
    }

    async fn live_host(
        &self,
        tier: HostTier,
        owner_if_live: impl FnOnce(&OwnerRegistry) -> Result<OwnerRef, String>,
    ) -> Result<Arc<HostProcess>, String> {
        let policy = activation::extension_host_policy_enabled();
        let authority = self.live_owner_authority(tier, owner_if_live)?;
        if let Some(authority) = authority {
            #[cfg(test)]
            let env_scope = crate::test_support::env_scope_ticket();
            tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _scope = activation::PolicyScope::propagate(policy);
                crate::plugins::registry::verify_plugin_component_authority(
                    &authority,
                    PluginActivationCapability::Native,
                )
            })
            .await
            .map_err(|error| format!("authority check failed: {error}"))??;
        }
        self.ready_host(tier)
            .map_err(|status| host_down(tier, &status))
    }

    pub(crate) async fn live_host_for(
        &self,
        registration: &ToolRegistration,
    ) -> Result<Arc<HostProcess>, String> {
        self.live_host(registration.tier, |registry| {
            if registry.is_live(registration.handle, &registration.owner) {
                Ok(registration.owner.clone())
            } else {
                Err(format!(
                    "extension tool `{}` from `{}` is no longer registered",
                    registration.name, registration.plugin_name
                ))
            }
        })
        .await
    }

    /// [`Self::live_host`] for a command the user just invoked: the exact
    /// registration (handle and owner generation) must still be admitted.
    pub(crate) async fn live_host_for_command(
        &self,
        command: &command::ExtensionCommandRef,
    ) -> Result<(Arc<HostProcess>, CommandRegistration), String> {
        let mut found = None;
        let host = self
            .live_host(HostTier::Plugin, |registry| {
                let registration = registry
                    .live_command(command.handle, &command.plugin_id, command.generation)
                    .ok_or_else(|| {
                        format!(
                            "extension command from `{}` is no longer registered (the plugin was reloaded, disabled or lost trust)",
                            command.plugin_id
                        )
                    })?;
                let owner = registration.owner.clone();
                found = Some(registration);
                Ok(owner)
            })
            .await?;
        Ok((host, found.expect("set when the owner was found")))
    }
}

/// The error a call gets while the host of `tier` cannot take it.
fn host_down(tier: HostTier, status: &HostStatus) -> String {
    format!("{} is down: {status}", tier.host_label())
}

/// Channel callbacks. Holds a `Weak` so the host process (which owns the
/// callbacks) never keeps the manager alive. Each belongs to one tier's host
/// process and generation.
struct Events {
    shared: Weak<ManagerShared>,
    tier: HostTier,
    generation: u64,
}

#[async_trait]
impl HostEvents for Events {
    fn responded(&self) {
        if let Some(shared) = self.shared.upgrade() {
            // Completing a live request is evidence of recovery before its
            // caller's post-await liveness checks. The pending ping retains
            // its original hang deadline; this creates no new health clock.
            set_host_health(&shared, self.tier, self.generation, false);
        }
    }

    async fn host_request(
        &self,
        request: protocol::HostRequest,
        cx: HostRequestContext,
    ) -> Result<serde_json::Value, protocol::RpcErrorWire> {
        let refuse = |message: &str| protocol::RpcErrorWire {
            code: protocol::error_code::REFUSED,
            message: message.to_string(),
            data: None,
        };
        match request {
            protocol::HostRequest::ExecutionRedeem(params) => {
                let shared = self
                    .shared
                    .upgrade()
                    .ok_or_else(|| refuse("extension host manager is gone"))?;
                if self.tier != HostTier::Builtin {
                    return Err(refuse("execution broker is builtin-only"));
                }
                shared
                    .execution_broker
                    .serve(&shared, self.generation, params, cx)
                    .await
            }
            protocol::HostRequest::UserQuestion(params) => {
                let shared = self
                    .shared
                    .upgrade()
                    .ok_or_else(|| refuse("extension host manager is gone"))?;
                if shared
                    .tier_runtime(self.tier)
                    .host_generation
                    .load(Ordering::SeqCst)
                    != self.generation
                {
                    return Err(refuse("stale host generation"));
                }
                shared
                    .core_calls
                    .serve_question(&shared, self.tier, self.generation, params, cx)
                    .await
            }
            protocol::HostRequest::SessionCheckpoint(params) => {
                let shared = self
                    .shared
                    .upgrade()
                    .ok_or_else(|| refuse("extension host manager is gone"))?;
                if shared
                    .tier_runtime(self.tier)
                    .host_generation
                    .load(Ordering::SeqCst)
                    != self.generation
                {
                    return Err(refuse("stale host generation"));
                }
                shared
                    .core_calls
                    .serve_checkpoint(&shared, self.tier, self.generation, params, cx)
                    .await
            }
            protocol::HostRequest::CoreCall(params) => {
                let Some(shared) = self.shared.upgrade() else {
                    return Err(refuse("extension host manager is gone"));
                };
                if shared
                    .tier_runtime(self.tier)
                    .host_generation
                    .load(Ordering::SeqCst)
                    != self.generation
                {
                    return Err(refuse("stale host generation"));
                }
                shared
                    .core_calls
                    .serve(&shared, self.tier, self.generation, params, cx)
                    .await
            }
            protocol::HostRequest::Register(params) if params.kind == RegisterKind::McpServer => {
                let shared = self
                    .shared
                    .upgrade()
                    .ok_or_else(|| refuse("extension host manager is gone"))?;
                let result =
                    native_mcp::admit(&shared, self.tier, self.generation, params, &cx).await;
                Ok(serde_json::to_value(result).expect("registration result is JSON"))
            }
            protocol::HostRequest::Register(params) if params.kind == RegisterKind::AvatarPack => {
                let Some(shared) = self.shared.upgrade() else {
                    return Err(refuse("extension host manager is gone"));
                };
                let result = avatars::admit(&shared, self.tier, self.generation, params, &cx).await;
                Ok(serde_json::to_value(result).expect("registration result is JSON"))
            }
            protocol::HostRequest::Register(params) if params.kind == RegisterKind::SkillRoot => {
                let Some(shared) = self.shared.upgrade() else {
                    return Err(refuse("extension host manager is gone"));
                };
                let result =
                    skills::admit_root(&shared, self.tier, self.generation, params, &cx).await;
                Ok(serde_json::to_value(result).expect("registration result is JSON"))
            }
            request @ (protocol::HostRequest::ProcLaunch(_)
            | protocol::HostRequest::ProcRead(_)
            | protocol::HostRequest::ProcWrite(_)
            | protocol::HostRequest::ProcClose(_)
            | protocol::HostRequest::NetStart(_)
            | protocol::HostRequest::NetFetch(_)
            | protocol::HostRequest::NetRead(_)
            | protocol::HostRequest::NetRelease(_)
            | protocol::HostRequest::NetClose(_)) => {
                let shared = self
                    .shared
                    .upgrade()
                    .ok_or_else(|| refuse("extension host manager is gone"))?;
                if self.tier != HostTier::Builtin {
                    return Err(refuse("MCP broker is builtin-only"));
                }
                shared
                    .mcp_broker
                    .serve(&shared, self.generation, request, cx)
                    .await
            }
            other => supervisor::registry_host_request(self, other, &cx),
        }
    }

    fn register(&self, params: &protocol::RegisterParams) -> RegisterResult {
        let Some(shared) = self.shared.upgrade() else {
            return RegisterResult::Refused {
                refused: "extension host manager is gone".to_string(),
            };
        };
        let runtime = shared.tier_runtime(self.tier);
        let slot = runtime.host.lock().expect("host lock");
        if runtime.host_generation.load(Ordering::SeqCst) != self.generation {
            return RegisterResult::Refused {
                refused: "stale host generation".to_string(),
            };
        }
        let mut foreign_owner = false;
        let result = {
            let mut registry = shared.registry.lock().expect("registry lock");
            // A host registers only for owners it was asked to activate: one
            // of its own tier. The owner token already makes another tier's
            // owner unreachable; this says so out loud.
            match registry.owner(&params.owner.plugin_id) {
                Some(entry) if entry.tier != self.tier => {
                    foreign_owner = true;
                    Err(format!(
                        "owner `{}` belongs to the {} tier, not this host's {} tier",
                        params.owner.plugin_id,
                        entry.tier.name(),
                        self.tier.name()
                    ))
                }
                _ => registry.register(params),
            }
        };
        drop(slot);
        match result {
            Ok(handle) => RegisterResult::Admitted { handle },
            Err(reason) => {
                let kind = match params.kind {
                    RegisterKind::Tool => "tool",
                    RegisterKind::Command => "command",
                    RegisterKind::Hook => "hook",
                    RegisterKind::PromptSection => "prompt section",
                    RegisterKind::PromptTemplate => "prompt template",
                    RegisterKind::SkillRoot => "skill root",
                    RegisterKind::AvatarPack => "avatar pack",
                    RegisterKind::ShellHook => "shell hook",
                    RegisterKind::McpServer => "MCP server",
                };
                let message = format!(
                    "extension `{}` {kind} `{}` refused: {reason}",
                    params.owner.plugin_id, params.spec.name
                );
                // A host naming the other tier's owner does not get to write
                // under that owner's id: the refusal is a global diagnostic.
                if foreign_owner {
                    shared.diagnostic(message);
                } else {
                    shared.plugin_diagnostic(&params.owner.plugin_id, message);
                }
                RegisterResult::Refused { refused: reason }
            }
        }
    }

    fn unregister(&self, params: &protocol::UnregisterParams) {
        if let Some(shared) = self.shared.upgrade() {
            shared
                .registry
                .lock()
                .expect("registry lock")
                .unregister(&params.owner, params.handle);
        }
    }

    fn faulted(&self, params: &protocol::FaultedParams) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let runtime = shared.tier_runtime(self.tier);
        let slot = runtime.host.lock().expect("host lock");
        if runtime.host_generation.load(Ordering::SeqCst) != self.generation {
            return;
        }
        if !shared
            .registry
            .lock()
            .expect("registry lock")
            .mark_failed(&params.owner, OwnerState::Faulted(params.error.clone()))
        {
            return;
        }
        if let HostSlot::Ready(host) | HostSlot::Unresponsive(host) = &*slot {
            host.revoke_calls_of(&params.owner.plugin_id);
        }
        drop(slot);
        shared.core_calls.revoke_owner(&params.owner.plugin_id);
        shared
            .execution_broker
            .revoke_owner(&params.owner.plugin_id);
        shared.mcp_users.fetch_sub(
            shared.mcp_broker.revoke_owner(&params.owner.plugin_id),
            Ordering::SeqCst,
        );
        shared.plugin_diagnostic(
            &params.owner.plugin_id,
            format!(
                "extension `{}` faulted and was disposed: {}",
                params.owner.plugin_id, params.error
            ),
        );
    }

    fn exited(&self, host_generation: u64, reason: String, stderr_tail: String) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // Whatever this process was asked for, nothing it held is good again.
        shared.core_calls.revoke_host(self.tier, host_generation);
        shared
            .execution_broker
            .revoke_host(self.tier, host_generation);
        shared.mcp_users.fetch_sub(
            shared.mcp_broker.revoke_host(self.tier, host_generation),
            Ordering::SeqCst,
        );
        let runtime = shared.tier_runtime(self.tier);
        let retry = {
            let mut slot = runtime.host.lock().expect("host lock");
            if runtime.host_generation.load(Ordering::SeqCst) != host_generation
                || !matches!(&*slot, HostSlot::Ready(_) | HostSlot::Unresponsive(_))
            {
                return;
            }
            let mut supervision = runtime.supervision.lock().expect("supervision lock");
            let planned = supervision.planned_restart.take() == Some(host_generation)
                && reason == DIRTY_RESTART_REASON;
            let restart = if planned {
                supervision.retry_ticket += 1;
                true
            } else {
                supervision.record_crash(Instant::now(), &shared.options.supervision)
            };
            shared
                .registry
                .lock()
                .expect("registry lock")
                .host_exited(self.tier, &reason);
            *slot = if restart {
                HostSlot::Restarting {
                    reason: reason.clone(),
                }
            } else {
                HostSlot::Failed {
                    reason: format!("crash budget exhausted: {reason}"),
                    stderr_tail,
                }
            };
            restart.then_some((supervision.retry_ticket, supervision.policy))
        };
        shared.diagnostic(format!("{} {reason}", self.tier.host_label()));
        if let Some((ticket, policy)) = retry {
            schedule_restart(&shared, self.tier, host_generation, ticket, policy);
        }
    }

    fn log(&self, params: &protocol::LogParams) {
        if !matches!(params.level.as_str(), "warn" | "error") {
            return;
        }
        let Some(plugin_id) = params.plugin_id.as_deref() else {
            return;
        };
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let runtime = shared.tier_runtime(self.tier);
        let _slot = runtime.host.lock().expect("host lock");
        // Only for an owner of this host's own tier: a host cannot put
        // diagnostics under the other tier's owner ids.
        if runtime.host_generation.load(Ordering::SeqCst) != self.generation
            || !shared
                .registry
                .lock()
                .expect("registry lock")
                .owner(plugin_id)
                .is_some_and(|entry| entry.tier == self.tier)
        {
            return;
        }
        shared.plugin_diagnostic(plugin_id, format!("{}: {}", params.level, params.msg));
    }
}

/// One scheduled retry owns a ticket, so explicit retry/shutdown and a newer
/// host generation invalidate it. The existing reconcile lock owns replay.
fn schedule_restart(
    shared: &Arc<ManagerShared>,
    tier: HostTier,
    generation: u64,
    ticket: u64,
    policy: bool,
) {
    let weak = Arc::downgrade(shared);
    let backoff = shared.options.supervision.restart_backoff;
    tokio::spawn(async move {
        tokio::time::sleep(backoff).await;
        let Some(shared) = weak.upgrade() else {
            return;
        };
        {
            let _serial = shared.sync_lock.lock().await;
            let runtime = shared.tier_runtime(tier);
            let mut slot = runtime.host.lock().expect("host lock");
            if runtime.host_generation.load(Ordering::SeqCst) != generation
                || runtime
                    .supervision
                    .lock()
                    .expect("supervision lock")
                    .retry_ticket
                    != ticket
                || !matches!(&*slot, HostSlot::Restarting { .. })
            {
                return;
            }
            *slot = HostSlot::Idle;
        }
        let manager = ExtensionHostManager { shared };
        if let Err(error) = manager.reconcile_with_policy(policy).await {
            manager.shared.diagnostic(error);
        }
    });
}

fn set_host_health(
    shared: &ManagerShared,
    tier: HostTier,
    generation: u64,
    unresponsive: bool,
) -> bool {
    let runtime = shared.tier_runtime(tier);
    let mut slot = runtime.host.lock().expect("host lock");
    if runtime.host_generation.load(Ordering::SeqCst) != generation {
        return false;
    }
    let host = match &*slot {
        HostSlot::Ready(host) | HostSlot::Unresponsive(host) => Arc::clone(host),
        _ => return false,
    };
    *slot = if unresponsive {
        HostSlot::Unresponsive(host)
    } else {
        HostSlot::Ready(host)
    };
    true
}

fn restart_dirty_host_when_idle(
    shared: &ManagerShared,
    host: &Arc<HostProcess>,
    generation: u64,
) -> bool {
    // Reconciliation owns activation/deactivation between wire requests too.
    let Ok(_serial) = shared.sync_lock.try_lock() else {
        return false;
    };
    let runtime = shared.tier_runtime(host.tier);
    let slot = runtime.host.lock().expect("host lock");
    if runtime.host_generation.load(Ordering::SeqCst) != generation
        || !matches!(&*slot, HostSlot::Ready(current) if Arc::ptr_eq(current, host))
    {
        return false;
    }
    let mut supervision = runtime.supervision.lock().expect("supervision lock");
    if !supervision.dirty_restart_pending || !host.terminate_if_idle(DIRTY_RESTART_REASON) {
        return false;
    }
    supervision.planned_restart = Some(generation);
    true
}

/// A monitor never owns the manager. Dropping the manager or changing host
/// generation stops its monitor; pending calls are never retried here.
fn monitor_host(shared: &Arc<ManagerShared>, host: &Arc<HostProcess>, generation: u64) {
    let tier = host.tier;
    let weak = Arc::downgrade(shared);
    let host = Arc::clone(host);
    let options = shared.options.supervision.clone();
    tokio::spawn(async move {
        let mut blocked_since = None;
        loop {
            tokio::time::sleep(options.heartbeat_interval).await;
            let Some(shared) = weak.upgrade() else {
                return;
            };
            if shared
                .tier_runtime(tier)
                .host_generation
                .load(Ordering::SeqCst)
                != generation
                || host.has_exited()
            {
                return;
            }
            if restart_dirty_host_when_idle(&shared, &host, generation) {
                return;
            }
            // Only where no kernel limit is planned (a macOS Node host).
            if host.memory() == supervisor::MemoryEnforcement::Heartbeat
                && let Some(resident) = host.pid.and_then(supervisor::resident_bytes)
                && resident > host.memory_cap
            {
                shared.diagnostic(format!(
                    "{} exceeded its memory cap ({} MiB resident, cap {} MiB); killed",
                    tier.host_label(),
                    resident / (1024 * 1024),
                    host.memory_cap / (1024 * 1024)
                ));
                drop(shared);
                host.terminate("exceeded the memory cap".into());
                return;
            }
            drop(shared);
            let (id, mut answer) = match host.start_request(CoreRequest::Ping, None) {
                Ok(request) => {
                    blocked_since = None;
                    request
                }
                Err(supervisor::HostCallError::Busy) => {
                    // A full outbound queue can itself be caused by a hung
                    // host. Bound that wait too instead of skipping forever.
                    let elapsed = blocked_since.get_or_insert_with(Instant::now).elapsed();
                    if elapsed >= options.hang_timeout {
                        host.terminate("heartbeat queue remained blocked".into());
                        return;
                    }
                    if elapsed >= options.ping_timeout {
                        let Some(shared) = weak.upgrade() else {
                            return;
                        };
                        if !set_host_health(&shared, tier, generation, true) {
                            return;
                        }
                    }
                    continue;
                }
                Err(_) => return,
            };
            let result = match tokio::time::timeout(options.ping_timeout, &mut answer).await {
                Ok(result) => result,
                Err(_) => {
                    let Some(shared) = weak.upgrade() else {
                        host.forget(id);
                        return;
                    };
                    if !set_host_health(&shared, tier, generation, true) {
                        host.forget(id);
                        return;
                    }
                    drop(shared);
                    let remaining = options.hang_timeout.saturating_sub(options.ping_timeout);
                    match tokio::time::timeout(remaining, answer).await {
                        Ok(result) => result,
                        Err(_) => {
                            host.forget(id);
                            host.terminate("heartbeat timed out".into());
                            return;
                        }
                    }
                }
            };
            host.forget(id);
            if !matches!(result, Ok(Ok(serde_json::Value::Object(ref object))) if object.is_empty())
            {
                if !host.has_exited() {
                    host.terminate("invalid heartbeat response".into());
                }
                return;
            }
            let Some(shared) = weak.upgrade() else {
                return;
            };
            if !set_host_health(&shared, tier, generation, false) {
                return;
            }
        }
    });
}

/// Supervises at most one extension host per trust tier for this engine
/// process: the plugin tier's, and the builtin tier's once a built-in module
/// asks for it (none does yet).
pub struct ExtensionHostManager {
    pub(crate) shared: Arc<ManagerShared>,
}

impl ExtensionHostManager {
    #[must_use]
    pub fn new(options: ExtensionHostOptions) -> Self {
        Self::with_builtin_modules(options, tier::BUILTIN_MODULES)
    }

    /// A manager with its own built-in module table. Production passes the
    /// const table through [`Self::new`]; a test passes one of its own to
    /// exercise tier 0 without a production row.
    fn with_builtin_modules(
        options: ExtensionHostOptions,
        builtin_modules: &'static [BuiltinModule],
    ) -> Self {
        Self {
            shared: Arc::new(ManagerShared {
                options,
                builtin_modules,
                attachments: Mutex::new(BTreeMap::new()),
                next_attachment: AtomicU64::new(0),
                registry: Mutex::new(OwnerRegistry::new()),
                core_calls: Arc::default(),
                engine_handle: OnceLock::new(),
                execution_broker: execution::Broker::default(),
                harness_users: AtomicU64::new(0),
                stock_users: AtomicU64::new(0),
                mcp_broker: mcp::Broker::default(),
                mcp_users: AtomicU64::new(0),
                plugin: TierRuntime::new(),
                builtin: TierRuntime::new(),
                sync_lock: tokio::sync::Mutex::new(()),
                skill_admission: Arc::new(tokio::sync::Semaphore::new(1)),
                diagnostics: Mutex::new(VecDeque::new()),
                runtime: Mutex::new(RuntimePin::default()),
                plugin_configs: Mutex::new(plugin_config::PluginConfigs::default()),
            }),
        }
    }

    /// Reuse the Engine's existing runtime; structural commands may leave it unbound.
    pub(crate) fn bind_engine_handle(&self, handle: tokio::runtime::Handle) {
        let _ = self.shared.engine_handle.set(handle);
    }

    /// The plugin host's state: what `/plugin` and the error a plugin's tool
    /// call gets while it is down report.
    #[must_use]
    pub fn status(&self) -> HostStatus {
        self.tier_status(HostTier::Plugin)
    }

    /// The state of `tier`'s host.
    #[must_use]
    fn tier_status(&self, tier: HostTier) -> HostStatus {
        self.shared
            .tier_runtime(tier)
            .host
            .lock()
            .expect("host lock")
            .status()
    }

    /// The pinned runtime's one-line summary, once a host on it has completed
    /// the handshake.
    #[must_use]
    pub fn runtime_summary(&self) -> Option<String> {
        self.shared
            .runtime
            .lock()
            .expect("runtime lock")
            .pinned
            .as_ref()
            .map(|(_, summary)| summary.clone())
    }

    /// How many times this manager has tried to start a host process, both
    /// tiers together.
    #[must_use]
    pub fn spawn_attempts(&self) -> u64 {
        HostTier::ALL
            .into_iter()
            .map(|tier| self.tier_spawn_attempts(tier))
            .sum()
    }

    /// How many times this manager has tried to start `tier`'s host.
    #[must_use]
    fn tier_spawn_attempts(&self, tier: HostTier) -> u64 {
        self.shared
            .tier_runtime(tier)
            .spawn_attempts
            .load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> {
        self.shared
            .diagnostics
            .lock()
            .expect("diagnostics lock")
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }

    /// Recent retained diagnostics for exactly this plugin, not a persistent
    /// log. The command renderer escapes each field before displaying it.
    #[must_use]
    pub fn owner_report(&self, plugin_id: &str) -> Option<OwnerReport> {
        let registry = self.shared.registry.lock().expect("registry lock");
        let state = registry.owner(plugin_id).map(|entry| match &entry.state {
            OwnerState::Failed(reason) => OwnerState::Failed(bounded_diagnostic(reason.clone())),
            OwnerState::Faulted(reason) => OwnerState::Faulted(bounded_diagnostic(reason.clone())),
            state => state.clone(),
        });
        let tools = registry
            .live_tools()
            .into_iter()
            .filter(|tool| tool.owner.plugin_id == plugin_id)
            .map(|tool| tool.name)
            .collect();
        drop(registry);
        let mut diagnostics: Vec<_> = self
            .shared
            .diagnostics
            .lock()
            .expect("diagnostics lock")
            .iter()
            .rev()
            .filter(|entry| entry.plugin_id.as_deref() == Some(plugin_id))
            .take(20)
            .map(|entry| entry.message.clone())
            .collect();
        diagnostics.reverse();
        if state.is_none() && diagnostics.is_empty() {
            return None;
        }
        Some(OwnerReport {
            state,
            tools,
            diagnostics,
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn live_tool_names(&self) -> Vec<String> {
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .live_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[cfg(test)]
    #[must_use]
    pub fn live_command_names(&self) -> Vec<String> {
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .live_commands()
            .into_iter()
            .map(|command| command.name)
            .collect()
    }

    #[cfg(test)]
    #[must_use]
    pub fn owner_state(&self, plugin_id: &str) -> Option<OwnerState> {
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .owner(plugin_id)
            .map(|entry| entry.state.clone())
    }

    /// Install the user's `[plugins]` settings (boot), and the config file a
    /// later `/plugin reload` re-reads them from. Takes effect at the next
    /// reconcile: a plugin whose settings changed is re-activated.
    pub fn set_plugin_settings(
        &self,
        settings: &BTreeMap<String, codewhale_config::extension_host::PluginSettings>,
        source: Option<PathBuf>,
    ) {
        self.shared
            .plugin_configs
            .lock()
            .expect("plugin configs lock")
            .replace(settings, source);
    }

    /// Read `[plugins]` from the config file again, if boot named one. A file
    /// that cannot be read or parsed keeps the previous settings.
    async fn reload_plugin_settings(&self) {
        let Some(path) = self
            .shared
            .plugin_configs
            .lock()
            .expect("plugin configs lock")
            .source()
        else {
            return;
        };
        let read_path = path.clone();
        match tokio::task::spawn_blocking(move || {
            codewhale_runtime::config::read_plugin_settings(&read_path)
        })
        .await
        {
            Ok(Ok(settings)) => self
                .shared
                .plugin_configs
                .lock()
                .expect("plugin configs lock")
                .replace_reloaded(&settings),
            Ok(Err(reason)) => self.shared.diagnostic(format!(
                "plugin settings were not reloaded ({reason}); keeping the previous ones"
            )),
            Err(error) => self.shared.diagnostic(format!(
                "plugin settings were not reloaded from {}: {error}",
                path.display()
            )),
        }
    }

    /// What the user configured for `plugin_name`: the keys (never the values)
    /// or why the config is refused. `None` when nothing is configured.
    #[must_use]
    pub fn plugin_config_summary(&self, plugin_name: &str) -> Option<Result<Vec<String>, String>> {
        self.shared
            .plugin_configs
            .lock()
            .expect("plugin configs lock")
            .summary(plugin_name)
    }

    /// An explicit plugin mutation retries failed receipts and clears the
    /// shared crash budget. Merely opening another engine never does this.
    pub fn retry(&self) {
        // One tier at a time, so no two tiers' locks are ever held together.
        for tier in HostTier::ALL {
            let runtime = self.shared.tier_runtime(tier);
            let mut slot = runtime.host.lock().expect("host lock");
            let mut supervision = runtime.supervision.lock().expect("supervision lock");
            supervision.crashes.clear();
            supervision.launch_failed = false;
            supervision.retry_ticket += 1;
            if matches!(
                &*slot,
                HostSlot::Failed { .. } | HostSlot::Restarting { .. }
            ) {
                *slot = HostSlot::Idle;
            }
        }
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .forget_inactive();
    }

    fn retry_launch_on_attach(&self) {
        for tier in HostTier::ALL {
            let runtime = self.shared.tier_runtime(tier);
            let mut slot = runtime.host.lock().expect("host lock");
            let mut supervision = runtime.supervision.lock().expect("supervision lock");
            if matches!(&*slot, HostSlot::Failed { .. })
                && supervision.launch_failed
                && supervision.last_start.is_some_and(|at| {
                    at.elapsed() >= self.shared.options.supervision.start_retry_cooldown
                })
            {
                *slot = HostSlot::Idle;
                supervision.retry_ticket += 1;
            }
        }
    }

    /// Record native tool names from one engine's turn build (the registry
    /// before scripts, plugins and extensions are added) so
    /// `registry/register` refuses collisions. Additive: engines in one
    /// process report different native surfaces and none may shrink the set.
    pub fn note_native_names<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .add_native_names(names);
    }

    /// Attach an engine whose workspace plugin snapshot is `plugins`. Nothing
    /// is reconciled until [`HostAttachment::sync`] or a background sync.
    #[must_use]
    pub fn attach(self: &Arc<Self>, plugins: Arc<PluginRegistry>) -> HostAttachment {
        self.retry_launch_on_attach();
        let id = self.shared.next_attachment.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared
            .attachments
            .lock()
            .expect("attachments lock")
            .insert(
                id,
                AttachmentState {
                    plugins: Arc::new(plugins.bind_caller(composition_scope::SelectionRevision {
                        attachment_id: id,
                        revision: 1,
                    })),
                    desired: BTreeMap::new(),
                    selection: composition_scope::CompositionSelection::default(),
                    revision: 1,
                    selection_cancel: CancellationToken::new(),
                    settled: None,
                    session_id: None,
                    agent_id: None,
                },
            );
        command::bump_epoch();
        HostAttachment {
            id,
            manager: Arc::clone(self),
        }
    }

    /// How many engines are attached.
    #[must_use]
    pub fn attached_engines(&self) -> usize {
        self.shared
            .attachments
            .lock()
            .expect("attachments lock")
            .len()
    }

    /// Replace the snapshot of every engine attached to `plugins`'s
    /// workspace: a plugin was enabled, disabled, trusted or revoked there.
    fn refresh_workspace(&self, plugins: &Arc<PluginRegistry>) {
        let ids: Vec<_> = self
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .iter()
            .filter(|(_, state)| state.plugins.workspace() == plugins.workspace())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.shared.core_calls.revoke_attachment(id);
        }
        for state in self
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .values_mut()
        {
            if state.plugins.workspace() == plugins.workspace() {
                state.selection_cancel.cancel();
                state.selection_cancel = CancellationToken::new();
                state.revision += 1;
                let view = plugins.retain_native_selection_from(state.plugins.as_ref());
                let id = state
                    .plugins
                    .caller_selection()
                    .expect("attached caller")
                    .attachment_id;
                state.plugins = Arc::new(view.bind_caller(composition_scope::SelectionRevision {
                    attachment_id: id,
                    revision: state.revision,
                }));
                state.desired.clear();
                state.selection = Default::default();
            }
        }
        command::bump_epoch();
    }

    /// Add the live tools of the owners attachment `id` desires to
    /// `tool_registry`, *after* natives and `~/.codewhale/tools` scripts. A
    /// name already present is skipped with a diagnostic —
    /// `ToolRegistry::register` would silently overwrite it. Returns the
    /// names added.
    fn install_tools_for(
        &self,
        id: u64,
        tool_registry: &mut crate::tools::ToolRegistry,
    ) -> Vec<String> {
        let desired = self
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get(&id)
            .map(|state| state.selection.clone())
            .unwrap_or_default();
        if desired.desired.is_empty() {
            return Vec::new();
        }
        let tools: Vec<ToolRegistration> = self
            .shared
            .registry
            .lock()
            .expect("registry lock")
            .live_tools()
            .into_iter()
            .filter(|tool| {
                desired.includes(
                    &tool.owner.plugin_id,
                    &tool.content_hash,
                    tool.scope.as_ref(),
                )
            })
            .collect();
        if tools.is_empty() {
            return Vec::new();
        }
        let taken: HashSet<String> = tool_registry
            .names()
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect();
        let mut installed = Vec::new();
        let mut counts = BTreeMap::new();
        for tool in &tools {
            *counts
                .entry(tool.name.to_ascii_lowercase())
                .or_insert(0usize) += 1;
        }
        for registration in tools {
            if counts[&registration.name.to_ascii_lowercase()] != 1 {
                self.shared.plugin_diagnostic(&registration.owner.plugin_id,format!("Native tool `{}` is ambiguous in this caller; rename the selected definitions",registration.name));
                continue;
            }
            if taken.contains(&registration.name.to_ascii_lowercase()) {
                let origin = tool_registry
                    .get(&registration.name)
                    .map(|existing| existing.registration_origin().into_owned())
                    .unwrap_or_else(|| "another tool".to_string());
                self.shared.plugin_diagnostic(&registration.owner.plugin_id, format!(
                    "extension tool `{}` from `{}` skipped: the name is already registered by {origin}",
                    registration.name, registration.plugin_name
                ));
                continue;
            }
            installed.push(registration.name.clone());
            tool_registry.register(Arc::new(tool::HostToolSpec::for_selection(
                registration,
                Arc::clone(&self.shared),
                desired.revision,
            )));
        }
        installed
    }

    /// The live commands of the owners that engines attached for `workspace`
    /// desire, each bound to the reviewed bytes that engine desires: what the
    /// user registry loads for that workspace. A workspace never sees another
    /// workspace's project plugins' commands.
    pub(crate) fn commands_for_workspace(
        &self,
        workspace: &Path,
    ) -> Vec<command::ExtensionCommandEntry> {
        let mut desired: BTreeSet<(String, String)> = BTreeSet::new();
        for state in self
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .values()
            .filter(|state| state.plugins.workspace() == workspace)
        {
            desired.extend(
                state
                    .desired
                    .iter()
                    .map(|(plugin_id, hash)| (plugin_id.clone(), hash.clone())),
            );
        }
        if desired.is_empty() {
            return Vec::new();
        }
        let registry = self.shared.registry.lock().expect("registry lock");
        registry
            .live_commands()
            .into_iter()
            .filter(|command| {
                command.scope.is_none()
                    && desired.contains(&(
                        command.owner.plugin_id.clone(),
                        command.content_hash.clone(),
                    ))
            })
            .filter_map(|registration| {
                let authority = registry.authority_for(&registration.owner)?;
                Some(command::ExtensionCommandEntry {
                    registration,
                    authority,
                    workspace: workspace.to_path_buf(),
                    selection: None,
                })
            })
            .collect()
    }

    pub(crate) fn commands_for_plugins(
        &self,
        plugins: &PluginRegistry,
    ) -> Vec<command::ExtensionCommandEntry> {
        let Some(revision) = plugins.caller_selection() else {
            return self.commands_for_workspace(plugins.workspace());
        };
        let selected = self.shared.selection(revision.attachment_id);
        if selected.revision != Some(revision) {
            return Vec::new();
        }
        let registry = self.shared.registry.lock().expect("registry lock");
        let commands: Vec<_> = registry
            .live_commands()
            .into_iter()
            .filter(|command| {
                selected.includes(
                    &command.owner.plugin_id,
                    &command.content_hash,
                    command.scope.as_ref(),
                )
            })
            .collect();
        let mut counts = BTreeMap::new();
        for command in &commands {
            *counts.entry(command.name.clone()).or_insert(0usize) += 1;
        }
        commands
            .into_iter()
            .filter(|command| counts[&command.name] == 1)
            .filter_map(|registration| {
                Some(command::ExtensionCommandEntry {
                    authority: registry.authority_for(&registration.owner)?,
                    registration,
                    workspace: plugins.workspace().to_path_buf(),
                    selection: Some(revision),
                })
            })
            .collect()
    }

    /// Reconcile without waiting (turn builds, session start,
    /// plugin changes).
    pub fn reconcile_in_background(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let manager = Arc::clone(self);
        let policy = activation::extension_host_policy_enabled();
        tokio::spawn(async move {
            if let Err(error) = manager.reconcile_with_policy(policy).await {
                manager.shared.diagnostic(error);
            }
        });
    }

    /// Like [`Self::reconcile_in_background`], after re-reading the plugin
    /// settings from the config file: for an explicit plugin reload.
    fn reload_and_reconcile_in_background(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let manager = Arc::clone(self);
        let policy = activation::extension_host_policy_enabled();
        tokio::spawn(async move {
            manager.reload_plugin_settings().await;
            if let Err(error) = manager.reconcile_with_policy(policy).await {
                manager.shared.diagnostic(error);
            }
        });
    }

    /// Reconcile host owners with the reviewed, enabled plugins that declare
    /// `native` entries in any attached engine's snapshot: revoke what no
    /// attachment desires any more or what changed (synchronously, then ask
    /// the host to tear down), spawn the host if needed, activate the rest.
    pub async fn reconcile(&self) -> Result<(), String> {
        self.reconcile_with_policy(activation::extension_host_policy_enabled())
            .await
    }

    async fn reconcile_with_policy(&self, policy: bool) -> Result<(), String> {
        let shared = &self.shared;
        let _serial = shared.sync_lock.lock().await;
        let (desired, errors) = loop {
            let snapshots: Vec<(u64, Arc<PluginRegistry>)> = shared
                .attachments
                .lock()
                .expect("attachments lock")
                .iter()
                .map(|(id, state)| (*id, Arc::clone(&state.plugins)))
                .collect();
            #[cfg(test)]
            let env_scope = crate::test_support::env_scope_ticket();
            let scan = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _scope = activation::PolicyScope::propagate(policy);
                union_of_desired_owners(snapshots)
            })
            .await
            .map_err(|error| format!("plugin scan failed: {error}"))?;
            let published = scan.publish(&mut shared.attachments.lock().expect("attachments lock"));
            if let Some(published) = published {
                break published;
            }
            // Attach, detach, or workspace refresh raced the blocking scan.
            // Rescan before changing either engine views or global owners.
        };
        for error in errors {
            shared.diagnostic(error);
        }

        // The settings each desired plugin would be activated with now. Read
        // before the registry lock: the settings lock is never held with it.
        let wanted_config: BTreeMap<String, String> = {
            let configs = shared.plugin_configs.lock().expect("plugin configs lock");
            desired
                .iter()
                .map(|(plugin_id, want)| {
                    (
                        plugin_id.clone(),
                        plugin_config::activation_hash(&configs.select(&want.plugin_name)),
                    )
                })
                .collect()
        };

        // 1. Revoke first — never waits for the host. Plugin-tier owners only:
        // the builtin tier's modules are not plugins, are never desired by an
        // attachment, and the table that wants them is fixed for the
        // manager's life, so no plugin change revokes one.
        let mut revoked: Vec<OwnerRef> = Vec::new();
        let mut to_activate: Vec<(String, DesiredOwner)> = Vec::new();
        {
            let mut registry = shared.registry.lock().expect("registry lock");
            let existing: Vec<(String, PluginAuthority, String)> = registry
                .owners()
                .filter(|entry| entry.tier == HostTier::Plugin)
                .filter_map(|entry| {
                    Some((
                        entry.owner.plugin_id.clone(),
                        entry.authority.clone()?,
                        entry.config_hash.clone(),
                    ))
                })
                .collect();
            for (plugin_id, authority, config_hash) in &existing {
                // An explicit trust/enable transition revokes the persisted
                // authority even when the bytes stay identical. Refresh that
                // owner instead of retaining a tool that can only fail closed.
                // A changed config is a changed activation: a new generation.
                let keep = desired.get(plugin_id).is_some_and(|want| {
                    want.authority.content_hash == authority.content_hash
                        && want.authority.capability_hash == authority.capability_hash
                        && want.authority.state_generation == authority.state_generation
                        && want.authority.state_path == authority.state_path
                        && wanted_config.get(plugin_id) == Some(config_hash)
                });
                if !keep {
                    if let Some(owner) = registry.revoke_owner(plugin_id) {
                        revoked.push(owner);
                    }
                    registry.forget_owner(plugin_id);
                }
            }
            for (plugin_id, want) in desired {
                // A failed or faulted activation of these exact bytes is not
                // retried every turn; changed authority or an explicit
                // plugin mutation/reload permits another attempt.
                if registry
                    .owner(&plugin_id)
                    .is_none_or(|owner| owner.state == OwnerState::Active)
                {
                    to_activate.push((plugin_id, want));
                }
            }
        }
        let host = shared.ready_host(HostTier::Plugin).ok();
        for owner in revoked {
            shared.core_calls.revoke_owner(&owner.plugin_id);
            shared.execution_broker.revoke_owner(&owner.plugin_id);
            shared.mcp_users.fetch_sub(
                shared.mcp_broker.revoke_owner(&owner.plugin_id),
                Ordering::SeqCst,
            );
            shared.plugin_diagnostic(
                &owner.plugin_id,
                format!("extension `{}` revoked", owner.plugin_id),
            );
            if let Some(host) = &host {
                host.revoke_calls_of(&owner.plugin_id);
                shared.deactivate_owner(host, &owner).await;
            }
        }

        // The plugin tier first, then the builtin tier; each only when it has
        // something to activate, so with no plugin and no module no host
        // starts. One tier's failure never stops the other's.
        let plugins = async {
            if to_activate.is_empty() {
                return Ok::<(), String>(());
            }
            let host = self.ensure_host(HostTier::Plugin, policy).await?;
            for (plugin_id, want) in to_activate {
                if host.has_exited() {
                    break;
                }
                let content_hash = want.authority.content_hash.clone();
                self.activate_owner(
                    &host,
                    Activation {
                        tier: HostTier::Plugin,
                        owner_id: plugin_id,
                        name: want.plugin_name,
                        authority: Some(want.authority),
                        content_hash,
                        entries: want.entries,
                    },
                )
                .await;
            }
            Ok(())
        }
        .await;
        // MCP selection admits tier-0 demand independently of Native plugins.
        // Harness demand still requires the captured Native policy below.
        let builtin = if policy
            || shared.mcp_users.load(Ordering::SeqCst) > 0
            || shared.stock_users.load(Ordering::SeqCst) > 0
        {
            self.reconcile_builtin(policy).await
        } else {
            Ok(())
        };
        for state in shared
            .attachments
            .lock()
            .expect("attachments lock")
            .values_mut()
        {
            if state.selection.revision == state.plugins.caller_selection() {
                state.settled = state.selection.revision;
            }
        }
        match (plugins, builtin) {
            (Err(error), Err(other)) => {
                shared.diagnostic(other);
                Err(error)
            }
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    /// One real host consumer asks for the pinned MCP module. Rust mode never
    /// starts tier 0. This uses the same manager, supervision and owner registry.
    async fn ensure_mcp_builtin(
        &self,
    ) -> Result<(Arc<HostProcess>, codewhale_config::OwnerRef, u64), String> {
        self.ensure_demand_builtin("host:mcp", activation::extension_host_policy_enabled())
            .await
    }
    /// Pure Core adapters demand the same pinned harness without enabling Native plugins.
    async fn ensure_stock_builtin(
        &self,
    ) -> Result<(Arc<HostProcess>, codewhale_config::OwnerRef, u64), String> {
        self.ensure_demand_builtin("host:harness", activation::extension_host_policy_enabled())
            .await
    }
    async fn ensure_harness_builtin(
        &self,
    ) -> Result<(Arc<HostProcess>, codewhale_config::OwnerRef, u64), String> {
        let native_policy = activation::extension_host_policy_enabled();
        if !native_policy {
            return Err(
                "Builtin backend requires features.extension_host; no Rust fallback is allowed"
                    .to_string(),
            );
        }
        self.ensure_demand_builtin("host:harness", native_policy)
            .await
    }
    async fn ensure_demand_builtin(
        &self,
        owner_id: &str,
        native_policy: bool,
    ) -> Result<(Arc<HostProcess>, codewhale_config::OwnerRef, u64), String> {
        let _serial = self.shared.sync_lock.lock().await;
        let root = host_root(&self.shared.options)?;
        materialize_bundle(&root)?;
        // A crashed tier-0 process cannot carry its old owner into a new
        // process. Retire only this builtin before minting a fresh generation.
        let stale = self
            .shared
            .registry
            .lock()
            .expect("registry lock")
            .owner(owner_id)
            .is_some_and(|entry| entry.state != OwnerState::Active);
        if stale {
            self.shared.core_calls.revoke_owner(owner_id);
            self.shared.execution_broker.revoke_owner(owner_id);
            self.shared.mcp_users.fetch_sub(
                self.shared.mcp_broker.revoke_owner(owner_id),
                Ordering::SeqCst,
            );
            self.shared
                .registry
                .lock()
                .expect("registry lock")
                .forget_owner(owner_id);
        }
        // Replay the actual Native policy after a Builtin crash. MCP demand
        // must never switch optional third-party activation on.
        self.reconcile_builtin(native_policy).await?;
        let host = self
            .shared
            .ready_host(HostTier::Builtin)
            .map_err(|status| host_down(HostTier::Builtin, &status))?;
        let owner = self
            .shared
            .registry
            .lock()
            .expect("registry lock")
            .owner(owner_id)
            .filter(|entry| entry.state == OwnerState::Active)
            .map(|entry| entry.owner.clone())
            .ok_or_else(|| "demanded builtin failed to activate".to_string())?;
        let generation = self.shared.builtin.host_generation.load(Ordering::SeqCst);
        Ok((host, owner, generation))
    }

    async fn reconcile_builtin(&self, policy: bool) -> Result<(), String> {
        let shared = &self.shared;
        let wanted: Vec<&'static BuiltinModule> = {
            let registry = shared.registry.lock().expect("registry lock");
            shared
                .builtin_modules
                .iter()
                .filter(|module| {
                    (module.id != "mcp" || shared.mcp_users.load(Ordering::SeqCst) > 0)
                        && (module.id != "harness"
                            || (policy && shared.harness_users.load(Ordering::SeqCst) > 0)
                            || shared.stock_users.load(Ordering::SeqCst) > 0)
                        && registry.owner(&module.owner_id()).is_none()
                })
                .collect()
        };
        if wanted.is_empty() {
            return Ok(());
        }
        let root = host_root(&shared.options)?;
        let mut activations = Vec::new();
        for module in wanted {
            // The module's source, accepted only if its SHA-256 is the one the
            // table pins: the path and digest to activate it with.
            let source = {
                let root = root.clone();
                tokio::task::spawn_blocking(move || {
                    let path = builtin_source_path(&root, module);
                    let bytes = std::fs::read(&path).map_err(|error| {
                        format!(
                            "cannot read the source of built-in module `{}` at {}: {error}",
                            module.id,
                            path.display()
                        )
                    })?;
                    let digest = hex(Sha256::digest(&bytes));
                    if digest != module.source_sha256 {
                        return Err(format!(
                            "the source of built-in module `{}` at {} has sha256 {digest}, not the {} the core pins",
                            module.id,
                            path.display(),
                            module.source_sha256
                        ));
                    }
                    Ok((path, digest))
                })
                .await
                .map_err(|error| format!("built-in module scan failed: {error}"))?
            };
            let owner_id = module.owner_id();
            match source {
                Ok(entry) => activations.push(Activation {
                    tier: HostTier::Builtin,
                    owner_id,
                    name: module.id.to_string(),
                    authority: None,
                    content_hash: module.source_sha256.to_string(),
                    entries: vec![entry],
                }),
                Err(reason) => {
                    let mut registry = shared.registry.lock().expect("registry lock");
                    if let Ok(owner) = registry.begin_owner(
                        HostTier::Builtin,
                        &owner_id,
                        module.id,
                        None,
                        module.source_sha256,
                    ) {
                        registry.mark_failed(&owner, OwnerState::Failed(reason.clone()));
                    }
                    drop(registry);
                    shared.plugin_diagnostic(
                        &owner_id,
                        format!(
                            "built-in module `{}` failed to activate: {reason}",
                            module.id
                        ),
                    );
                }
            }
        }
        if activations.is_empty() {
            return Ok(());
        }
        let host = self.ensure_host(HostTier::Builtin, policy).await?;
        for activation in activations {
            if host.has_exited() {
                break;
            }
            self.activate_owner(&host, activation).await;
        }
        Ok(())
    }

    async fn activate_owner(&self, host: &Arc<HostProcess>, want: Activation) {
        let shared = &self.shared;
        let plugin_id = want.owner_id.as_str();
        let begun = {
            let mut registry = shared.registry.lock().expect("registry lock");
            if let Some(owner) = registry
                .owner(plugin_id)
                .filter(|entry| entry.state == OwnerState::Active)
            {
                Ok(owner.owner.clone())
            } else {
                registry.begin_owner(
                    want.tier,
                    plugin_id,
                    &want.name,
                    want.authority.clone(),
                    &want.content_hash,
                )
            }
        };
        let owner = match begun {
            Ok(owner) => owner,
            Err(reason) => {
                shared.plugin_diagnostic(
                    plugin_id,
                    format!("extension `{}` was not activated: {reason}", want.name),
                );
                return;
            }
        };
        // What the owner is given: its settings and its own directory. A
        // config that is refused, or a directory that cannot be made, fails the
        // activation with the reason; the plugin is never started with
        // different settings than the user wrote. A built-in module takes no
        // user settings: `[plugins]` is keyed by plugin name and is not read
        // for tier 0.
        let selection = match want.tier {
            HostTier::Plugin => shared
                .plugin_configs
                .lock()
                .expect("plugin configs lock")
                .select(&want.name),
            HostTier::Builtin => Ok(plugin_config::PluginConfig::empty()),
        };
        shared
            .registry
            .lock()
            .expect("registry lock")
            .set_config_hash(&owner, &plugin_config::activation_hash(&selection));
        let context = match selection {
            Ok(config) => self
                .owner_data_dir(want.tier, plugin_id, &want.name)
                .await
                .map(|data_dir| (config, data_dir)),
            Err(reason) => Err(reason),
        };
        let (config, data_dir) = match context {
            Ok(context) => context,
            Err(reason) => {
                shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .mark_failed(&owner, OwnerState::Failed(reason.clone()));
                shared.plugin_diagnostic(
                    plugin_id,
                    format!("extension `{}` failed to activate: {reason}", want.name),
                );
                return;
            }
        };
        // Retire no-longer-wanted entry handles before awaiting host disposal.
        if want.tier == HostTier::Plugin {
            let scopes: Vec<_> = {
                let registry = shared.registry.lock().expect("registry lock");
                registry
                    .owner(plugin_id)
                    .map(|entry| {
                        entry
                            .scopes
                            .keys()
                            .filter(|scope| {
                                !want.entries.iter().any(|(path, hash)| {
                                    scope.path == path.to_string_lossy() && scope.sha256 == *hash
                                })
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for scope in scopes {
                let handles = shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .revoke_scope(&owner, &scope);
                shared.core_calls.revoke_scope(plugin_id, &scope);
                shared.execution_broker.revoke_scope(plugin_id, &scope);
                host.revoke_calls_for_handles(plugin_id, &handles);
                shared.deactivate_entry(host, &owner, Some(scope)).await;
            }
        }
        if want.tier == HostTier::Plugin {
            let admission = match want.authority.clone() {
                Some(authority) => host.admit_native_root(authority).await,
                None => Err("Native owner has no reviewed bundle authority".into()),
            };
            if let Err(reason) = admission {
                shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .mark_failed(&owner, OwnerState::Failed(reason.clone()));
                shared.plugin_diagnostic(
                    plugin_id,
                    format!(
                        "extension `{}` failed Native admission: {reason}",
                        want.name
                    ),
                );
                shared.deactivate_owner(host, &owner).await;
                return;
            }
        }
        let mut failure = None;
        let mut native_failure = None;
        // A plugin may declare several `native` entries. Each is activated in
        // declaration order under one owner. Each entry has an independent
        // scope: a failing entry withdraws its own handles and cleanup, while
        // already-active sibling entries remain usable.
        let mut tools = BTreeSet::new();
        let mut commands = BTreeSet::new();
        for (path, sha256) in &want.entries {
            let scope = (want.tier == HostTier::Plugin).then(|| EntryRef {
                path: path.to_string_lossy().into_owned(),
                sha256: sha256.clone(),
            });
            if let Some(scope) = &scope {
                let admitted = shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .begin_scope(&owner, scope.clone());
                if admitted.is_err() {
                    continue;
                }
            }
            let request = CoreRequest::Activate(ActivateParams {
                scope: scope.clone(),
                owner: owner.clone(),
                plugin_name: want.name.clone(),
                entry: EntryRef {
                    path: path.to_string_lossy().into_owned(),
                    sha256: sha256.clone(),
                },
                config: config.value.clone(),
                data_dir: Some(data_dir.clone()),
            });
            let outcome = host
                .call(request, Some(plugin_id.to_string()))
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ActivateResult>(value)
                        .map_err(|error| format!("malformed activation answer: {error}"))
                })
                .and_then(|result| match result {
                    ActivateResult::Ok { tools, commands } => Ok((tools, commands)),
                    ActivateResult::Failed { diagnostic } => Err(diagnostic),
                });
            match outcome {
                Ok((names, slash)) => {
                    tools.extend(names);
                    commands.extend(slash);
                    if let Some(scope) = &scope {
                        shared
                            .registry
                            .lock()
                            .expect("registry lock")
                            .mark_scope_active(&owner, scope);
                    }
                }
                Err(reason) => {
                    if let Some(scope) = scope {
                        shared
                            .registry
                            .lock()
                            .expect("registry lock")
                            .fail_scope(&owner, &scope);
                        shared.core_calls.revoke_scope(plugin_id, &scope);
                        shared.execution_broker.revoke_scope(plugin_id, &scope);
                        native_failure.get_or_insert_with(|| reason.clone());
                        shared.plugin_diagnostic(plugin_id, format!("Native entry failed to activate: {reason}; other selected entries remain live"));
                        shared.deactivate_entry(host, &owner, Some(scope)).await;
                    } else {
                        failure = Some(reason);
                        break;
                    }
                }
            }
        }
        if want.tier == HostTier::Plugin
            && shared
                .registry
                .lock()
                .expect("registry lock")
                .owner(plugin_id)
                .is_none_or(|entry| {
                    !entry
                        .scopes
                        .values()
                        .any(|state| *state == OwnerState::Active)
                })
        {
            let reason =
                native_failure.unwrap_or_else(|| "No selected Native entry became active".into());
            shared
                .registry
                .lock()
                .expect("registry lock")
                .mark_failed(&owner, OwnerState::Failed(reason.clone()));
            shared.plugin_diagnostic(
                plugin_id,
                format!("extension `{}` failed to activate: {reason}", want.name),
            );
            // Per-entry acknowledgements above account for incomplete cleanup.
            // Also retire the now-empty parent so its owner record cannot linger.
            shared.deactivate_owner(host, &owner).await;
            return;
        }
        match failure {
            None => {
                let active = shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .mark_active(&owner);
                if active {
                    shared.plugin_diagnostic(
                        plugin_id,
                        format!(
                            "extension `{}` active (tools: {}; commands: {})",
                            want.name,
                            tools.iter().cloned().collect::<Vec<_>>().join(", "),
                            commands
                                .iter()
                                .map(|name| format!("/{name}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                }
            }
            Some(reason) => {
                // All-or-nothing: drop anything half-registered on this side,
                // and dispose any entry the host did activate.
                shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .mark_failed(&owner, OwnerState::Failed(reason.clone()));
                shared.plugin_diagnostic(
                    plugin_id,
                    format!("extension `{}` failed to activate: {reason}", want.name),
                );
                shared.deactivate_owner(host, &owner).await;
            }
        }
    }

    /// Create (if needed) and name an owner's own directory inside its tier's
    /// data dir, the host sandbox's writable root: a plugin's at its
    /// long-standing path, a built-in module's under the builtin tier's.
    async fn owner_data_dir(
        &self,
        tier: HostTier,
        plugin_id: &str,
        plugin_name: &str,
    ) -> Result<String, String> {
        let path = supervisor::owner_data_dir(
            &host_root(&self.shared.options)?,
            tier,
            plugin_id,
            plugin_name,
        );
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&path).await.map_err(|error| {
            format!(
                "cannot create the plugin data directory {}: {error}",
                path.display()
            )
        })?;
        path.to_str().map(str::to_owned).ok_or_else(|| {
            format!(
                "the plugin data directory {} is not valid UTF-8",
                path.display()
            )
        })
    }

    /// `tier`'s host, started (lazily, once per generation) if it is idle.
    async fn ensure_host(&self, tier: HostTier, policy: bool) -> Result<Arc<HostProcess>, String> {
        let shared = &self.shared;
        let runtime = shared.tier_runtime(tier);
        let label = tier.host_label();
        let mut generation = {
            let mut slot = runtime.host.lock().expect("host lock");
            match &*slot {
                HostSlot::Ready(host) if !host.has_exited() && !host.is_retiring() => {
                    return Ok(Arc::clone(host));
                }
                HostSlot::Idle => {}
                other => return Err(host_down(tier, &other.status())),
            }
            *slot = HostSlot::Starting;
            let mut supervision = runtime.supervision.lock().expect("supervision lock");
            supervision.last_start = Some(Instant::now());
            supervision.policy = policy;
            supervision.launch_failed = false;
            supervision.dirty_teardowns.clear();
            supervision.dirty_restart_pending = false;
            supervision.planned_restart = None;
            runtime.host_generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        // At most two launches: under `auto`, before anything is pinned, a
        // Bun that cannot start is reported once and Node is resolved for the
        // rest of the session. An explicit `bun` or `node` never falls back.
        let spawned = loop {
            runtime.spawn_attempts.fetch_add(1, Ordering::SeqCst);
            let options = shared.options.clone();
            let snapshot_state_paths: Vec<_> = if tier == HostTier::Plugin {
                shared
                    .attachments
                    .lock()
                    .expect("attachments lock")
                    .values()
                    .filter_map(|state| state.plugins.state_path().map(Path::to_path_buf))
                    .collect()
            } else {
                Vec::new()
            };
            let (pinned, bun_failed) = {
                let runtime = shared.runtime.lock().expect("runtime lock");
                (runtime.pinned.clone(), runtime.bun_failed)
            };
            #[cfg(test)]
            let env_scope = crate::test_support::env_scope_ticket();
            let prepared = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                prepare_launch(&options, tier, pinned, bun_failed, &snapshot_state_paths)
            })
            .await
            .map_err(|error| format!("extension host preparation failed: {error}"))
            .and_then(|result| result);
            let (launch, summary) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => break Err(error),
            };
            let events: Arc<dyn HostEvents> = Arc::new(Events {
                shared: Arc::downgrade(shared),
                tier,
                generation,
            });
            let reason =
                match HostProcess::spawn(generation, &launch, bundle_sha256(), events).await {
                    Ok(host) => break Ok((host, summary)),
                    Err(reason) => reason,
                };
            if shared.options.runtime
                != codewhale_config::extension_host::ExtensionHostRuntime::Auto
                || launch.runtime.kind != codewhale_dependencies::HostRuntimeKind::Bun
                || summary.is_none()
            {
                break Err(reason);
            }
            shared.runtime.lock().expect("runtime lock").bun_failed = true;
            shared.diagnostic(format!(
                "{label}: Bun {} at {} failed to start ({reason}); runtime = \"auto\" uses Node for the rest of this session",
                launch.runtime.version_string(),
                launch.runtime.path.display()
            ));
            // A fresh generation, so the failed Bun host's exit report can
            // never be taken for the Node host's.
            generation = {
                let slot = runtime.host.lock().expect("host lock");
                if runtime.host_generation.load(Ordering::SeqCst) != generation
                    || !matches!(&*slot, HostSlot::Starting)
                {
                    break Err(format!("{label} startup was superseded"));
                }
                runtime.host_generation.fetch_add(1, Ordering::SeqCst) + 1
            };
        };
        let mut slot = runtime.host.lock().expect("host lock");
        if runtime.host_generation.load(Ordering::SeqCst) != generation
            || !matches!(&*slot, HostSlot::Starting)
        {
            drop(slot);
            if let Ok((host, _)) = spawned {
                host.terminate("host startup superseded".into());
            }
            return Err(format!("{label} startup was superseded"));
        }
        match spawned {
            Ok((host, summary)) => {
                *slot = HostSlot::Ready(Arc::clone(&host));
                drop(slot);
                // Pinned only once a host on this runtime has completed the
                // handshake; every restart then reuses it.
                if let Some(summary) = summary {
                    let mut runtime = shared.runtime.lock().expect("runtime lock");
                    if runtime.pinned.is_none() {
                        runtime.pinned = Some((host.runtime.clone(), summary.clone()));
                        drop(runtime);
                        shared.diagnostic(format!("extension host runtime: {summary}"));
                    }
                }
                if host.has_exited() {
                    Events {
                        shared: Arc::downgrade(shared),
                        tier,
                        generation,
                    }
                    .exited(
                        generation,
                        "exited immediately after handshake".into(),
                        host.stderr_tail(),
                    );
                    return Err(format!("{label} exited immediately after handshake"));
                }
                monitor_host(shared, &host, generation);
                shared.diagnostic(format!(
                    "{label} started (pid {}, {} {}, sandbox {})",
                    host.pid
                        .map_or_else(|| "?".to_string(), |pid| pid.to_string()),
                    host.runtime.kind.name(),
                    host.runtime_version.get().map_or("?", String::as_str),
                    host.sandbox.label()
                ));
                Ok(host)
            }
            Err(reason) => {
                runtime
                    .supervision
                    .lock()
                    .expect("supervision lock")
                    .launch_failed = true;
                *slot = HostSlot::Failed {
                    reason: format!("start failed: {reason}"),
                    stderr_tail: String::new(),
                };
                drop(slot);
                shared.diagnostic(format!("{label} failed to start: {reason}"));
                Err(reason)
            }
        }
    }

    /// Bounded shutdown of each tier's host process, if one is running.
    /// Production has no such call: the hosts are shared by every engine in
    /// the process, so no single engine's shutdown may stop them. When this
    /// process ends a host sees stdin EOF and kills its own process tree; if a
    /// plugin blocks its event loop, its watchdog thread does so when the
    /// parent changes.
    #[cfg(test)]
    pub async fn shutdown(&self) {
        for tier in HostTier::ALL {
            let runtime = self.shared.tier_runtime(tier);
            let host = {
                let mut slot = runtime.host.lock().expect("host lock");
                runtime.host_generation.fetch_add(1, Ordering::SeqCst);
                runtime
                    .supervision
                    .lock()
                    .expect("supervision lock")
                    .retry_ticket += 1;
                match std::mem::replace(&mut *slot, HostSlot::Idle) {
                    HostSlot::Ready(host) | HostSlot::Unresponsive(host) => Some(host),
                    _ => None,
                }
            };
            if let Some(host) = host {
                self.shared
                    .registry
                    .lock()
                    .expect("registry lock")
                    .revoke_all(tier, "extension host shut down");
                host.shutdown().await;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn host_requests_started(&self) -> Option<u64> {
        self.shared
            .ready_host(HostTier::Plugin)
            .ok()
            .map(|host| host.requests_started())
    }

    #[cfg(test)]
    pub(crate) fn host_pid(&self) -> Option<u32> {
        self.shared
            .ready_host(HostTier::Plugin)
            .ok()
            .and_then(|host| host.pid)
    }
}

/// Human-readable host section for `/plugin`.
pub(crate) fn render_status(manager: &ExtensionHostManager) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("Extension host (experimental): ");
    let status = manager.status();
    match status.clone() {
        HostStatus::Ready {
            pid,
            runtime,
            runtime_version,
            sandbox,
            memory: _,
        } => {
            let _ = write!(
                out,
                "running · pid {} · {runtime} {runtime_version} · {sandbox}",
                pid.map_or_else(|| "?".to_string(), |pid| pid.to_string()),
            );
        }
        HostStatus::Failed { stderr_tail, .. } => {
            let _ = write!(out, "{status}");
            let tail = stderr_tail.trim();
            if !tail.is_empty() {
                let start = tail
                    .char_indices()
                    .rev()
                    .nth(599)
                    .map_or(0, |(index, _)| index);
                let _ = write!(out, "\n  stderr: {}", &tail[start..]);
            }
        }
        down => {
            let _ = write!(out, "{down}");
        }
    }
    if let Some(summary) = manager.runtime_summary() {
        let _ = write!(out, "\n  runtime: {summary}");
        // How the running host's cap is enforced, as its handshake settled
        // it; with no host running there is nothing enforced to describe.
        if let HostStatus::Ready { memory, .. } = &status {
            let cap = manager.shared.options.supervision.memory_cap;
            let _ = write!(out, " · {}", memory.describe(cap));
        }
    }
    let _ = write!(
        out,
        "\n  spawn attempts: {} · engines attached: {}",
        manager.spawn_attempts(),
        manager.attached_engines()
    );
    // The built-in host is shown only once something has started it (nothing
    // does yet); the lists and the shared-process count below are the plugin
    // host's.
    match manager.tier_status(HostTier::Builtin) {
        HostStatus::Idle => {}
        HostStatus::Ready {
            pid,
            runtime,
            runtime_version,
            sandbox,
            memory: _,
        } => {
            let _ = write!(
                out,
                "\n  built-in host (tier 0): running · pid {} · {runtime} {runtime_version} · {sandbox}",
                pid.map_or_else(|| "?".to_string(), |pid| pid.to_string()),
            );
        }
        other => {
            let _ = write!(out, "\n  built-in host (tier 0): {other}");
        }
    }
    let (tools, commands, owners) = {
        let registry = manager.shared.registry.lock().expect("registry lock");
        let owners = registry
            .owners()
            .filter(|entry| entry.tier == HostTier::Plugin && entry.state == OwnerState::Active)
            .count();
        let tools: Vec<_> = registry
            .live_tools()
            .into_iter()
            .filter(|tool| tool.tier == HostTier::Plugin)
            .collect();
        let commands: Vec<_> = registry
            .live_commands()
            .into_iter()
            .filter(|command| command.tier == HostTier::Plugin)
            .collect();
        (tools, commands, owners)
    };
    if owners > 1 {
        let _ = write!(
            out,
            "\n  {owners} plugins share this one host process and can alter each other's behaviour"
        );
    }
    for tool in tools {
        let _ = write!(
            out,
            "\n  tool {} (extension:{}; needs approval, which your approval mode or a session grant for this exact call of this plugin build may give)",
            tool.name, tool.plugin_name
        );
    }
    for command in commands {
        let _ = write!(
            out,
            "\n  command /{} (extension:{}; runs only when you invoke it)",
            command.name, command.plugin_name
        );
    }
    let diagnostics = manager.diagnostics();
    for diagnostic in diagnostics.iter().rev().take(5).rev() {
        let _ = write!(out, "\n  · {diagnostic}");
    }
    out
}

/// A plugin was enabled, disabled, trusted, revoked or removed: reconcile the
/// host now, so a disabled plugin's calls are cancelled and its code torn
/// down without waiting for the next turn. No-op with the flag off.
pub fn plugins_changed(plugins: Arc<PluginRegistry>) {
    if activation::extension_host_policy_enabled() {
        let manager = manager();
        manager.refresh_workspace(&plugins);
        manager.retry();
        // An explicit plugin action, so also the moment to pick up an edit to
        // `[plugins."<name>".config]`.
        manager.reload_and_reconcile_in_background();
    }
}

/// Install the user's per-plugin settings at boot (the host's other boot
/// options are `configure`). `source` is the config file `/plugin reload`
/// re-reads them from. No-op with the flag off.
pub fn install_plugin_settings(
    settings: Option<&BTreeMap<String, codewhale_config::extension_host::PluginSettings>>,
    source: Option<PathBuf>,
) {
    if activation::extension_host_policy_enabled() {
        manager().set_plugin_settings(settings.unwrap_or(&BTreeMap::new()), source);
    }
}

/// What the user configured for `plugin_name` (keys only, or the reason it is
/// refused), for `/plugin show`. `None` with the flag off or nothing configured.
#[must_use]
pub fn plugin_config_summary(plugin_name: &str) -> Option<Result<Vec<String>, String>> {
    if !activation::extension_host_policy_enabled() {
        return None;
    }
    manager().plugin_config_summary(plugin_name)
}

/// The live extension commands of `workspace`'s reviewed plugins, for the user
/// command registry. Empty with the flag off. Never starts the host.
#[must_use]
pub(crate) fn live_commands_for(workspace: &Path) -> Vec<command::ExtensionCommandEntry> {
    if !activation::extension_host_policy_enabled() {
        return Vec::new();
    }
    manager().commands_for_workspace(workspace)
}

pub(crate) fn live_commands_for_plugins(
    plugins: &PluginRegistry,
) -> Vec<command::ExtensionCommandEntry> {
    if !activation::extension_host_policy_enabled() {
        return Vec::new();
    }
    manager().commands_for_plugins(plugins)
}

/// Run an extension command the user invoked; see [`command::run`]. Errors
/// are the text to show.
#[cfg(test)]
pub async fn run_command(
    command: &command::ExtensionCommandRef,
    raw_input: &str,
    session_id: Option<&str>,
) -> Result<command::CommandOutcome, String> {
    command::run(&manager().shared, command, raw_input, session_id).await
}

/// The command the current UI caller selected. A stale focus cannot consume a
/// command reference captured from another agent's palette.
pub async fn run_command_for_plugins(
    command: &command::ExtensionCommandRef,
    raw_input: &str,
    session_id: Option<&str>,
    plugins: &PluginRegistry,
) -> Result<command::CommandOutcome, String> {
    if command.scope.is_some() && command.selection != plugins.caller_selection() {
        return Err("Native command belongs to a different caller; select it again".into());
    }
    command::run(&manager().shared, command, raw_input, session_id).await
}

/// The `/plugin` section. With the experimental host off it is one line
/// saying so.
#[must_use]
pub fn status_report() -> String {
    if !activation::extension_host_policy_enabled() {
        return format!("Extension host (experimental): {}", HostStatus::Disabled);
    }
    render_status(&manager())
}

pub struct OwnerReport {
    pub state: Option<OwnerState>,
    pub tools: Vec<String>,
    pub diagnostics: Vec<String>,
}

/// The resolved plugin id is supplied by the existing `/plugin show` facet.
pub fn owner_report(plugin_id: &str) -> Option<OwnerReport> {
    if !activation::extension_host_policy_enabled() {
        return None;
    }
    manager().owner_report(plugin_id)
}

/// Where a built-in module's source is expected:
/// `<bundle dir>/builtin/<module>.mjs`, beside the bundle that was
/// materialized for this build.
fn builtin_source_path(root: &Path, module: &BuiltinModule) -> PathBuf {
    supervisor::bundle_dir(root, bundle_sha256())
        .join("builtin")
        .join(format!("{}.mjs", module.id))
}

/// Reviewed, enabled plugins with `native` entries, keyed by plugin id, read
/// from Codewhale's immutable staged snapshot. Blocking.
fn desired_owners(plugins: &PluginRegistry) -> (BTreeMap<String, DesiredOwner>, Vec<String>) {
    let (sources, mut errors) = crate::plugins::runtime::active_component_sources(
        plugins,
        PluginActivationCapability::Native,
    );
    let mut desired: BTreeMap<String, DesiredOwner> = BTreeMap::new();
    let mut broken: BTreeSet<String> = BTreeSet::new();
    let mut needs_selection: BTreeSet<String> = BTreeSet::new();
    for source in sources {
        let plugin_id = source.authority.plugin_id.as_str().to_string();
        if plugins.native_catalog_requires_selection(&plugin_id) {
            if needs_selection.insert(plugin_id.clone()) {
                errors.push(format!("Plugin `{}` raw agent-presets has no default; select a reviewed roster preset before activation", source.plugin_name));
            }
            continue;
        }
        // A plugin id can never be a tier-0 owner id. Discovery builds ids as
        // `<scope>/<hex>/<name>` and a manifest name cannot hold `:`, so this
        // cannot fire; it is the last check before an id reaches the host.
        if let Err(reason) = HostTier::Plugin.check_owner_id(&plugin_id) {
            errors.push(format!(
                "Plugin `{}` native entry {} was denied: {reason}",
                source.plugin_name,
                source.path.display()
            ));
            broken.insert(plugin_id);
            continue;
        }
        // The rule discovery reports, re-checked on the staged copy: the
        // name here, and file-ness by the read itself.
        let bytes = match crate::plugins::runtime::native_entry_problem(&source.path, true) {
            None => std::fs::read(&source.path).map_err(|error| error.to_string()),
            Some(problem) => Err(problem.to_string()),
        };
        match bytes {
            Ok(bytes) => {
                let digest = hex(Sha256::digest(&bytes));
                if !plugins.native_entry_selected(
                    &plugin_id,
                    &source.path.to_string_lossy(),
                    &digest,
                ) {
                    continue;
                }
                let owner = desired.entry(plugin_id).or_insert_with(|| DesiredOwner {
                    plugin_name: source.plugin_name.clone(),
                    authority: source.authority.clone(),
                    entries: Vec::new(),
                });
                // The manifest may name one file twice; it is one entry.
                if !owner.entries.iter().any(|(path, _)| *path == source.path) {
                    owner
                        .entries
                        .push((source.path.clone(), hex(Sha256::digest(&bytes))));
                }
            }
            Err(reason) => {
                errors.push(format!(
                    "Plugin `{}` native entry {} was denied: {reason}",
                    source.plugin_name,
                    source.path.display()
                ));
                broken.insert(plugin_id);
            }
        }
    }
    // All-or-nothing per plugin: one unusable entry keeps the whole plugin out.
    for plugin_id in broken {
        desired.remove(&plugin_id);
    }
    (desired, errors)
}

/// One scan of the complete attachment set. Its per-engine views and global
/// owner union must be published together, against those same snapshots.
struct DesiredScan {
    attachments: Vec<(u64, Arc<PluginRegistry>, BTreeMap<String, String>)>,
    owners: BTreeMap<String, DesiredOwner>,
    errors: Vec<String>,
}

impl DesiredScan {
    fn publish(
        self,
        current: &mut BTreeMap<u64, AttachmentState>,
    ) -> Option<(BTreeMap<String, DesiredOwner>, Vec<String>)> {
        if current.len() != self.attachments.len()
            || self.attachments.iter().any(|(id, scanned, _)| {
                !current
                    .get(id)
                    .is_some_and(|state| Arc::ptr_eq(&state.plugins, scanned))
            })
        {
            return None;
        }
        for (id, _, desired) in self.attachments {
            let state = current.get_mut(&id).expect("validated attachment");
            let entries = self
                .owners
                .iter()
                .flat_map(|(plugin_id, want)| {
                    want.entries.iter().filter_map(|(path, sha256)| {
                        let entry = EntryRef {
                            path: path.to_string_lossy().into_owned(),
                            sha256: sha256.clone(),
                        };
                        (desired.get(plugin_id) == Some(&want.authority.content_hash)
                            && state.plugins.native_entry_selected(
                                plugin_id,
                                &entry.path,
                                &entry.sha256,
                            ))
                        .then(|| composition_scope::NativePresetRef {
                            plugin_id: plugin_id.clone(),
                            content_hash: want.authority.content_hash.clone(),
                            entry,
                        })
                    })
                })
                .collect();
            state.selection = composition_scope::CompositionSelection {
                revision: state.plugins.caller_selection(),
                desired: desired.clone(),
                entries,
            };
            state.desired = desired;
        }
        command::bump_epoch();
        Some((self.owners, self.errors))
    }
}

/// Scan every attached snapshot (engines sharing one snapshot scan it once)
/// and merge what they desire. Blocking.
///
/// Two snapshots can disagree about one plugin id only while one of them is
/// stale; the stale one then fails its persisted-state check and desires
/// nothing, so the first valid scan wins and the per-attachment hashes keep
/// each engine's tools to the bytes it desires.
fn union_of_desired_owners(snapshots: Vec<(u64, Arc<PluginRegistry>)>) -> DesiredScan {
    let mut union: BTreeMap<String, DesiredOwner> = BTreeMap::new();
    let mut per_attachment = Vec::with_capacity(snapshots.len());
    let mut scanned: Vec<(Arc<PluginRegistry>, BTreeMap<String, String>)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (id, plugins) in snapshots {
        if let Some((_, hashes)) = scanned.iter().find(|(seen, _)| Arc::ptr_eq(seen, &plugins)) {
            per_attachment.push((id, Arc::clone(&plugins), hashes.clone()));
            continue;
        }
        let (desired, scan_errors) = desired_owners(&plugins);
        for error in scan_errors {
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
        let hashes: BTreeMap<String, String> = desired
            .iter()
            .map(|(plugin_id, want)| (plugin_id.clone(), want.authority.content_hash.clone()))
            .collect();
        for (plugin_id, want) in desired {
            match union.entry(plugin_id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(want);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    for native in want.entries {
                        if !entry.get().entries.contains(&native) {
                            entry.get_mut().entries.push(native);
                        }
                    }
                }
            }
        }
        per_attachment.push((id, Arc::clone(&plugins), hashes.clone()));
        scanned.push((plugins, hashes));
    }
    DesiredScan {
        attachments: per_attachment,
        owners: union,
        errors,
    }
}

/// One engine's hold on the process-wide extension host.
///
/// The engine publishes its workspace plugin snapshot here and installs only
/// the tools of owners that snapshot desires. Dropping it detaches without
/// revoking anything: the next reconcile revokes owners no remaining
/// attachment desires, so an engine being replaced never tears down plugins
/// its successor is about to use.
pub struct HostAttachment {
    id: u64,
    manager: Arc<ExtensionHostManager>,
}

impl HostAttachment {
    #[must_use]
    pub fn manager(&self) -> &Arc<ExtensionHostManager> {
        &self.manager
    }

    /// The engine switched workspace: publish the new snapshot.
    pub fn set_plugins(&self, plugins: Arc<PluginRegistry>) {
        self.manager.shared.core_calls.revoke_attachment(self.id);
        self.manager
            .shared
            .execution_broker
            .revoke_attachment(self.id);
        if let Some(state) = self
            .manager
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get_mut(&self.id)
        {
            state.selection_cancel.cancel();
            state.selection_cancel = CancellationToken::new();
            state.revision += 1;
            state.plugins = Arc::new(plugins.bind_caller(composition_scope::SelectionRevision {
                attachment_id: self.id,
                revision: state.revision,
            }));
            state.desired.clear();
            state.selection = Default::default();
        }
        command::bump_epoch();
    }

    pub fn set_identity(&self, session_id: Option<String>, agent_id: Option<String>) {
        if let Some(state) = self
            .manager
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get_mut(&self.id)
        {
            state.session_id = session_id;
            state.agent_id = agent_id;
        }
    }
    pub fn plugin_view(&self) -> Arc<PluginRegistry> {
        self.manager
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get(&self.id)
            .map(|state| Arc::clone(&state.plugins))
            .unwrap_or_else(|| Arc::new(PluginRegistry::new()))
    }
    pub async fn reconcile(&self) -> Result<(), String> {
        self.manager.reconcile().await
    }

    /// Reconcile the host against every attachment, waiting for it.
    #[cfg(test)]
    pub async fn sync(&self) -> Result<(), String> {
        self.manager.reconcile().await
    }

    /// Reconcile the host against every attachment, without waiting.
    pub fn sync_in_background(&self) {
        self.manager.reconcile_in_background();
    }

    pub fn selection_settled(&self) -> bool {
        self.manager
            .shared
            .attachments
            .lock()
            .expect("attachments lock")
            .get(&self.id)
            .is_some_and(|state| {
                state.settled.is_some() && state.settled == state.plugins.caller_selection()
            })
    }

    pub async fn sync_within(&self, limit: Duration) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let manager = Arc::clone(&self.manager);
        let policy = activation::extension_host_policy_enabled();
        let task = tokio::spawn(async move {
            if let Err(error) = manager.reconcile_with_policy(policy).await {
                manager.shared.diagnostic(error);
            }
        });
        let _ = tokio::time::timeout(limit, task).await;
    }

    /// Add this engine's live extension tools to `tool_registry`.
    pub fn install_tools(&self, tool_registry: &mut crate::tools::ToolRegistry) -> Vec<String> {
        self.manager.install_tools_for(self.id, tool_registry)
    }
}

impl ManagerShared {
    pub(crate) fn plugins_for_selection(
        &self,
        revision: composition_scope::SelectionRevision,
        session_id: Option<&str>,
    ) -> Option<(Arc<PluginRegistry>, Option<String>)> {
        self.attachments
            .lock()
            .expect("attachments lock")
            .get(&revision.attachment_id)
            .filter(|state| {
                state.revision == revision.revision && state.session_id.as_deref() == session_id
            })
            .map(|state| (Arc::clone(&state.plugins), state.agent_id.clone()))
    }
    pub(super) fn selection_cancellation(
        &self,
        revision: composition_scope::SelectionRevision,
    ) -> Option<CancellationToken> {
        self.attachments
            .lock()
            .expect("attachments lock")
            .get(&revision.attachment_id)
            .filter(|state| state.revision == revision.revision)
            .map(|state| state.selection_cancel.clone())
    }
    pub(crate) fn selection(&self, id: u64) -> composition_scope::CompositionSelection {
        self.attachments
            .lock()
            .expect("attachments lock")
            .get(&id)
            .map(|state| state.selection.clone())
            .unwrap_or_default()
    }
    pub(crate) fn selection_current(
        &self,
        selection: composition_scope::SelectionRevision,
        plugin_id: &str,
        hash: &str,
        scope: Option<&EntryRef>,
    ) -> bool {
        self.attachments
            .lock()
            .expect("attachments lock")
            .get(&selection.attachment_id)
            .is_some_and(|state| {
                state.revision == selection.revision
                    && state.selection.includes(plugin_id, hash, scope)
            })
    }
    pub(crate) fn check_selection(
        &self,
        selection: Option<composition_scope::SelectionRevision>,
        plugins: Option<&PluginRegistry>,
        plugin_id: &str,
        hash: &str,
        scope: Option<&EntryRef>,
    ) -> Result<(), String> {
        if scope.is_none() {
            return Ok(());
        }
        let selection = selection.ok_or("Native contribution has no caller selection")?;
        if plugins.and_then(PluginRegistry::caller_selection) != Some(selection)
            || !self.selection_current(selection, plugin_id, hash, scope)
        {
            return Err("Native contribution is no longer selected for this caller".into());
        }
        Ok(())
    }
}
/// Last-consumer guard for any tool executed under a caller-bound composition.
pub(crate) fn validate_caller_plugins(plugins: Option<&PluginRegistry>) -> Result<(), String> {
    let Some(revision) = plugins.and_then(PluginRegistry::caller_selection) else {
        return Ok(());
    };
    let manager = manager();
    let attachments = manager.shared.attachments.lock().expect("attachments lock");
    if attachments
        .get(&revision.attachment_id)
        .is_some_and(|state| state.revision == revision.revision)
    {
        Ok(())
    } else {
        Err("Native caller composition changed or was detached; prepare the call again".into())
    }
}
/// Raw catalog choices are revalidated against the reviewed staged bundle;
/// discovery never activates them. Generic Native entries retain active-scope
/// discovery. Child preparation revalidates before attaching the chosen entry.
pub(crate) fn native_presets_for_plugins(
    plugins: &PluginRegistry,
) -> Vec<(composition_scope::NativePresetRef, PluginAuthority)> {
    if !activation::extension_host_policy_enabled() {
        return Vec::new();
    }
    let manager = manager();
    let mut presets: Vec<_> = crate::plugins::native_presets::admitted_entries(plugins)
        .into_iter()
        .map(|(preset, _, authority)| (preset, authority))
        .collect();
    if manager.shared.ready_host(HostTier::Plugin).is_err() {
        return presets;
    }
    let registry = manager.shared.registry.lock().expect("registry lock");
    for owner in registry.owners() {
        let Some(authority) = owner.authority.as_ref() else {
            continue;
        };
        if owner.tier != HostTier::Plugin
            || owner.state != OwnerState::Active
            || authority.workspace != plugins.workspace()
            || !plugins.get(&owner.owner.plugin_id).is_some_and(|plugin| {
                plugin.component_active(PluginActivationCapability::Native)
                    && plugin.content_hash == owner.content_hash
                    && plugin.state_generation == authority.state_generation
            })
        {
            continue;
        }
        for (entry, state) in &owner.scopes {
            if *state == OwnerState::Active
                && !presets.iter().any(|(preset, _)| {
                    preset.plugin_id == owner.owner.plugin_id && preset.entry == *entry
                })
            {
                presets.push((
                    composition_scope::NativePresetRef {
                        plugin_id: owner.owner.plugin_id.clone(),
                        content_hash: owner.content_hash.clone(),
                        entry: entry.clone(),
                    },
                    authority.clone(),
                ));
            }
        }
    }
    presets.sort_by(|(left, _), (right, _)| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| left.entry.path.cmp(&right.entry.path))
    });
    presets
}

pub(crate) fn caller_view(
    workspace: &Path,
    session_id: Option<&str>,
    agent_id: Option<&str>,
) -> Option<Arc<PluginRegistry>> {
    let manager = manager();
    let attachments = manager.shared.attachments.lock().expect("attachments lock");
    let mut found = attachments.values().filter(|state| {
        state.plugins.workspace() == workspace
            && state.session_id.as_deref() == session_id
            && state.agent_id.as_deref() == agent_id
    });
    let view = found.next().map(|state| Arc::clone(&state.plugins));
    if found.next().is_some() { None } else { view }
}

impl Drop for HostAttachment {
    fn drop(&mut self) {
        self.manager.shared.core_calls.revoke_attachment(self.id);
        if let Ok(mut attachments) = self.manager.shared.attachments.lock()
            && let Some(state) = attachments.remove(&self.id)
        {
            state.selection_cancel.cancel();
        }
        command::bump_epoch();
    }
}

impl std::fmt::Debug for HostAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAttachment")
            .field("id", &self.id)
            .finish()
    }
}

static GLOBAL: OnceLock<Arc<ExtensionHostManager>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_MANAGER: std::cell::RefCell<Option<Arc<ExtensionHostManager>>> =
        const { std::cell::RefCell::new(None) };
}

/// Configure the process-wide manager once, at boot, from user config.
pub(crate) fn configure_with_handle(
    options: ExtensionHostOptions,
    handle: Option<tokio::runtime::Handle>,
) {
    let manager = Arc::new(ExtensionHostManager::new(options));
    if let Some(handle) = handle {
        manager.bind_engine_handle(handle);
    }
    let _ = GLOBAL.set(manager);
}

/// The manager for this engine process (one host per process and tier).
#[must_use]
pub fn manager() -> Arc<ExtensionHostManager> {
    #[cfg(test)]
    if let Some(manager) = TEST_MANAGER.with(|cell| cell.borrow().clone()) {
        return manager;
    }
    Arc::clone(
        GLOBAL.get_or_init(|| Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()))),
    )
}

/// Test-only: route [`manager`] on this thread to `manager`.
#[cfg(test)]
pub(crate) struct TestManagerGuard(Option<Arc<ExtensionHostManager>>);

#[cfg(test)]
impl TestManagerGuard {
    pub(crate) fn install(manager: Arc<ExtensionHostManager>) -> Self {
        Self(TEST_MANAGER.with(|cell| cell.replace(Some(manager))))
    }
}

#[cfg(test)]
impl Drop for TestManagerGuard {
    fn drop(&mut self) {
        TEST_MANAGER.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}

impl ExtensionHostManager {
    pub(crate) fn has_shell_hooks(&self, event: codewhale_config::hooks::HookEvent) -> bool {
        activation::extension_host_policy_enabled()
            && self
                .shared
                .registry
                .lock()
                .expect("registry lock")
                .live_shell_hooks()
                .iter()
                .any(|r| r.hook.event == event)
    }
    pub(crate) fn shell_hooks(
        &self,
        caller: &crate::hooks::HookCaller,
        event: codewhale_config::hooks::HookEvent,
    ) -> Vec<codewhale_config::hooks::Hook> {
        if !activation::extension_host_policy_enabled() {
            return Vec::new();
        }
        let Some(plugins) = caller.plugins.as_ref() else {
            return Vec::new();
        };
        let Some(revision) = plugins.caller_selection() else {
            return Vec::new();
        };
        let Some((current, agent)) = self
            .shared
            .plugins_for_selection(revision, caller.session_id.as_deref())
        else {
            return Vec::new();
        };
        if agent != caller.agent_id
            || !Arc::ptr_eq(&current, plugins)
            || caller.workspace != plugins.workspace()
        {
            return Vec::new();
        }
        self.shared
            .registry
            .lock()
            .expect("registry lock")
            .live_shell_hooks()
            .into_iter()
            .filter(|r| {
                r.hook.event == event
                    && plugins.selected_native_entries().iter().any(|entry| {
                        entry.plugin_id == r.owner.plugin_id
                            && entry.content_hash == r.content_hash
                            && r.scope.as_ref().is_none_or(|scope| entry.entry == *scope)
                    })
            })
            .map(|r| r.hook)
            .collect()
    }
}
